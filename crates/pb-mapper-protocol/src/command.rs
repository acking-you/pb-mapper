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
    Service {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direct_query: Option<bool>,
    },
    Sessions,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PbConnStatusResp {
    Sessions(Vec<crate::direct::SessionObservation>),
    RemoteId {
        server_map: String,
        active: String,
        idle: String,
    },
    Keys(Vec<String>),
    Service {
        key: String,
        connections: Vec<PbServiceConnStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direct: Option<crate::direct::DirectCapability>,
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
    /// Authenticated discovery; available to administrator and temporary credentials.
    QuicEndpoint,
    DirectControl {
        namespace: Option<u64>,
    },
    DirectRelay {
        id: crate::direct::SessionId,
        secret: crate::direct::SessionSecret,
        namespace: Option<u64>,
        data_protocol: Option<u16>,
    },
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direct: Option<u16>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direct: Option<u16>,
    },
    Subcribe {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_protocol: Option<u16>,
    },
    SubcribeScoped {
        key: String,
        namespace: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_protocol: Option<u16>,
    },
    Status(PbConnStatusReq),
    StatusScoped {
        status: PbConnStatusReq,
        namespace: u64,
    },
    Stream {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_protocol: Option<u16>,
        dst_id: u32,
        #[serde(default)]
        server_generation: u64,
    },
    StreamScoped {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_protocol: Option<u16>,
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
    /// Reserve a namespace that no key owns.
    NamespaceAllocate,
    /// [`Self::KeyIssue`] into a namespace from [`Self::NamespaceAllocate`].
    KeyIssueScoped {
        namespace: u64,
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
    /// Whether the request changes relay state, and so must pass the durable
    /// administrator-mutation replay check. Every variant is named: a new one
    /// does not compile until someone decides.
    pub fn is_mutating(&self) -> bool {
        match self {
            Self::KeyIssue { .. }
            | Self::NamespaceAllocate
            | Self::KeyIssueScoped { .. }
            | Self::KeyRenew { .. }
            | Self::KeyRevoke { .. }
            | Self::KeyGc
            | Self::AuthStateReset { .. }
            | Self::RootKeyRotate { .. }
            | Self::LegacyProtocolSet { .. }
            | Self::ConnectionRetire { .. } => true,
            Self::KeyList { .. }
            | Self::KeyShow { .. }
            | Self::KeyReveal { .. }
            | Self::AuthStatus
            | Self::ConnectionList { .. }
            | Self::ServiceList { .. } => false,
        }
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

/// The [`PbErrorResponse::code`] a relay answers an authenticated request it
/// cannot decode with, which is also how a relay that predates a request kind
/// answers that kind.
pub const REQUEST_DECODE_FAILED: &str = "request_decode_failed";

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
    NamespaceAllocated {
        namespace: u64,
    },
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
    /// A change acknowledged by name: `action` is one of the `ACTION_*`
    /// constants.
    Ok {
        action: String,
    },
}

/// The [`AdminResponse::Ok`] action after the auth state was reset.
pub const ACTION_AUTH_STATE_RESET: &str = "auth_state_reset";
/// The [`AdminResponse::Ok`] action after the root key was rotated.
pub const ACTION_ADMINISTRATOR_KEY_ROTATED: &str = "administrator_key_rotated";
/// The [`AdminResponse::Ok`] action after the legacy-protocol policy was set.
pub const ACTION_LEGACY_PROTOCOL_UPDATED: &str = "legacy_protocol_updated";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PbConnResponse {
    QuicEndpoint(Option<QuicEndpoint>),
    Direct(crate::direct::DirectEvent),
    Register(u32),
    RegisterV2 {
        conn_id: u32,
        generation: u64,
        lease_ttl_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direct: Option<crate::direct::DirectCapability>,
    },
    Subcribe {
        codec_key: Option<AesKeyType>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_protocol: Option<u16>,
        client_id: u32,
        server_id: u32,
    },
    Stream {
        codec_key: Option<AesKeyType>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_protocol: Option<u16>,
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
            server_time: pb_mapper_core::time::unix_seconds(),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PbServerRequest {
    Direct(crate::direct::DirectProviderMessage),
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
    Direct(Box<crate::direct::DirectEvent>),
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

/// Only a UDP port is advertised; use the IP of the authenticated TCP peer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct QuicEndpoint {
    pub port: u16,
    pub certificate: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire form a running peer on the other side of an upgrade has to
    /// keep parsing: optional fields left `None` stay absent from the JSON
    /// rather than serialise as null.
    #[test]
    fn unset_optional_fields_are_absent_from_the_wire() {
        let register = PbConnRequest::Register {
            direct: None,
            key: "legacy".into(),
            need_codec: false,
            is_datagram: false,
            protocol_version: None,
            client_instance_id: None,
            heartbeat_interval_ms: None,
            heartbeat_tolerance_ms: None,
        };
        let wire = br#"{"Register":{"need_codec":false,"is_datagram":false,"key":"legacy"}}"#;
        assert_eq!(register.encode().unwrap(), wire);
        assert_eq!(PbConnRequest::decode(wire).unwrap(), register);
        assert_eq!(
            serde_json::to_vec(&PbConnStatusReq::Service {
                key: "legacy".into(),
                direct_query: None
            })
            .unwrap(),
            br#"{"Service":{"key":"legacy"}}"#
        );
        let response = PbConnResponse::RegisterV2 {
            conn_id: 1,
            generation: 2,
            lease_ttl_ms: 3000,
            direct: None,
        };
        assert_eq!(
            response.encode().unwrap(),
            br#"{"RegisterV2":{"conn_id":1,"generation":2,"lease_ttl_ms":3000}}"#
        );
        let response = PbConnStatusResp::Service {
            key: "legacy".into(),
            connections: vec![],
            direct: None,
        };
        assert_eq!(
            serde_json::to_vec(&response).unwrap(),
            br#"{"Service":{"key":"legacy","connections":[]}}"#
        );
    }
}
