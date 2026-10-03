//! Bounded, credential-free recovery counters exposed separately from lifecycle status.

use crate::endpoint::RelayEndpoint;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};

/// The phase most recently entered by a tunnel worker.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPhase {
    Starting,
    Capacity,
    Resolving,
    Connecting,
    Handshake,
    Ready,
    Backoff,
    Stopped,
}

impl RecoveryPhase {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Capacity => "capacity",
            Self::Resolving => "resolving",
            Self::Connecting => "connecting",
            Self::Handshake => "handshake",
            Self::Ready => "ready",
            Self::Backoff => "backoff",
            Self::Stopped => "stopped",
        }
    }
}

/// Why a recovery attempt failed, without credentials or application data.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryFailure {
    Dns,
    Transport,
    Timeout,
    ServiceUnavailable,
    Rejected,
    NetworkChanged,
}

impl RecoveryFailure {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Transport => "transport",
            Self::Timeout => "timeout",
            Self::ServiceUnavailable => "service_unavailable",
            Self::Rejected => "rejected",
            Self::NetworkChanged => "network_changed",
        }
    }
}

/// A point-in-time view of this tunnel and its process-shared relay budget.
#[derive(Clone, Debug, Serialize)]
pub struct TunnelDiagnostics {
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
}

struct State {
    phase: RecoveryPhase,
    attempts: u64,
    failures: u64,
    failure: Option<RecoveryFailure>,
    success: Option<Instant>,
    latency: Option<Duration>,
    retry: Option<Instant>,
}

#[derive(Clone)]
pub(crate) struct Diagnostics(Arc<Mutex<State>>);

impl Default for Diagnostics {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            phase: RecoveryPhase::Starting,
            attempts: 0,
            failures: 0,
            failure: None,
            success: None,
            latency: None,
            retry: None,
        })))
    }
}

impl Diagnostics {
    pub(crate) fn heard(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).success = Some(Instant::now());
    }
    pub(crate) fn responded(&self, elapsed: Duration) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.success = Some(Instant::now());
        state.latency = Some(elapsed);
    }
    pub(crate) fn phase(&self, phase: RecoveryPhase) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).phase = phase;
    }
    pub(crate) fn attempt(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.attempts = state.attempts.saturating_add(1);
        state.retry = None;
        state.phase = RecoveryPhase::Capacity;
    }
    pub(crate) fn succeeded(&self, elapsed: Duration) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = RecoveryPhase::Ready;
        state.failures = 0;
        state.success = Some(Instant::now());
        state.latency = Some(elapsed);
        state.retry = None;
    }
    pub(crate) fn failed(&self, failure: RecoveryFailure, retry: Duration) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.failures = state.failures.saturating_add(1);
        let emit = state.failure != Some(failure) || state.failures.is_power_of_two();
        state.phase = RecoveryPhase::Backoff;
        state.failure = Some(failure);
        state.retry = Some(Instant::now() + retry);
        emit
    }
    pub(crate) fn snapshot(&self, endpoint: &RelayEndpoint) -> TunnelDiagnostics {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let ms = |time: Duration| time.as_millis().min(u128::from(u64::MAX)) as u64;
        let (active_control_setups, active_data_setups) = endpoint.active_setups();
        TunnelDiagnostics {
            sdk_version: env!("CARGO_PKG_VERSION"),
            last_attempt_phase: state.phase,
            attempts: state.attempts,
            consecutive_failures: state.failures,
            last_failure: state.failure,
            last_success_age_ms: state.success.map(|time| ms(time.elapsed())),
            last_setup_latency_ms: state.latency.map(ms),
            next_retry_in_ms: state
                .retry
                .map(|time| ms(time.saturating_duration_since(Instant::now()))),
            dns_age_ms: endpoint.dns_age().map(ms),
            network_generation: endpoint.network_generation(),
            active_control_setups,
            active_data_setups,
        }
    }
}
