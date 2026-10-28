use serde::Serialize;
use std::net::SocketAddr;

/// A gateway mapping operation or lease transition, independent of tunnel readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMappingStage {
    WaitingForDirect,
    RelayOnly,
    Discovering,
    GatewayFound,
    Discovered,
    Requesting,
    Active,
    Renewing,
    Expired,
    Retrying,
    NetworkChanged,
    Releasing,
    Released,
    ReleaseSent,
    Failed,
    ReleaseFailed,
    Cancelled,
    Stopped,
}

impl PortMappingStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForDirect => "waiting_for_direct",
            Self::RelayOnly => "relay_only",
            Self::Discovering => "discovering",
            Self::GatewayFound => "gateway_found",
            Self::Discovered => "discovered",
            Self::Requesting => "requesting",
            Self::Active => "active",
            Self::Renewing => "renewing",
            Self::Expired => "expired",
            Self::Retrying => "retrying",
            Self::NetworkChanged => "network_changed",
            Self::Releasing => "releasing",
            Self::Released => "released",
            Self::ReleaseSent => "release_sent",
            Self::Failed => "failed",
            Self::ReleaseFailed => "release_failed",
            Self::Cancelled => "cancelled",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMappingProtocol {
    Pcp,
    NatPmp,
    Upnp,
}
impl PortMappingProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pcp => "pcp",
            Self::NatPmp => "nat_pmp",
            Self::Upnp => "upnp",
        }
    }
}

/// A credential-free observation. `active` says whether a lease remains usable,
/// including while renewal is pending or failed. Error details are bounded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortMappingEvent {
    pub stage: PortMappingStage,
    pub network_generation: u64,
    pub local_port: Option<u16>,
    pub gateway: Option<std::net::Ipv4Addr>,
    pub protocol: Option<PortMappingProtocol>,
    pub available_protocols: Vec<PortMappingProtocol>,
    pub external_addr: Option<SocketAddr>,
    pub active: bool,
    pub lease_lifetime_ms: Option<u64>,
    pub retry_in_ms: Option<u64>,
    pub error: Option<String>,
}

impl PortMappingEvent {
    pub(crate) fn new(stage: PortMappingStage, network_generation: u64) -> Self {
        Self {
            stage,
            network_generation,
            local_port: None,
            gateway: None,
            protocol: None,
            available_protocols: Vec::new(),
            external_addr: None,
            active: false,
            lease_lifetime_ms: None,
            retry_in_ms: None,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PortMappingDiagnostics {
    pub current: PortMappingEvent,
    pub attempts: u64,
    pub renewals: u64,
    pub failures: u64,
}
