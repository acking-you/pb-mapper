//! Local and remote server management FFI entrypoints.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::ctl::Origin;
use crate::events;
use crate::handle::PbMapperHandle;
use crate::response::{err_ctl, err_null_handle, ok_data, ok_message, parse_c_string};

/// Start pb-mapper server.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_start_server(
    handle: *mut PbMapperHandle,
    port: u16,
    enable_keep_alive: c_int,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        state.start_server(port, enable_keep_alive != 0).await
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

    let handle = unsafe { &mut *handle };
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

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let status = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_local_server_status().await
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

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_service_conns(service_key).await
    });

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

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_server_status_detail().await
    });

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

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.force_refresh_server_status().await
    });

    match result {
        Ok(detail) => ok_data(serde_json::to_value(detail).unwrap_or_else(|_| json!({}))),
        Err(e) => err_ctl(&e),
    }
}
