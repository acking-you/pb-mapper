use pb_mapper_auth::LegacyProtocolPolicy;
use pb_mapper_protocol::command::{PbConnStatusResp, PbServiceConnStatus};

/// Transport used by a registered service or a local subscriber.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    Tcp,
    Udp,
}

impl Transport {
    pub(crate) fn is_datagram(self) -> bool {
        matches!(self, Self::Udp)
    }
}

/// Observed lifecycle of a [`super::Registration`] or [`super::Connection`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TunnelStatus {
    Starting,
    Connected,
    Retrying,
    Stopped,
    Failed(String),
}

impl TunnelStatus {
    pub(crate) fn from_callback(status: &str) -> Self {
        match status {
            "connected" => Self::Connected,
            "retrying" => Self::Retrying,
            "failed" => Self::Failed("failed".into()),
            _ => Self::Retrying,
        }
    }
}

/// Relay mapping dump from `status remote-id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteId {
    pub server_map: String,
    pub active: String,
    pub idle: String,
}

impl RemoteId {
    pub(crate) fn from_status(status: PbConnStatusResp) -> super::Result<Self> {
        match status {
            PbConnStatusResp::RemoteId {
                server_map,
                active,
                idle,
            } => Ok(Self {
                server_map,
                active,
                idle,
            }),
            other => Err(super::Error::protocol(format!(
                "expected remote-id status, got {other:?}"
            ))),
        }
    }
}

/// One control connection the relay is holding for a service key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceConnection {
    pub conn_id: u32,
    pub generation: u64,
    pub protocol_version: u16,
    pub healthy: bool,
    pub last_rx_age_ms: u64,
}

impl From<PbServiceConnStatus> for ServiceConnection {
    fn from(value: PbServiceConnStatus) -> Self {
        Self {
            conn_id: value.conn_id,
            generation: value.generation,
            protocol_version: value.protocol_version,
            healthy: value.healthy,
            last_rx_age_ms: value.last_rx_age_ms,
        }
    }
}

/// Legacy framing policy, matching `pb-mapper admin legacy-protocol`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacyProtocol {
    Allow,
    Deny,
}

impl From<LegacyProtocol> for LegacyProtocolPolicy {
    fn from(value: LegacyProtocol) -> Self {
        match value {
            LegacyProtocol::Allow => Self::Allow,
            LegacyProtocol::Deny => Self::Deny,
        }
    }
}

impl From<LegacyProtocolPolicy> for LegacyProtocol {
    fn from(value: LegacyProtocolPolicy) -> Self {
        match value {
            LegacyProtocolPolicy::Allow => Self::Allow,
            LegacyProtocolPolicy::Deny => Self::Deny,
        }
    }
}
