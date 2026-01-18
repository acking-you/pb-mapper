//! Shared helpers for FFI response formatting and argument parsing.

use std::ffi::{c_char, CStr, CString};
use std::ptr;

use serde_json::json;

/// Convert a JSON value into an owned C string pointer.
///
/// Returns null if serialization or allocation fails.
pub(crate) fn to_c_string(value: serde_json::Value) -> *mut c_char {
    match serde_json::to_string(&value)
        .ok()
        .and_then(|s| CString::new(s).ok())
    {
        Some(cstring) => cstring.into_raw(),
        None => ptr::null_mut(),
    }
}

/// Build a success response with a message.
pub(crate) fn ok_message(message: &str) -> *mut c_char {
    to_c_string(json!({"success": true, "message": message}))
}

/// Build an error response with a message.
pub(crate) fn err_message(message: &str) -> *mut c_char {
    to_c_string(json!({"success": false, "message": message}))
}

/// Build a success response with data payload.
pub(crate) fn ok_data(data: serde_json::Value) -> *mut c_char {
    to_c_string(json!({"success": true, "data": data}))
}

/// Parse a required C string argument.
pub(crate) fn parse_c_string(ptr: *const c_char, field: &str) -> Result<String, String> {
    if ptr.is_null() {
        return Err(format!("{field} is null"));
    }
    unsafe {
        CStr::from_ptr(ptr)
            .to_str()
            .map(|s| s.to_string())
            .map_err(|_| format!("{field} is not valid UTF-8"))
    }
}
