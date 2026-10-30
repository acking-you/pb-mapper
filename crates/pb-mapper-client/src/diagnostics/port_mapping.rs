//! What a tunnel's gateway port mapping is doing, as [`TunnelDiagnostics`]
//! and [`TunnelEventKind::PortMappingChanged`] report it.
//!
//! [`TunnelDiagnostics`]: super::TunnelDiagnostics
//! [`TunnelEventKind::PortMappingChanged`]: super::TunnelEventKind::PortMappingChanged

use serde::Serialize;
use std::net::{Ipv4Addr, SocketAddr};

/// Where a gateway mapping stands. A lease outlives some stages: whether one
/// is usable right now is [`PortMappingStatus::active`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMappingStage {
    /// Asked for when needed (`PortMapping::Auto`): starts if a direct
    /// attempt fails to punch through, which none has.
    Standby,
    /// Requested; starts with the tunnel's first direct session, when its UDP
    /// endpoint opens.
    WaitingForDirect,
    /// Looking for the default gateway and the protocols it speaks.
    Discovering,
    /// Found the gateway.
    GatewayFound,
    /// Learned which of PCP, NAT-PMP and UPnP the gateway speaks.
    Discovered,
    /// Asking the gateway for a mapping.
    Requesting,
    /// The gateway granted a lease.
    Active,
    /// Extending the lease, which stays usable meanwhile.
    Renewing,
    /// The lease ran out before it was renewed.
    Expired,
    /// No lease yet; asking again, 30 s after the first time and then less
    /// often, up to every 10 min.
    Retrying,
    /// The local network changed; the gateway is looked for again.
    NetworkChanged,
    /// Asking the gateway to delete the lease.
    Releasing,
    /// The gateway confirmed the deletion (UPnP).
    Released,
    /// The deletion was sent; PCP and NAT-PMP do not acknowledge it.
    ReleaseSent,
    /// Discovery or a request failed; see [`PortMappingStatus::error`].
    Failed,
    /// The lease could not be deleted and may stay until it expires.
    ReleaseFailed,
    /// An operation was abandoned before it finished.
    Cancelled,
    /// The mapping stopped with its tunnel.
    Stopped,
}

impl PortMappingStage {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standby => "standby",
            Self::WaitingForDirect => "waiting_for_direct",
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

/// How a gateway is asked for a mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMappingProtocol {
    Pcp,
    NatPmp,
    Upnp,
}

impl PortMappingProtocol {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pcp => "pcp",
            Self::NatPmp => "nat_pmp",
            Self::Upnp => "upnp",
        }
    }
}

/// A gateway mapping as it stands after its latest change. Credential-free;
/// what the gateway said about a failure is cut to 256 characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortMappingStatus {
    pub stage: PortMappingStage,
    /// The local network this mapping belongs to; a change starts over.
    pub network_generation: u64,
    /// The direct endpoint's UDP port, once it is open.
    pub local_port: Option<u16>,
    pub gateway: Option<Ipv4Addr>,
    /// The protocol of the latest request or lease.
    pub protocol: Option<PortMappingProtocol>,
    /// What the gateway speaks, once discovery finished.
    pub available_protocols: Vec<PortMappingProtocol>,
    /// The public address the latest lease mapped to.
    pub external_addr: Option<SocketAddr>,
    /// Whether a lease is usable now: still true while a renewal is pending
    /// or has failed, until the lease expires or is released.
    pub active: bool,
    pub lease_lifetime_ms: Option<u64>,
    pub error: Option<String>,
}

impl PortMappingStatus {
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
            error: None,
        }
    }
}

/// A tunnel's port mapping: where it stands, and what it took to get there.
#[derive(Clone, Debug, Serialize)]
pub struct PortMappingDiagnostics {
    pub current: PortMappingStatus,
    /// Requests for a new lease.
    pub attempts: u64,
    pub renewals: u64,
    /// Failed discoveries, requests and releases.
    pub failures: u64,
}
