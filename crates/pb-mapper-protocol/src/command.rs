use serde::{Deserialize, Serialize};
use snafu::ResultExt;

use pb_mapper_auth::{
    AuthStatus, IssuedTemporaryKey, KeyPage, LegacyProtocolPolicy, TemporaryKeyMetadata,
};
use pb_mapper_core::checksum::AesKeyType;
use pb_mapper_core::error::{MsgSerializeSnafu, Result};

pub const CONTROL_PROTOCOL_V2: u16 = 2;

pub trait MessageSerializer {
    fn encode(&self) -> Result<Vec<u8>>;
    fn decode(msg: &[u8]) -> Result<Self>
    where
        Self: Sized;
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum PbConnStatusReq {
    RemoteId,
    Keys,
    Service { key: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PbConnStatusResp {
    RemoteId {
        server_map: String,
        active: String,
        idle: String,
    },
    Keys(Vec<String>),
    Service {
        key: String,
        connections: Vec<PbServiceConnStatus>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PbServiceConnStatus {
    pub conn_id: u32,
    pub generation: u64,
    pub protocol_version: u16,
    pub healthy: bool,
    pub last_rx_age_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum PbConnRequest {
    Register {
        need_codec: bool,
        is_datagram: bool,
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol_version: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_instance_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heartbeat_interval_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heartbeat_tolerance_ms: Option<u64>,
    },
    RegisterScoped {
        need_codec: bool,
        is_datagram: bool,
        key: String,
        namespace: u64,
        force_namespace: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol_version: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_instance_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heartbeat_interval_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heartbeat_tolerance_ms: Option<u64>,
    },
    Subcribe {
        key: String,
    },
    SubcribeScoped {
        key: String,
        namespace: u64,
    },
    Status(PbConnStatusReq),
    StatusScoped {
        status: PbConnStatusReq,
        namespace: u64,
    },
    Stream {
        key: String,
        dst_id: u32,
        #[serde(default)]
        server_generation: u64,
    },
    StreamScoped {
        key: String,
        namespace: u64,
        dst_id: u32,
        #[serde(default)]
        server_generation: u64,
    },
    Admin(AdminRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum AdminRequest {
    KeyIssue {
        ttl_seconds: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    KeyList {
        #[serde(default)]
        page: u32,
        #[serde(default = "default_page_size")]
        page_size: u16,
    },
    KeyShow {
        key_id: u64,
    },
    KeyReveal {
        key_id: u64,
    },
    KeyRenew {
        key_id: u64,
        ttl_seconds: u64,
    },
    KeyRevoke {
        key_id: u64,
    },
    KeyGc,
    AuthStatus,
    AuthStateReset {
        confirm: bool,
    },
    RootKeyRotate {
        new_admin_key: String,
    },
    LegacyProtocolSet {
        policy: LegacyProtocolPolicy,
    },
    ConnectionList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_id: Option<u64>,
        #[serde(default)]
        page: u32,
        #[serde(default = "default_page_size")]
        page_size: u16,
    },
    ServiceList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_id: Option<u64>,
        #[serde(default)]
        page: u32,
        #[serde(default = "default_page_size")]
        page_size: u16,
    },
    /// Drop registered control connections the relay is still holding.
    ///
    /// The manual counterpart to the relay's own lease sweep, for the case an
    /// operator can see but the relay cannot: a registration that answers its
    /// heartbeat yet no longer forwards, or a service whose connection quota is
    /// full of connections that should have gone away.
    ConnectionRetire {
        /// Namespace owning the service. Absent means the unscoped namespace,
        /// which is where an administrator's own registrations live.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_id: Option<u64>,
        service_name: String,
        /// Retire only this connection. Absent retires every connection the
        /// service has, which is what frees a full quota in one call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conn_id: Option<u32>,
    },
}

impl AdminRequest {
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::KeyIssue { .. }
                | Self::KeyRenew { .. }
                | Self::KeyRevoke { .. }
                | Self::KeyGc
                | Self::AuthStateReset { .. }
                | Self::RootKeyRotate { .. }
                | Self::LegacyProtocolSet { .. }
                | Self::ConnectionRetire { .. }
        )
    }
}

const fn default_page_size() -> u16 {
    100
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PbErrorResponse {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub server_time: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdminServiceInfo {
    pub key_id: u64,
    pub namespace: u64,
    pub service_name: String,
    pub transport: String,
    pub codec_enabled: bool,
    pub connection_count: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdminConnectionInfo {
    pub key_id: u64,
    pub namespace: u64,
    pub service_name: String,
    pub conn_id: u32,
    pub generation: u64,
    pub protocol_version: u16,
    pub healthy: bool,
    pub transport: String,
    pub codec_enabled: bool,
    pub last_rx_age_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdminServicePage {
    pub schema_version: u16,
    pub items: Vec<AdminServiceInfo>,
    pub next_page: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdminConnectionPage {
    pub schema_version: u16,
    pub items: Vec<AdminConnectionInfo>,
    pub next_page: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum AdminResponse {
    KeyIssued(IssuedTemporaryKey),
    KeyList(KeyPage),
    KeyShown(IssuedTemporaryKey),
    KeyRenewed(IssuedTemporaryKey),
    KeyRevoked(TemporaryKeyMetadata),
    KeyGc {
        removed: u64,
    },
    AuthStatus(AuthStatus),
    Services(AdminServicePage),
    Connections(AdminConnectionPage),
    /// How many registered connections `ConnectionRetire` actually dropped. Zero
    /// is a normal answer: the target may have unwound on its own first.
    ConnectionsRetired {
        retired: u32,
    },
    Ok {
        action: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PbConnResponse {
    Register(u32),
    RegisterV2 {
        conn_id: u32,
        generation: u64,
        lease_ttl_ms: u64,
    },
    Subcribe {
        codec_key: Option<AesKeyType>,
        client_id: u32,
        server_id: u32,
    },
    Stream {
        codec_key: Option<AesKeyType>,
    },
    Status(PbConnStatusResp),
    Admin(AdminResponse),
    Error(PbErrorResponse),
}

impl PbConnResponse {
    pub fn error(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self::Error(PbErrorResponse {
            code: code.into(),
            message: message.into(),
            retryable,
            server_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PbServerRequest {
    Ping,
    PingV2 {
        seq: u64,
    },
    StreamAck {
        client_id: u32,
        #[serde(default)]
        server_generation: u64,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum LocalServer {
    /// pb server makes a stream request to local server
    Stream {
        client_id: u32,
        #[serde(default)]
        server_generation: u64,
    },
    /// pb server response a pong msg when it receive a ping request
    Pong,
    PongV2 {
        seq: u64,
    },
    Retire {
        reason: String,
        conn_id: u32,
        #[serde(default)]
        server_generation: u64,
    },
}

macro_rules! gen_impl_msg_serializer {
    ($struct_name:ident) => {
        impl MessageSerializer for $struct_name {
            fn encode(&self) -> Result<Vec<u8>> {
                serde_json::to_vec(self).with_context(|_| MsgSerializeSnafu {
                    action: "encode",
                    struct_name: stringify!($struct_name),
                    content: "payload redacted".to_string(),
                })
            }

            fn decode(msg: &[u8]) -> Result<Self> {
                serde_json::from_slice(msg).with_context(|_| MsgSerializeSnafu {
                    action: "decode",
                    struct_name: stringify!($struct_name),
                    content: format!("{}-byte payload redacted", msg.len()),
                })
            }
        }
    };
}

gen_impl_msg_serializer!(PbConnRequest);
gen_impl_msg_serializer!(PbConnResponse);
gen_impl_msg_serializer!(PbServerRequest);
gen_impl_msg_serializer!(LocalServer);

#[cfg(test)]
mod tests {
    use super::PbConnRequest;

    /// The wire form of `Register` is load-bearing: a running peer on the other
    /// side of an upgrade has to keep parsing it. The `None` fields must stay
    /// absent from the JSON rather than serialise as null.
    #[test]
    fn test_serde_mapper_header() {
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
