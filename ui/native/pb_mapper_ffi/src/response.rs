//! Shared helpers for FFI response formatting and argument parsing.
//!
//! Every JSON-envelope export has one shape, which [`respond`] holds: the
//! null-handle check, then the call with the borrowed handle (its arguments
//! parsed, its work run, its change announced), then the envelope.

use std::borrow::Cow;
use std::ffi::{CStr, CString, c_char};
use std::ptr;

use pb_mapper_client::sdk::PortMapping;
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::CtlError;
use crate::handle::PbMapperHandle;

/// What a JSON-envelope export answers with when it succeeds.
pub(crate) enum Reply {
    /// `{"success": true, "message": …}`
    Message(Cow<'static, str>),
    /// `{"success": true, "data": …}`
    Data(Value),
}

/// Runs `call` with the handle, or answers that there is none, and turns
/// what it returns into the envelope. `handle` is what `as_ref` on the
/// export's raw pointer gives.
pub(crate) fn respond(
    handle: Option<&PbMapperHandle>,
    call: impl FnOnce(&PbMapperHandle) -> Result<Reply, CtlError>,
) -> *mut c_char {
    let Some(handle) = handle else {
        return err_null_handle();
    };
    match call(handle) {
        Ok(Reply::Message(message)) => ok_message(&message),
        Ok(Reply::Data(data)) => ok_data(data),
        Err(error) => err_ctl(&error),
    }
}

/// `value` as JSON, or `fallback` if it does not serialize.
pub(crate) fn json_or(value: impl Serialize, fallback: Value) -> Value {
    serde_json::to_value(value).unwrap_or(fallback)
}

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

/// Build an error response: the sentence for a person, the code for a script.
///
/// Every failure leaving this library goes through here, so `success: false`
/// always arrives with a `code` and a caller never has to match on prose.
pub(crate) fn err_ctl(error: &CtlError) -> *mut c_char {
    to_c_string(json!({
        "success": false,
        "message": error.to_string(),
        "code": error.code(),
    }))
}

/// The response for a call made with a handle that was never created, or was
/// already destroyed.
///
/// Its own function because every entry point taking a handle opens with the
/// same check, and all of them mean the same thing.
pub(crate) fn err_null_handle() -> *mut c_char {
    err_ctl(&CtlError::invalid_argument("handle is null"))
}

/// Build a success response with data payload.
pub(crate) fn ok_data(data: serde_json::Value) -> *mut c_char {
    to_c_string(json!({"success": true, "data": data}))
}

/// Parse a required C string argument.
pub(crate) fn parse_c_string(ptr: *const c_char, field: &str) -> Result<String, CtlError> {
    if ptr.is_null() {
        return Err(CtlError::invalid_argument(format!("{field} is null")));
    }
    unsafe {
        CStr::from_ptr(ptr)
            .to_str()
            .map(|s| s.to_string())
            .map_err(|_| CtlError::invalid_argument(format!("{field} is not valid UTF-8")))
    }
}

/// The port mapping a C caller names: 0 off, 1 when needed, 2 always.
pub fn port_mapping_from_c(raw: std::ffi::c_int) -> Result<PortMapping, CtlError> {
    match raw {
        0 => Ok(PortMapping::Off),
        1 => Ok(PortMapping::Auto),
        2 => Ok(PortMapping::Always),
        _ => Err(CtlError::invalid_argument(format!(
            "port_mapping must be 0, 1 or 2, got {raw}"
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn read_and_free(ptr: *mut c_char) -> serde_json::Value {
        assert!(!ptr.is_null(), "a response must serialise");
        let owned = unsafe { CString::from_raw(ptr) };
        serde_json::from_slice(owned.as_bytes()).expect("responses are JSON")
    }

    /// The contract the CLI will read: a failure always says what kind it is,
    /// so a script never has to match on the sentence.
    #[test]
    fn every_failure_carries_a_code() {
        let failed = read_and_free(err_ctl(&CtlError::not_found("no such service")));
        assert_eq!(failed["success"], false);
        assert_eq!(failed["message"], "no such service");
        assert_eq!(failed["code"], "NOT_FOUND");

        // Including the ones raised at the boundary, before any state is
        // touched: a call without a handle never runs.
        let null_handle = read_and_free(respond(None, |_| panic!("ran without a handle")));
        assert_eq!(null_handle["code"], "INVALID_ARGUMENT");

        let bad_arg = parse_c_string(std::ptr::null(), "service_key")
            .expect_err("a null pointer is not a string");
        assert_eq!(bad_arg.code(), crate::error::ErrorCode::InvalidArgument);
    }

    /// Success responses have no code to report, and must not grow one.
    #[test]
    fn success_responses_stay_as_they_were() {
        let ok = read_and_free(ok_message("service registration started"));
        assert_eq!(ok["success"], true);
        assert_eq!(ok["message"], "service registration started");
        assert!(ok.get("code").is_none(), "success carries no error code");
    }
}
