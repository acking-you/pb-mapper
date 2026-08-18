//! App-level configuration FFI entrypoints.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::ctl::Origin;
use crate::events;
use crate::handle::PbMapperHandle;
use crate::response::{err_ctl, err_null_handle, ok_data, ok_message, parse_c_string};

/// Get current app config.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_get_config_json(handle: *mut PbMapperHandle) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let (config, isolated_admin_key) = handle.runtime.block_on(async move {
        let state = state.lock().await;
        (state.get_config_status().await, state.isolated_admin_key())
    });

    ok_data(json!({
        "serverAddress": config.server_address,
        "keepAliveEnabled": config.keep_alive_enabled,
        "msgHeaderKey": config.msg_header_key,
        "isolatedRelayAdminKey": isolated_admin_key.unwrap_or_default(),
    }))
}

/// Update app config.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_update_config(
    handle: *mut PbMapperHandle,
    server_address: *const c_char,
    enable_keep_alive: c_int,
    msg_header_key: *const c_char,
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

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        state
            .update_config(server_address, enable_keep_alive != 0, msg_header_key)
            .await
    });

    match result {
        Ok(_) => {
            events::emit(events::ChangeKind::Config, None, Origin::Ui);
            ok_message("configuration saved")
        }
        Err(e) => err_ctl(&e),
    }
}
