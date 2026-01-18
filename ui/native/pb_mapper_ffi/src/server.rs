//! Local and remote server management FFI entrypoints.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::handle::PbMapperHandle;
use crate::response::{err_message, ok_data, ok_message};

/// Start pb-mapper server.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_start_server(
    handle: *mut PbMapperHandle,
    port: u16,
    enable_keep_alive: c_int,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        state.start_server(port, enable_keep_alive != 0).await
    });

    match result {
        Ok(_) => ok_message("server started"),
        Err(e) => err_message(&e),
    }
}

/// Stop pb-mapper server.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_stop_server(handle: *mut PbMapperHandle) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        state.stop_server().await
    });

    match result {
        Ok(_) => ok_message("server stopped"),
        Err(e) => err_message(&e),
    }
}

/// Get local server status (running/uptime).
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_get_local_server_status_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let status = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_local_server_status().await
    });

    ok_data(serde_json::to_value(status).unwrap_or_else(|_| json!({})))
}

/// Get server status detail (remote server).
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_get_server_status_detail_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_server_status_detail().await
    });

    match result {
        Ok(detail) => ok_data(serde_json::to_value(detail).unwrap_or_else(|_| json!({}))),
        Err(e) => err_message(&e),
    }
}
