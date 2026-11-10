//! Client connection and status FFI entrypoints.

use std::ffi::{c_char, c_int};

use serde_json::json;

use crate::ctl::Origin;
use crate::events;
use crate::handle::PbMapperHandle;
use crate::response::{Reply, json_or, parse_c_string, port_mapping_from_c, respond};
use crate::state::DirectOptions;

/// Connect client to service. `namespace` is the temporary credential whose
/// service it is, for the administrator key; negative for this credential's
/// own namespace. A nonzero `allow_direct` has each session try a direct
/// path before the relay; zero keeps them all on the relay. `port_mapping`
/// says when to ask the router to map the direct UDP port, while direct is
/// allowed: 0 never, 1 once punching through fails, 2 from the start.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and each string argument null or a NUL-terminated string
/// that stays valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_connect_service(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
    local_address: *const c_char,
    protocol: *const c_char,
    enable_keep_alive: c_int,
    namespace: i64,
    allow_direct: c_int,
    port_mapping: c_int,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        let service_key = parse_c_string(service_key, "service_key")?;
        let local_address = parse_c_string(local_address, "local_address")?;
        let protocol = parse_c_string(protocol, "protocol")?;
        let port_mapping = port_mapping_from_c(port_mapping)?;
        // See `pb_mapper_register_service`: the connect path releases the
        // state lock for its slow phase, so the caller must not be holding
        // it. Saving the config is `connect_service`'s job, not this
        // layer's — that is what makes a connection from the window and one
        // from a terminal end up in the same state.
        handle.runtime.block_on(crate::state::connect_service(
            &handle.state,
            service_key,
            local_address,
            protocol,
            enable_keep_alive != 0,
            u64::try_from(namespace).ok(),
            DirectOptions {
                allow_direct: allow_direct != 0,
                port_mapping,
            },
        ))?;
        events::emit(events::ChangeKind::Clients, None, Origin::Ui);
        Ok(Reply::Message("client connection started".into()))
    })
}

/// Disconnect client from service.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and each string argument null or a NUL-terminated string
/// that stays valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_disconnect_service(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        let service_key = parse_c_string(service_key, "service_key")?;
        handle
            .runtime
            .block_on(async { handle.state.lock().await.disconnect_service(service_key) })?;
        events::emit(events::ChangeKind::Clients, None, Origin::Ui);
        Ok(Reply::Message("client disconnected".into()))
    })
}

/// Delete client config (also stops client if running).
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and each string argument null or a NUL-terminated string
/// that stays valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_delete_client_config(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        let service_key = parse_c_string(service_key, "service_key")?;
        handle.runtime.block_on(async {
            handle
                .state
                .lock()
                .await
                .delete_client_config_and_stop(service_key)
        })?;
        events::emit(events::ChangeKind::Clients, None, Origin::Ui);
        Ok(Reply::Message("client config deleted".into()))
    })
}

/// Get client configs list.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_get_client_configs_json(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        let clients = handle
            .runtime
            .block_on(async { handle.state.lock().await.get_client_configs() });
        Ok(Reply::Data(json!({"clients": clients})))
    })
}

/// Get client status for a specific key.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and each string argument null or a NUL-terminated string
/// that stays valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_get_client_status_json(
    handle: *mut PbMapperHandle,
    service_key: *const c_char,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        let service_key = parse_c_string(service_key, "service_key")?;
        let status = handle
            .runtime
            .block_on(async { handle.state.lock().await.get_client_status(service_key) });
        Ok(Reply::Data(json_or(status, json!({}))))
    })
}
