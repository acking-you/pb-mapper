//! App-level configuration FFI entrypoints.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int};

use pb_mapper_client::sdk::RelayTransport;
use pb_mapper_core::checksum::get_process_credential;
use pb_mapper_core::config::resolve_pb_mapper_server_async;
use serde_json::json;

use crate::ctl::Origin;
use crate::error::CtlError;
use crate::events;
use crate::handle::PbMapperHandle;
use crate::response::{err_ctl, err_null_handle, ok_data, parse_c_string};

/// Get current app config.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_get_config_json(handle: *mut PbMapperHandle) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let (config, isolated_admin_key_set) = handle.runtime.block_on(async move {
        let state = state.lock().await;
        (
            state.get_config_status().await,
            state.isolated_admin_key().is_some(),
        )
    });

    ok_data(json!({
        "serverAddress": config.server_address,
        "keepAliveEnabled": config.keep_alive_enabled,
        "msgHeaderKey": config.msg_header_key,
        "relayTransport": config.relay_transport,
        "isolatedRelayAdminKeySet": isolated_admin_key_set,
    }))
}

/// Reveal the embedded relay administrator key. This is a separate call so
/// routine config fetches cannot leak the root secret.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_reveal_isolated_admin_key(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let key = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.isolated_admin_key()
    });

    ok_data(json!({
        "isolatedRelayAdminKey": key.unwrap_or_default(),
    }))
}

/// Update app config. `relay_transport` is `"tcp"` or `"quic"`.
///
/// When the carrier changes, `data.move` names this computer's tunnels on
/// the saved relay, under the saved credential, still on the old carrier:
/// they keep it until started again, which is the window's to offer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_update_config(
    handle: *mut PbMapperHandle,
    server_address: *const c_char,
    enable_keep_alive: c_int,
    msg_header_key: *const c_char,
    relay_transport: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let server_address = match parse_c_string(server_address, "server_address") {
        Ok(v) => v,
        Err(e) => return err_ctl(&e),
    };
    let msg_header_key = match parse_c_string(msg_header_key, "msg_header_key") {
        Ok(v) => v,
        Err(e) => return err_ctl(&e),
    };
    let relay_transport = match parse_c_string(relay_transport, "relay_transport")
        .and_then(|raw| parse_transport(&raw))
    {
        Ok(v) => v,
        Err(e) => return err_ctl(&e),
    };

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        // Where that relay is, to know its tunnels by address as well as by
        // name: resolved before the state is locked, since it can be slow,
        // and only when the carrier looks about to change. `None` is not
        // looked up; `Some(None)` did not resolve.
        let current = state.lock().await.get_config_status().await.relay_transport;
        let looked_up = if current == relay_transport {
            None
        } else {
            Some(
                resolve_pb_mapper_server_async(Some(&server_address))
                    .await
                    .ok(),
            )
        };
        // Compared, written, and the tunnels to move named, under one lock:
        // two saves at once cannot both see the old carrier, or both miss
        // the change, and the tunnels named are the ones on another carrier
        // than the one this save wrote.
        let mut state = state.lock().await;
        let changed = state.get_config_status().await.relay_transport != relay_transport;
        state
            .update_config(
                server_address.clone(),
                enable_keep_alive != 0,
                msg_header_key,
                relay_transport,
            )
            .await?;
        let credential = match get_process_credential() {
            Ok(credential) if changed => credential,
            _ => return Ok(Default::default()),
        };
        let endpoint = match looked_up {
            Some(endpoint) => endpoint,
            // Another save changed the carrier between that look and this
            // lock, so the change was not foreseen: looked up now, with the
            // lock held, rather than naming tunnels by address text alone.
            None => resolve_pb_mapper_server_async(Some(&server_address))
                .await
                .ok(),
        };
        let moving = state.on_other_carrier(
            &server_address,
            endpoint.as_ref(),
            credential,
            relay_transport,
        );
        Ok::<_, CtlError>(moving)
    });

    match result {
        Ok(moving) => {
            events::emit(events::ChangeKind::Config, None, Origin::Ui);
            ok_data(json!({
                "move": {
                    "shares": moving.shares,
                    "connections": moving.connections,
                },
            }))
        }
        Err(e) => err_ctl(&e),
    }
}

/// The carrier `raw` names.
pub(crate) fn parse_transport(raw: &str) -> Result<RelayTransport, CtlError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "tcp" => Ok(RelayTransport::Tcp),
        "quic" => Ok(RelayTransport::Quic),
        other => Err(CtlError::invalid_argument(format!(
            "`{other}` is not a relay carrier; use tcp or quic"
        ))),
    }
}
