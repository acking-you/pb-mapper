//! Local and remote server management FFI entrypoints.
//!
//! The remote relay's questions are not asked under the state lock: each
//! takes it only for the settings and client it asks with, so a relay that
//! is slow or silent holds up no other call. See `state::status`.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::ctl::Origin;
use crate::events;
use crate::handle::PbMapperHandle;
use crate::response::{err_ctl, err_null_handle, ok_data, ok_message, parse_c_string};

/// Start pb-mapper server. A nonzero `quic_port` also takes QUIC on that
/// UDP port; zero offers none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_start_server(
    handle: *mut PbMapperHandle,
    port: u16,
    enable_keep_alive: c_int,
    quic_port: u16,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let handle = unsafe { &*handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        state
            .start_server(
                port,
                enable_keep_alive != 0,
                (quic_port != 0).then_some(quic_port),
            )
            .await
    });

    match result {
        Ok(_) => {
            events::emit(events::ChangeKind::Server, None, Origin::Ui);
            ok_message("server started")
        }
        Err(e) => err_ctl(&e),
    }
}

/// Stop pb-mapper server.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_stop_server(handle: *mut PbMapperHandle) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let handle = unsafe { &*handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        state.stop_server().await
    });

    match result {
        Ok(_) => {
            events::emit(events::ChangeKind::Server, None, Origin::Ui);
            ok_message("server stopped")
        }
        Err(e) => err_ctl(&e),
    }
}

/// Get local server status (running/uptime).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_get_local_server_status_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let handle = unsafe { &*handle };
    let state = handle.state.clone();
    let status = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_local_server_status()
    });

    ok_data(serde_json::to_value(status).unwrap_or_else(|_| json!({})))
}

/// The connections the remote server holds for one service key.
///
/// The status detail's `serverMap` is a Debug dump of the whole map and is not
/// something a UI should be parsing. This answers the same question with the
/// protocol's own structured query, one key at a time.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_get_service_conns_json(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let service_key = match parse_c_string(service_key, "service_key") {
        Ok(v) => v,
        Err(e) => return err_ctl(&e),
    };

    let handle = unsafe { &*handle };
    let result = handle
        .runtime
        .block_on(crate::state::get_service_conns(&handle.state, service_key));

    match result {
        Ok(conns) => ok_data(serde_json::to_value(conns).unwrap_or_else(|_| json!([]))),
        Err(e) => err_ctl(&e),
    }
}

/// Get server status detail (remote server).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_get_server_status_detail_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let handle = unsafe { &*handle };
    let result = handle
        .runtime
        .block_on(crate::state::get_server_status_detail(&handle.state));

    match result {
        Ok(detail) => ok_data(serde_json::to_value(detail).unwrap_or_else(|_| json!({}))),
        Err(e) => err_ctl(&e),
    }
}

/// Force-refresh server status (blocks until network result is available).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_force_refresh_server_status_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let handle = unsafe { &*handle };
    let result = handle
        .runtime
        .block_on(crate::state::force_refresh_server_status(&handle.state));

    match result {
        Ok(detail) => ok_data(serde_json::to_value(detail).unwrap_or_else(|_| json!({}))),
        Err(e) => err_ctl(&e),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::ffi::CStr;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use pb_mapper_client::sdk::RelayTransport;
    use pb_mapper_core::checksum::set_process_msg_header_key;
    use pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK;
    use serde_json::Value;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::pb_mapper_get_config_json;
    use crate::state::PbMapperState;

    fn reply(text: *mut c_char) -> Value {
        let value = serde_json::from_slice(unsafe { CStr::from_ptr(text) }.to_bytes()).unwrap();
        unsafe { crate::logging::pb_mapper_free_string(text) };
        value
    }

    /// The window checks the relay every few seconds, each call on a worker
    /// of its own. A relay that takes the connection and never answers keeps
    /// that check waiting out its timeout; the window's other calls, here
    /// reading the settings, must not wait with it.
    #[test]
    fn a_relay_that_never_answers_holds_up_no_other_call() {
        // Building a state and saving a credential write the process
        // credential; see `state::tests::temp_state`.
        let _process_credential_guard = PROCESS_CREDENTIAL_TEST_LOCK.blocking_lock();
        let root = std::env::temp_dir().join(format!(
            "pb-mapper-ffi-test-silent-relay-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let handle = PbMapperHandle {
            desktop: Default::default(),
            tunnel_events: Default::default(),
            state: Arc::new(tokio::sync::Mutex::new(PbMapperState::new(Some(
                root.display().to_string(),
            )))),
            control_shutdown: Default::default(),
            runtime: tokio::runtime::Runtime::new().unwrap(),
        };
        let ptr = || std::ptr::from_ref(&handle).cast_mut();

        // Takes every connection and answers none, until it hangs up.
        let relay = handle
            .runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let address = relay.local_addr().unwrap().to_string();
        let hang_up = CancellationToken::new();
        let (dialled, dials) = mpsc::channel();
        handle.runtime.spawn({
            let hang_up = hang_up.clone();
            async move {
                let mut held = Vec::new();
                loop {
                    tokio::select! {
                        () = hang_up.cancelled() => return,
                        accepted = relay.accept() => if let Ok((socket, _)) = accepted {
                            held.push(socket);
                            let _ = dialled.send(());
                        },
                    }
                }
            }
        });
        handle
            .runtime
            .block_on(async {
                handle
                    .state
                    .lock()
                    .await
                    .update_config(crate::state::AppConfig {
                        server_address: address.clone(),
                        keep_alive_enabled: false,
                        msg_header_key: pb_mapper_testkit::TEST_ADMIN_KEY.into(),
                        relay_transport: RelayTransport::Tcp,
                        lan_first: true,
                        ..Default::default()
                    })
            })
            .unwrap();

        std::thread::scope(|scope| {
            let checking =
                scope.spawn(|| reply(unsafe { pb_mapper_get_server_status_detail_json(ptr()) }));
            dials
                .recv_timeout(Duration::from_secs(5))
                .expect("the check dials the relay");
            let (read, settings) = mpsc::channel();
            scope.spawn(move || read.send(reply(unsafe { pb_mapper_get_config_json(ptr()) })));
            // Well inside the check's own timeout, which is seconds long.
            let config = settings.recv_timeout(Duration::from_secs(1));
            let overlapped = !checking.is_finished();
            // Either way the relay hangs up now, so the check ends at once
            // rather than at its timeout.
            hang_up.cancel();
            let config = config.expect("reading the settings waited for the relay check");
            assert!(overlapped, "the check was still waiting on the relay");
            assert_eq!(
                config["data"]["serverAddress"],
                address.as_str(),
                "{config}"
            );
            let detail = checking.join().unwrap();
            assert_eq!(detail["data"]["serverAvailable"], false, "{detail}");
        });

        let _ = set_process_msg_header_key(None);
        drop(handle);
        let _ = std::fs::remove_dir_all(&root);
    }
}
