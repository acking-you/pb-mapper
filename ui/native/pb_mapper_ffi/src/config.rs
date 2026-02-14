//! App-level configuration FFI entrypoints.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::handle::PbMapperHandle;
use crate::response::{err_message, ok_data, ok_message, parse_c_string};
use crate::state::AppConfig;

/// Get current app config.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_get_config_json(handle: *mut PbMapperHandle) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let config: AppConfig = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_config_status().await
    });

    ok_data(json!({
        "serverAddress": config.server_address,
        "keepAliveEnabled": config.keep_alive_enabled,
        "msgHeaderKey": config.msg_header_key
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
        return err_message("handle is null");
    }

    let server_address = match parse_c_string(server_address, "server_address") {
        Ok(v) => v,
        Err(e) => return err_message(&e),
    };
    let msg_header_key = match parse_c_string(msg_header_key, "msg_header_key") {
        Ok(v) => v,
        Err(e) => return err_message(&e),
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
        Ok(_) => ok_message("configuration saved"),
        Err(e) => err_message(&e),
    }
}
