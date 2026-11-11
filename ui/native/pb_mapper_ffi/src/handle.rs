//! FFI handle lifecycle and app directory configuration.

use std::ffi::{CStr, c_char};
use std::ptr;
use std::sync::Arc;

use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::CtlError;
use crate::response::{Reply, respond};
use crate::state::PbMapperState;

/// Opaque handle for the pb-mapper runtime and shared state.
pub struct PbMapperHandle {
    pub(crate) desktop: crate::desktop::DesktopState,
    /// The window's readers of tunnel events.
    pub(crate) tunnel_events: crate::tunnel_events::TunnelEventReaders,
    pub(crate) runtime: Runtime,
    pub(crate) state: Arc<Mutex<PbMapperState>>,
    /// Stops the control server when the handle goes away.
    pub(crate) control_shutdown: CancellationToken,
}

impl Drop for PbMapperHandle {
    fn drop(&mut self) {
        self.control_shutdown.cancel();
    }
}

/// Create a new pb-mapper handle, to be freed with `pb_mapper_destroy`; null
/// when no runtime could be started.
#[unsafe(no_mangle)]
pub extern "C" fn pb_mapper_create() -> *mut PbMapperHandle {
    let runtime = match Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return ptr::null_mut(),
    };

    // Nothing is read until `pb_mapper_set_app_dir` says from where.
    let state = PbMapperState::unloaded();
    let handle = PbMapperHandle {
        desktop: Default::default(),
        tunnel_events: Default::default(),
        runtime,
        state: Arc::new(Mutex::new(state)),
        control_shutdown: CancellationToken::new(),
    };

    Box::into_raw(Box::new(handle))
}

/// Start accepting commands from `pb_mapper_ui <verb>` in other processes.
///
/// Failure is not fatal and is reported as such: if the endpoint cannot be
/// claimed — most often because another UI already holds it — the app should
/// carry on as a window. Refusing to open because a pipe is unavailable would
/// be a poor trade.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_start_control_server(
    handle: *mut PbMapperHandle,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        // `start` spawns onto this runtime, so it has to be entered first.
        let _guard = handle.runtime.enter();
        let endpoint =
            crate::ctl::server::start(handle.state.clone(), handle.control_shutdown.clone())
                .map_err(|e| {
                    CtlError::io(format!(
                        "control server not started: {e}. The UI still works as a window."
                    ))
                })?;
        Ok(Reply::Message(
            format!("control server listening on {endpoint}").into(),
        ))
    })
}

/// Destroy handle and free resources.
///
/// # Safety
/// `handle` must be a valid pointer returned by `pb_mapper_create`. Every call
/// borrowing it, a pending tunnel event read included, must have returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_destroy(handle: *mut PbMapperHandle) {
    if !handle.is_null() {
        unsafe { drop(Box::from_raw(handle)) };
        crate::logging::pb_mapper_shutdown_logging();
    }
}

/// Set the app directory and load the configuration in it; empty or null
/// means the platform's default directory, and so does a path that is not
/// UTF-8. A new handle has read nothing, so every client calls this first.
///
/// # Safety
/// `handle` must be null or come from `pb_mapper_create` and not yet be
/// destroyed, and `path` null or a NUL-terminated string that stays valid
/// for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_set_app_dir(
    handle: *mut PbMapperHandle,
    path: *const c_char,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        let path = if path.is_null() {
            None
        } else {
            // SAFETY: non-null, and NUL-terminated as the caller guarantees.
            match unsafe { CStr::from_ptr(path) }.to_str() {
                Ok(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            }
        };
        // Configure before loading the saved state so configuration errors
        // and all subsequent native diagnostics reach the same directory as
        // its files.
        crate::disk_log::configure(&PbMapperState::get_config_dir(&path));
        handle
            .runtime
            .block_on(async { handle.state.lock().await.set_app_directory_path(path) })?;
        Ok(Reply::Message("app directory updated".into()))
    })
}
