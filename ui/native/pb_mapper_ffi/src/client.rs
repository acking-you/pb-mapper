//! Client connection and status FFI entrypoints.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::handle::PbMapperHandle;
use crate::response::{err_message, ok_data, ok_message, parse_c_string};
use crate::state::{ClientConfigInfo, ClientStatusResponse};

/// Connect client to service.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_connect_service(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
    local_address: *const c_char,
    protocol: *const c_char,
    enable_keep_alive: c_int,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let service_key = match parse_c_string(service_key, "service_key") {
        Ok(v) => v,
        Err(e) => return err_message(&e),
    };
    let local_address = match parse_c_string(local_address, "local_address") {
        Ok(v) => v,
        Err(e) => return err_message(&e),
    };
    let protocol = match parse_c_string(protocol, "protocol") {
        Ok(v) => v,
        Err(e) => return err_message(&e),
    };

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        let connect_result = state
            .connect_service(
                service_key.clone(),
                local_address.clone(),
                protocol.clone(),
                enable_keep_alive != 0,
            )
            .await;

        match connect_result {
            Ok(_) => {
                if let Err(e) = state.save_client_config(
                    &service_key,
                    &local_address,
                    &protocol,
                    enable_keep_alive != 0,
                ) {
                    tracing::warn!("Failed to save client config: {}", e);
                    return Ok(Some(format!(
                        "Client started, but failed to save config: {e}"
                    )));
                }
                Ok(None)
            }
            Err(e) => Err(e),
        }
    });

    match result {
        Ok(Some(warning)) => ok_message(&warning),
        Ok(None) => ok_message("client connection started"),
        Err(e) => err_message(&e),
    }
}

/// Disconnect client from service.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_disconnect_service(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let service_key = match parse_c_string(service_key, "service_key") {
        Ok(v) => v,
        Err(e) => return err_message(&e),
    };

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        state.disconnect_service(service_key).await
    });

    match result {
        Ok(_) => ok_message("client disconnected"),
        Err(e) => err_message(&e),
    }
}

/// Delete client config (also stops client if running).
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_delete_client_config(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let service_key = match parse_c_string(service_key, "service_key") {
        Ok(v) => v,
        Err(e) => return err_message(&e),
    };

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        state.delete_client_config_and_stop(service_key).await
    });

    match result {
        Ok(_) => ok_message("client config deleted"),
        Err(e) => err_message(&e),
    }
}

/// Get client configs list.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_get_client_configs_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let clients: Vec<ClientConfigInfo> = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_client_configs().await
    });

    ok_data(json!({"clients": clients}))
}

/// Get client status for a specific key.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_get_client_status_json(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let service_key = match parse_c_string(service_key, "service_key") {
        Ok(v) => v,
        Err(e) => return err_message(&e),
    };

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let status: ClientStatusResponse = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_client_status(service_key).await
    });

    ok_data(serde_json::to_value(status).unwrap_or_else(|_| json!({})))
}
