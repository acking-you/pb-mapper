#[allow(async_fn_in_trait)]
pub mod common;
pub mod local;
pub mod pb_server;
pub mod utils;

// The `snafu_error_*` macros moved to `pb-mapper-core` with the error type.
// `#[macro_export]` puts them at that crate's root, so re-export them here to
// keep `crate::snafu_error_handle!` working while the split is in progress.
pub use pb_mapper_core::{
    snafu_error_get_or_continue, snafu_error_get_or_return, snafu_error_get_or_return_ok,
    snafu_error_handle,
};

// This was missing `#[cfg(test)]`, so it compiled into every release build.
#[cfg(test)]
mod tests {

    #[test]
    fn test_serde_mapper_header() {
        use crate::common::message::command::PbConnRequest;
        let mapper = PbConnRequest::Register {
            key: "test".into(),
            need_codec: false,
            is_datagram: false,
            protocol_version: None,
            client_instance_id: None,
            heartbeat_interval_ms: None,
            heartbeat_tolerance_ms: None,
        };
        let json_value = serde_json::to_string(&mapper).unwrap();
        let raw_json_str =
            r##"{"Register":{"need_codec":false,"is_datagram":false,"key":"test"}}"##;
        assert_eq!(raw_json_str, json_value);

        let value: PbConnRequest = serde_json::from_str(raw_json_str).unwrap();
        assert_eq!(mapper, value)
    }
}
