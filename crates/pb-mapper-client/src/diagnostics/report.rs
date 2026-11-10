//! The point-in-time views of a tunnel: [`TunnelDiagnostics`], and the
//! [`PathDiagnostics`] and [`DirectAttempt`] inside it.

use super::{
    DirectLink, DirectStage, LinkKind, PathTraffic, PortMappingDiagnostics, RecoveryFailure,
    RecoveryPhase, TunnelEvent,
};
use serde::Serialize;

/// The direct attempt a tunnel is making right now.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct DirectAttempt {
    /// The stage most recently entered by any attempt in flight.
    pub stage: DirectStage,
    /// Time since that attempt began.
    pub elapsed_ms: u64,
    /// Attempts in flight, this one included.
    pub in_flight: u32,
}

/// A point-in-time view of this tunnel and its process-shared relay budget.
/// Non-exhaustive: fields are added as the SDK observes more.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct TunnelDiagnostics {
    /// The carrier this tunnel reaches the relay over, fixed when it started.
    pub relay_transport: crate::relay::RelayTransport,
    /// Whether its direct setups try a shared local network first: its
    /// lan_first option, and never for a tunnel without direct.
    pub lan_first: bool,
    /// What its direct setups ask a shared local network to carry: its
    /// lan_transport option, and QUIC for a tunnel without direct and for
    /// a publisher, which serves what each consumer asks for.
    pub lan_transport: crate::sdk::LanTransport,
    /// How its sessions travel, on the relay and direct alike: present once
    /// one has opened or a direct attempt has begun.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct: Option<PathDiagnostics>,
    /// Its application bytes on each path; see [`TunnelMetrics::traffic`].
    ///
    /// [`TunnelMetrics::traffic`]: super::TunnelMetrics::traffic
    pub traffic: PathTraffic,
    /// The smoothed round trip of its control exchanges with the relay; see
    /// [`RelayHealth::rtt_ms`].
    ///
    /// [`RelayHealth::rtt_ms`]: super::RelayHealth::rtt_ms
    pub relay_rtt_ms: Option<u64>,
    /// Present when the tunnel maps its direct endpoint's port: asked for,
    /// with direct allowed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port_mapping: Option<PortMappingDiagnostics>,
    pub sdk_version: &'static str,
    /// Most recent worker phase; use tunnel status for aggregate readiness.
    pub last_attempt_phase: RecoveryPhase,
    pub attempts: u64,
    pub consecutive_failures: u64,
    pub last_failure: Option<RecoveryFailure>,
    /// Time since the last authenticated relay reply, even a negative reply.
    pub last_success_age_ms: Option<u64>,
    /// Last answered setup latency, excluding admission queue time.
    pub last_setup_latency_ms: Option<u64>,
    pub next_retry_in_ms: Option<u64>,
    pub dns_age_ms: Option<u64>,
    pub network_generation: u64,
    /// Shared across this process for the same configured relay address.
    pub active_control_setups: usize,
    pub active_data_setups: usize,
    /// The latest outcomes, oldest first; at most 16 entries. Steps along the
    /// way are only in [`TunnelEvents`]; see [`TunnelEventKind`].
    ///
    /// [`TunnelEvents`]: super::TunnelEvents
    /// [`TunnelEventKind`]: super::TunnelEventKind
    pub events: Vec<TunnelEvent>,
}

/// How a tunnel's sessions travel. Both ends count every session, on the
/// relay as well as direct. Non-exhaustive: fields are added as the SDK
/// observes more.
#[derive(Clone, Debug, Default, Serialize)]
#[non_exhaustive]
pub struct PathDiagnostics {
    pub direct_sessions: u64,
    pub relay_sessions: u64,
    pub active_direct: u64,
    pub active_relay: u64,
    /// Direct sessions within one local network, open now and in all.
    pub active_lan: u64,
    pub lan_sessions: u64,
    /// Direct sessions over TCP, which only a local network carries, open
    /// now and in all.
    pub active_lan_tcp: u64,
    pub lan_tcp_sessions: u64,
    /// Setups that found the peer on a local network of this end, and those
    /// of them that ended on another path or on the relay.
    pub lan_detected: u64,
    pub lan_missed: u64,
    pub fallbacks: u64,
    /// Why the latest fallback happened: [`FallbackReason::as_str`].
    ///
    /// [`FallbackReason::as_str`]: super::FallbackReason::as_str
    pub last_fallback: Option<String>,
    /// Where the latest direct session this end set up runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_kind: Option<LinkKind>,
    /// The direct attempt in flight, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<DirectAttempt>,
    /// The newest direct session open now: its path, how it was set up, and
    /// its carrier's view of it as of this read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<DirectLink>,
}
