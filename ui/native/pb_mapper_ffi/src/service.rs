//! Service registration and status FFI entrypoints.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::handle::PbMapperHandle;
use crate::response::{err_message, ok_data, ok_message, parse_c_string};
use crate::state::{ServiceConfigInfo, ServiceStatusResponse};

/// Register a service.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_register_service(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
    local_address: *const c_char,
    protocol: *const c_char,
    enable_encryption: c_int,
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
        state
            .register_service(
                service_key,
                local_address,
                protocol,
                enable_encryption != 0,
                enable_keep_alive != 0,
            )
            .await
    });

    match result {
        Ok(_) => ok_message("service registration started"),
        Err(e) => err_message(&e),
    }
}

/// Unregister a service (stop running but keep config).
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_unregister_service(
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
        state.unregister_service(service_key).await
    });

    match result {
        Ok(_) => ok_message("service unregistered"),
        Err(e) => err_message(&e),
    }
}

/// Delete service config (also stops service if running).
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_delete_service_config(
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
        state.delete_service_config_and_stop(service_key).await
    });

    match result {
        Ok(_) => ok_message("service config deleted"),
        Err(e) => err_message(&e),
    }
}

/// Get service configs list.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_get_service_configs_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let services: Vec<ServiceConfigInfo> = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_service_configs().await
    });

    ok_data(json!({"services": services}))
}

/// Get service status for a specific key.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_get_service_status_json(
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
    let status: ServiceStatusResponse = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_service_status(service_key).await
    });

    ok_data(serde_json::to_value(status).unwrap_or_else(|_| json!({})))
}
