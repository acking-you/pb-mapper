//! Opt-in coordination messages. Legacy connections never emit these messages.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

pub type SessionId = [u8; 16];
pub fn random_session_id() -> SessionId {
    rand::random()
}

pub const DIRECT_VERSION: u16 = 1;
pub const MAX_CANDIDATES: usize = 8;
pub const MAX_CERT_LEN: usize = 2048;
pub const SETUP_TTL_MS: u64 = 8_000;

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct SessionSecret(pub [u8; 32]);

impl PartialEq for SessionSecret {
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;
        bool::from(self.0.ct_eq(&other.0))
    }
}
impl Eq for SessionSecret {}
impl SessionSecret {
    pub fn random() -> Self {
        Self(rand::random())
    }
}

impl std::fmt::Debug for SessionSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectCapability {
    pub version: u16,
    pub stun_addr: SocketAddr,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerInfo {
    pub certificate: Vec<u8>,
    pub candidates: Vec<SocketAddr>,
}

impl PeerInfo {
    pub fn valid(&self) -> bool {
        !self.certificate.is_empty()
            && self.certificate.len() <= MAX_CERT_LEN
            && self.candidates.len() <= MAX_CANDIDATES
            && self.candidates.iter().all(valid_candidate)
    }
}

pub fn valid_candidate(addr: &SocketAddr) -> bool {
    addr.port() != 0
        && !addr.ip().is_unspecified()
        && !addr.ip().is_multicast()
        && match addr {
            SocketAddr::V4(addr) => !addr.ip().is_broadcast() && !addr.ip().is_link_local(),
            SocketAddr::V6(addr) => !addr.ip().is_unicast_link_local(),
        }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionGrant {
    pub id: SessionId,
    pub server_instance: SessionId,
    pub generation: u64,
    pub namespace: u64,
    pub service: String,
    pub is_datagram: bool,
    pub consumer_key: u64,
    pub provider_key: u64,
    pub consumer: PeerInfo,
    pub provider: PeerInfo,
    pub secret: SessionSecret,
    pub ttl_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DirectClientMessage {
    CancelOpen {
        request: u64,
    },
    Open {
        request: u64,
        key: String,
        is_datagram: bool,
        peer: PeerInfo,
    },
    RelaySelect {
        id: SessionId,
    },
    Cancel {
        id: SessionId,
    },
    Report(SessionUpdate),
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DirectProviderMessage {
    Accept { id: SessionId, peer: PeerInfo },
    Reject { id: SessionId, reason: String },
    RelaySelected { id: SessionId },
    Report(SessionUpdate),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DirectEvent {
    Ready(DirectCapability),
    Offer {
        grant: SessionGrant,
        stun_addr: SocketAddr,
    },
    Granted {
        request: u64,
        grant: SessionGrant,
    },
    LegacyRelay {
        request: u64,
    },
    Rejected {
        request: u64,
        reason: String,
    },
    RelaySelect {
        id: SessionId,
    },
    RelaySelected {
        id: SessionId,
    },
    Cancel {
        id: SessionId,
    },
    Pong,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionPath {
    Direct,
    Relay,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Active,
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionReport {
    pub id: SessionId,
    pub seq: u64,
    pub path: SessionPath,
    pub state: SessionState,
    pub reason: Option<String>,
}

/// Metadata claimed by an authenticated endpoint. It never authorizes setup.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub server_instance: SessionId,
    pub id: SessionId,
    pub namespace: u64,
    pub service: String,
    pub is_datagram: bool,
    pub consumer_key: u64,
    pub provider_key: u64,
}
impl From<&SessionGrant> for SessionSummary {
    fn from(grant: &SessionGrant) -> Self {
        Self {
            server_instance: grant.server_instance,
            id: grant.id,
            namespace: grant.namespace,
            service: grant.service.clone(),
            is_datagram: grant.is_datagram,
            consumer_key: grant.consumer_key,
            provider_key: grant.provider_key,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionUpdate {
    pub session: SessionSummary,
    pub secret: SessionSecret,
    pub report: SessionReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservedReport {
    pub report: SessionReport,
    pub age_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionObservation {
    pub server_instance: SessionId,
    /// Restored endpoint claim after restart or retention expiry, without setup authority.
    pub restored: bool,
    pub id: SessionId,
    pub service: String,
    pub is_datagram: bool,
    pub consumer: Option<ObservedReport>,
    pub provider: Option<ObservedReport>,
    pub stale: bool,
    pub conflicting: bool,
}

macro_rules! wire_message {
    ($($name:ty),+ $(,)?) => {$ (
        impl crate::command::MessageSerializer for $name {
            fn encode(&self) -> pb_mapper_core::error::Result<Vec<u8>> {
                serde_json::to_vec(self).map_err(|e| crate::secure::protocol_error(e.to_string()))
            }
            fn decode(bytes: &[u8]) -> pb_mapper_core::error::Result<Self> {
                serde_json::from_slice(bytes).map_err(|e| crate::secure::protocol_error(e.to_string()))
            }
        }
    )+};
}
wire_message!(
    DirectClientMessage,
    DirectProviderMessage,
    DirectEvent,
    SessionGrant
);

#[cfg(test)]
mod tests {
    use crate::command::{
        MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
    };
    #[test]
    fn disabled_direct_fields_are_absent_from_existing_wire_messages() {
        let request = PbConnRequest::Register {
            direct: None,
            key: "legacy".into(),
            need_codec: false,
            is_datagram: false,
            protocol_version: None,
            client_instance_id: None,
            heartbeat_interval_ms: None,
            heartbeat_tolerance_ms: None,
        };
        let expected = br#"{"Register":{"need_codec":false,"is_datagram":false,"key":"legacy"}}"#;
        assert_eq!(request.encode().unwrap(), expected);
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
