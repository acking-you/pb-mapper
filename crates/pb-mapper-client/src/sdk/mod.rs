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

pub use admin::{
    Admin, AuthStatusInfo, ConnectionInfo, ConnectionPage, IssuedKey, KeyListPage, KeyMetadata,
    ServiceInfo, ServicePage,
};
pub use client::{Client, ClientConfig, ConnectRequest, RegisterRequest, RelayMetricsStream};
pub use error::{Error, Result};
pub use handle::{Connection, Registration};
pub(crate) use types::DirectPolicy;
pub use types::{
    ConnectOptions, LanTransport, LegacyProtocol, PathPolicy, PortMapping, RegisterOptions,
    RemoteId, ServiceConnection, Transport, TunnelStatus,
};

/// The SDK version compiled into this process.
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");
pub use crate::diagnostics::{
    AddressFamily, ByteCount, CandidateCounts, DirectAttempt, DirectLink, DirectStage,
    EventsLagged, FallbackReason, InterfaceTier, LanCounts, LanEvidence, LinkKind, LinkSample,
    LinkStats, LinkVia, PathDiagnostics, PathTraffic, PortMappingDiagnostics, PortMappingProtocol,
    PortMappingStage, PortMappingStatus, RecoveryFailure, RecoveryPhase, RelayHealth,
    SessionCounts, SetupTimings, StunOutcome, TunnelDiagnostics, TunnelEvent, TunnelEventKind,
    TunnelEvents, TunnelMetrics, TunnelMetricsStream,
};
pub use pb_mapper_protocol::direct::{
    LinkSummary, ObservedReport, SessionObservation, SessionPath, SessionState,
};

pub use crate::relay::{
    QUIC_UNAVAILABLE, QuicConnectionDiagnostics, RelayDiagnostics, RelayTransport,
};
