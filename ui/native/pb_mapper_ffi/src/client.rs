//! Client connection and status FFI entrypoints.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::ctl::Origin;
use crate::events;
use crate::handle::PbMapperHandle;
use crate::response::{err_ctl, err_null_handle, ok_data, ok_message, parse_c_string};
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
        return err_null_handle();
    }

    let service_key = match parse_c_string(service_key, "service_key") {
        Ok(v) => v,
        Err(e) => return err_ctl(&e),
    };
    let local_address = match parse_c_string(local_address, "local_address") {
        Ok(v) => v,
        Err(e) => return err_ctl(&e),
    };
    let protocol = match parse_c_string(protocol, "protocol") {
        Ok(v) => v,
        Err(e) => return err_ctl(&e),
    };

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    // See `pb_mapper_register_service`: the connect path releases the state lock
    // for its slow phase, so the caller must not be holding it.
    // Saving the config is `connect_service`'s job, not this layer's — that is
    // what makes a connection from the window and one from a terminal end up
    // in the same state.
    let result = handle.runtime.block_on(async move {
        crate::state::connect_service(
            &state,
            service_key,
            local_address,
            protocol,
            enable_keep_alive != 0,
        )
        .await
    });

    match result {
        Ok(_) => {
            events::emit(events::ChangeKind::Clients, None, Origin::Ui);
            ok_message("client connection started")
        }
        Err(e) => err_ctl(&e),
    }
}

/// Disconnect client from service.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_disconnect_service(
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
        let mut state = state.lock().await;
        state.disconnect_service(service_key).await
    });

    match result {
        Ok(_) => {
            events::emit(events::ChangeKind::Clients, None, Origin::Ui);
            ok_message("client disconnected")
        }
        Err(e) => err_ctl(&e),
    }
}

/// Delete client config (also stops client if running).
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_delete_client_config(
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
        let mut state = state.lock().await;
        state.delete_client_config_and_stop(service_key).await
    });

    match result {
        Ok(_) => {
            events::emit(events::ChangeKind::Clients, None, Origin::Ui);
            ok_message("client config deleted")
        }
        Err(e) => err_ctl(&e),
    }
}

/// Get client configs list.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_get_client_configs_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
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
        return err_null_handle();
    }

    let service_key = match parse_c_string(service_key, "service_key") {
        Ok(v) => v,
        Err(e) => return err_ctl(&e),
    };

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let status: ClientStatusResponse = handle.runtime.block_on(async move {
        let state = state.lock().await;
        state.get_client_status(service_key).await
    });

    ok_data(serde_json::to_value(status).unwrap_or_else(|_| json!({})))
}
