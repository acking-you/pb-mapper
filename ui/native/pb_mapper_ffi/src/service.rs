//! Service registration and status FFI entrypoints.

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::ctl::Origin;
use crate::events;
use crate::handle::PbMapperHandle;
use crate::response::{Reply, json_or, parse_c_string, port_mapping_from_c, respond};
use crate::state::{DirectOptions, ShareSpec};

/// Register a service. A nonzero `allow_direct` lets connections reach it
/// directly rather than through the relay; `port_mapping` then says when to
/// ask the router to map the direct UDP port: 0 never, 1 once punching
/// through fails, 2 from the start.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and each string argument null or a NUL-terminated string
/// that stays valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_register_service(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
    local_address: *const c_char,
    protocol: *const c_char,
    enable_encryption: c_int,
    enable_keep_alive: c_int,
    allow_direct: c_int,
    port_mapping: c_int,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        // SAFETY: null or NUL-terminated, as the caller guarantees.
        let service_key = unsafe { parse_c_string(service_key, "service_key") }?;
        let local_address = unsafe { parse_c_string(local_address, "local_address") }?;
        let protocol = unsafe { parse_c_string(protocol, "protocol") }?;
        let port_mapping = port_mapping_from_c(port_mapping)?;
        // Not `state.lock().await` around the call: registration releases
        // the lock for its slow middle phase, which it can only do if the
        // caller does not hold it. See `state::register_service`.
        handle.runtime.block_on(crate::state::register_service(
            &handle.state,
            ShareSpec {
                service_key,
                local_address,
                protocol,
                enable_encryption: enable_encryption != 0,
                enable_keep_alive: enable_keep_alive != 0,
                direct: DirectOptions {
                    allow_direct: allow_direct != 0,
                    port_mapping,
                },
            },
        ))?;
        events::emit(events::ChangeKind::Services, None, Origin::Ui);
        Ok(Reply::Message("service registration started".into()))
    })
}

/// Unregister a service (stop running but keep config).
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and each string argument null or a NUL-terminated string
/// that stays valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_unregister_service(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        // SAFETY: null or NUL-terminated, as the caller guarantees.
        let service_key = unsafe { parse_c_string(service_key, "service_key") }?;
        handle
            .runtime
            .block_on(async { handle.state.lock().await.unregister_service(service_key) })?;
        events::emit(events::ChangeKind::Services, None, Origin::Ui);
        Ok(Reply::Message("service unregistered".into()))
    })
}

/// Delete service config (also stops service if running).
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and each string argument null or a NUL-terminated string
/// that stays valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_delete_service_config(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        // SAFETY: null or NUL-terminated, as the caller guarantees.
        let service_key = unsafe { parse_c_string(service_key, "service_key") }?;
        handle.runtime.block_on(async {
            handle
                .state
                .lock()
                .await
                .delete_service_config_and_stop(service_key)
        })?;
        events::emit(events::ChangeKind::Services, None, Origin::Ui);
        Ok(Reply::Message("service config deleted".into()))
    })
}

/// Get service configs list.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_get_service_configs_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        let services = handle
            .runtime
            .block_on(async { handle.state.lock().await.get_service_configs() });
        Ok(Reply::Data(json!({"services": services})))
    })
}

/// Get service status for a specific key.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and each string argument null or a NUL-terminated string
/// that stays valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_get_service_status_json(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        // SAFETY: null or NUL-terminated, as the caller guarantees.
        let service_key = unsafe { parse_c_string(service_key, "service_key") }?;
        let status = handle
            .runtime
            .block_on(async { handle.state.lock().await.get_service_status(service_key) });
        Ok(Reply::Data(json_or(status, json!({}))))
    })
}
