//! App-level configuration FFI entrypoints.

use std::ffi::{c_char, c_int};

use pb_mapper_client::sdk::{LanTransport, RelayTransport};
use pb_mapper_core::checksum::get_process_credential;
use pb_mapper_core::config::resolve_pb_mapper_server_async;
use serde_json::json;

use crate::ctl::Origin;
use crate::error::CtlError;
use crate::events;
use crate::handle::PbMapperHandle;
use crate::response::{Reply, parse_c_string, respond};
use crate::state::{AppConfig, RelayTunnels};

/// Get current app config.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_get_config_json(handle: *mut PbMapperHandle) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        let (config, isolated_admin_key_set) = handle.runtime.block_on(async {
            let state = handle.state.lock().await;
            (
                state.get_config_status(),
                state.isolated_admin_key().is_some(),
            )
        });
        Ok(Reply::Data(json!({
            "serverAddress": config.server_address,
            "keepAliveEnabled": config.keep_alive_enabled,
            "msgHeaderKey": config.msg_header_key,
            "relayTransport": config.relay_transport,
            "lanFirst": config.lan_first,
            "lanTransport": config.lan_transport,
            "isolatedRelayAdminKeySet": isolated_admin_key_set,
        })))
    })
}

/// Reveal the embedded relay administrator key. This is a separate call so
/// routine config fetches cannot leak the root secret.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_reveal_isolated_admin_key(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        let key = handle
            .runtime
            .block_on(async { handle.state.lock().await.isolated_admin_key() });
        Ok(Reply::Data(json!({
            "isolatedRelayAdminKey": key.unwrap_or_default(),
        })))
    })
}

/// Update app config. `relay_transport` is `"tcp"` or `"quic"`; `lan_first`
/// nonzero tries a local network shared with the other end first, and
/// `lan_transport`, `"quic"` or `"tcp"`, is what such a network carries for
/// what this computer dials, from the next tunnel or remote-desktop start.
/// A change of `lan_first` or `lan_transport` alone keeps what the window
/// knows about the relay.
///
/// A running tunnel keeps the carrier, LAN-first and LAN transport it
/// started with until it is started again, which is the window's to offer.
/// When the carrier changes, `data.move` names this computer's tunnels on
/// the saved relay, under the saved credential, still on the old carrier;
/// when `lan_first` changes, `data.lanFirst` names those there that may go
/// direct and started with the other setting; when `lan_transport` changes,
/// `data.lanTransport` names the connections there that may go direct and
/// started with the other one. A tunnel on another relay or credential is
/// in none: a start would move it onto these. Each is empty when its setting
/// did not change.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and each string argument null or a NUL-terminated string
/// that stays valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_update_config(
    handle: *mut PbMapperHandle,
    server_address: *const c_char,
    enable_keep_alive: c_int,
    msg_header_key: *const c_char,
    relay_transport: *const c_char,
    lan_first: c_int,
    lan_transport: *const c_char,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        // SAFETY: null or NUL-terminated, as the caller guarantees.
        let server_address = unsafe { parse_c_string(server_address, "server_address") }?;
        let msg_header_key = unsafe { parse_c_string(msg_header_key, "msg_header_key") }?;
        let relay_transport = unsafe { parse_c_string(relay_transport, "relay_transport") }
            .and_then(|raw| parse_transport(&raw))?;
        let lan_first = lan_first != 0;
        // SAFETY: null or NUL-terminated, as the caller guarantees.
        let lan_transport = unsafe { parse_c_string(lan_transport, "lan_transport") }
            .and_then(|raw| parse_lan_transport(&raw))?;
        let state = &handle.state;
        let (moving, reached, carried) = handle.runtime.block_on(async move {
            // Where that relay is, to know its tunnels by address as well as by
            // name: resolved before the state is locked, since it can be slow,
            // and only when the carrier, LAN-first or the LAN transport looks
            // about to change. `None` is not looked up; `Some(None)` did not
            // resolve.
            let current = state.lock().await.get_config_status();
            let looked_up = if current.relay_transport == relay_transport
                && current.lan_first == lan_first
                && current.lan_transport == lan_transport
            {
                None
            } else {
                Some(
                    resolve_pb_mapper_server_async(Some(&server_address))
                        .await
                        .ok(),
                )
            };
            // Compared, written, and the tunnels each change reaches named,
            // under one lock: two saves at once cannot both see the old
            // setting, or both miss the change, and the tunnels named are the
            // ones started otherwise than this save wrote.
            let mut state = state.lock().await;
            let saved = state.get_config_status();
            let carrier_changed = saved.relay_transport != relay_transport;
            let lan_first_changed = saved.lan_first != lan_first;
            let lan_transport_changed = saved.lan_transport != lan_transport;
            state.update_config(AppConfig {
                server_address: server_address.clone(),
                keep_alive_enabled: enable_keep_alive != 0,
                msg_header_key,
                relay_transport,
                lan_first,
                lan_transport,
            })?;
            let credential = match get_process_credential() {
                Ok(credential) if carrier_changed || lan_first_changed || lan_transport_changed => {
                    credential
                }
                _ => return Ok(Default::default()),
            };
            let endpoint = match looked_up {
                Some(endpoint) => endpoint,
                // Another save changed one of them between that look and this
                // lock, so the change was not foreseen: looked up now, with the
                // lock held, rather than naming tunnels by address text alone.
                None => resolve_pb_mapper_server_async(Some(&server_address))
                    .await
                    .ok(),
            };
            let endpoint = endpoint.as_ref();
            let moving = if carrier_changed {
                state.on_other_carrier(&server_address, endpoint, credential, relay_transport)
            } else {
                RelayTunnels::default()
            };
            let reached = if lan_first_changed {
                state.on_other_lan_first(&server_address, endpoint, credential, lan_first)
            } else {
                RelayTunnels::default()
            };
            let carried = if lan_transport_changed {
                state.on_other_lan_transport(&server_address, endpoint, credential, lan_transport)
            } else {
                RelayTunnels::default()
            };
            Ok::<_, CtlError>((moving, reached, carried))
        })?;
        events::emit(events::ChangeKind::Config, None, Origin::Ui);
        Ok(Reply::Data(json!({
            "move": named(moving),
            "lanFirst": named(reached),
            "lanTransport": named(carried),
        })))
    })
}

/// Tunnels to start again, as the window reads them.
fn named(tunnels: RelayTunnels) -> serde_json::Value {
    json!({
        "shares": tunnels.shares,
        "connections": tunnels.connections,
    })
}

/// The LAN transport `raw` names: QUIC unless it says TCP.
fn parse_lan_transport(raw: &str) -> Result<LanTransport, CtlError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "quic" => Ok(LanTransport::Quic),
        "tcp" => Ok(LanTransport::Tcp),
        other => Err(CtlError::invalid_argument(format!(
            "`{other}` is not a LAN transport; use quic or tcp"
        ))),
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
