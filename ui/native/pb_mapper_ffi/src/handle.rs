//! FFI handle lifecycle and app directory configuration.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, CStr};
use std::ptr;
use std::sync::Arc;

use tokio::runtime::Runtime;
use tokio::sync::Mutex;

use crate::response::{err_message, ok_message};
use crate::state::PbMapperState;

/// Opaque handle for the pb-mapper runtime and shared state.
pub struct PbMapperHandle {
    pub(crate) runtime: Runtime,
    pub(crate) state: Arc<Mutex<PbMapperState>>,
}

/// Create a new pb-mapper handle.
///
/// # Safety
/// Returns a pointer that must be freed with `pb_mapper_destroy`.
#[no_mangle]
pub extern "C" fn pb_mapper_create() -> *mut PbMapperHandle {
    let runtime = match Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return ptr::null_mut(),
    };

    let state = PbMapperState::new(None);
    let handle = PbMapperHandle {
        runtime,
        state: Arc::new(Mutex::new(state)),
    };

    Box::into_raw(Box::new(handle))
}

/// Destroy handle and free resources.
///
/// # Safety
/// `handle` must be a valid pointer returned by `pb_mapper_create`.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_destroy(handle: *mut PbMapperHandle) {
    if !handle.is_null() {
        unsafe { drop(Box::from_raw(handle)) };
    }
}

/// Set app directory path (mobile only). Empty or null resets to default.
///
/// # Safety
/// `handle` must be valid. `path` must be valid C string or null.
#[no_mangle]
pub unsafe extern "C" fn pb_mapper_set_app_dir(
    handle: *mut PbMapperHandle,
    path: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return err_message("handle is null");
    }

    let path_opt = if path.is_null() {
        None
    } else {
        match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(s) if !s.is_empty() => Some(s.to_string()),
            _ => None,
        }
    };

    let handle = unsafe { &mut *handle };
    let state = handle.state.clone();
    let result = handle.runtime.block_on(async move {
        let mut state = state.lock().await;
        state.set_app_directory_path(path_opt)
    });

    match result {
        Ok(_) => ok_message("app directory updated"),
        Err(e) => err_message(&e),
    }
}
