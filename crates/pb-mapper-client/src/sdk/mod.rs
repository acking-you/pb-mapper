//! Library API for talking to a deployed pb-mapper relay.
//!
//! A [`Client`] holds the relay address and a credential. From there the same
//! process can register local services, subscribe to them, inspect tenant
//! status, and — with an administrator credential — drive every admin RPC.

mod admin;
mod client;
mod error;
mod handle;
mod types;
mod worker;

pub use admin::{
    Admin, AuthStatusInfo, ConnectionInfo, ConnectionPage, IssuedKey, KeyListPage, KeyMetadata,
    ServiceInfo, ServicePage,
};
pub use client::{Client, RelayMetricsStream};
pub use error::{Error, Result};
pub use handle::{Connection, Registration};
pub use types::{
    ClientConfig, ConnectOptions, ConnectRequest, LanTransport, LegacyProtocol, PathPolicy,
    PortMapping, RegisterOptions, RegisterRequest, RemoteId, ServiceConnection, Transport,
    TunnelStatus, UnknownLabel,
};

/// The SDK version compiled into this process.
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");
pub use crate::diagnostics::{
    AddressFamily, ByteCount, CandidateCounts, CongestionControl, DirectAttempt, DirectLink,
    DirectStage, EventsLagged, FallbackReason, InterfaceTier, LanCounts, LanEvidence, LanTcpMiss,
    LinkKind, LinkSample, LinkStats, LinkVia, PathDiagnostics, PathTraffic, PortMappingDiagnostics,
    PortMappingProtocol, PortMappingStage, PortMappingStatus, RecoveryFailure, RecoveryPhase,
    RelayHealth, SessionCounts, SetupTimings, StunOutcome, TunnelDiagnostics, TunnelEvent,
    TunnelEventKind, TunnelEvents, TunnelMetrics, TunnelMetricsStream,
};
pub use pb_mapper_protocol::direct::{
    DirectTransport, LinkSummary, ObservedReport, SessionObservation, SessionPath, SessionState,
};

pub use crate::relay::{
    QUIC_UNAVAILABLE, QuicConnectionDiagnostics, RelayDiagnostics, RelayTransport,
};

/// The transport every direct QUIC connection runs with, sending under
/// `congestion`: for benchmarks and tests that build their own endpoints
/// around it. Not part of the supported API.
#[doc(hidden)]
#[cfg(feature = "p2p")]
pub fn direct_transport_config(
    congestion: CongestionControl,
) -> std::io::Result<quinn::TransportConfig> {
    crate::direct::transport(congestion)
}
