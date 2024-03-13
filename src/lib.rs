pub mod common;
pub mod local;
pub mod pb_server;
pub mod utils;

mod tests {

    #[test]
    fn test_serde_mapper_header() {
        use crate::common::message::PbConnRequest;
        let mapper = PbConnRequest::Register { key: "test".into() };
        let json_value = serde_json::to_string(&mapper).unwrap();
        let raw_json_str = r##"{"Register":{"key":"test"}}"##;
        assert_eq!(raw_json_str, json_value);

        let value: PbConnRequest = serde_json::from_str(raw_json_str).unwrap();
        assert_eq!(mapper, value)
    }
}
