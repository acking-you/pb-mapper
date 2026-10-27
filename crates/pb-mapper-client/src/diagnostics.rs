//! Bounded, credential-free recovery counters exposed separately from lifecycle status.
//!
//! Besides counters, a tunnel keeps the direct attempt it is making right now
//! and a short history of what happened to it ([`TunnelEvent`]), so a caller can
//! show recovery as it unfolds instead of a bare "retrying". Both are written
//! only on control-path transitions, never per packet.

use crate::endpoint::RelayEndpoint;
use serde::Serialize;
use std::collections::VecDeque;
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

/// Where a direct attempt stands. Every stage before [`Self::Committing`] can
/// still fall back to the relay; a fallback names the stage it left.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DirectStage {
    /// Asking the relay whether the publisher offers direct, and opening the
    /// control link.
    Negotiating,
    /// Learning this endpoint's public address and requesting a session grant.
    Discovering,
    /// Dialing the publisher through both NATs.
    Punching,
    /// The path is up; the publisher is opening its local service.
    Committing,
}

impl DirectStage {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Negotiating => "negotiating",
            Self::Discovering => "discovering",
            Self::Punching => "punching",
            Self::Committing => "committing",
        }
    }
}

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

/// What happened, in a [`TunnelEvent`].
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TunnelEventKind {
    /// The relay answered for the first time, or again after failures.
    RelayReady { latency_ms: u64 },
    /// A relay attempt failed; the next one is `retry_in_ms` away (zero when
    /// the worker retries at once or the failure is final).
    RelayFailed {
        failure: RecoveryFailure,
        retry_in_ms: u64,
    },
    /// The local network changed; recovery restarts at once.
    NetworkChanged,
    /// A session took the direct path.
    DirectEstablished { setup_ms: u64 },
    /// A session gave up on direct at `stage` and used the relay.
    DirectFallback { stage: DirectStage },
}

impl TunnelEventKind {
    /// Whether a repeat of `self` folds into the entry for `other`.
    fn folds_into(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::RelayFailed { failure: a, .. }, Self::RelayFailed { failure: b, .. }) => a == b,
            (Self::DirectFallback { stage: a }, Self::DirectFallback { stage: b }) => a == b,
            (Self::RelayReady { .. }, Self::RelayReady { .. })
            | (Self::NetworkChanged, Self::NetworkChanged)
            | (Self::DirectEstablished { .. }, Self::DirectEstablished { .. }) => true,
            _ => false,
        }
    }
}

/// One entry of a tunnel's recent history, oldest first in
/// [`TunnelDiagnostics::events`]. Consecutive repeats fold into one entry, so
/// a long outage cannot push out what came before it.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct TunnelEvent {
    /// Increases with every recorded event, a fold included, so a poller can
    /// tell what it has not seen yet.
    pub seq: u64,
    /// Time since the latest occurrence.
    pub age_ms: u64,
    /// Occurrences this entry stands for.
    pub repeats: u32,
    /// The latest occurrence.
    pub kind: TunnelEventKind,
}

/// How many entries a tunnel's history keeps.
const EVENT_CAPACITY: usize = 16;

/// A point-in-time view of this tunnel and its process-shared relay budget.
/// Non-exhaustive: fields are added as the SDK observes more.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct TunnelDiagnostics {
    /// The carrier this tunnel reaches the relay over, fixed when it started.
    pub relay_transport: crate::relay::RelayTransport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct: Option<PathDiagnostics>,
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
    /// Recent history, oldest first; at most 16 entries.
    pub events: Vec<TunnelEvent>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PathDiagnostics {
    pub direct_sessions: u64,
    pub relay_sessions: u64,
    pub active_direct: u64,
    pub active_relay: u64,
    pub fallbacks: u64,
    pub last_fallback: Option<String>,
    /// The direct attempt in flight, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<DirectAttempt>,
}

#[derive(Clone, Copy)]
struct Event {
    seq: u64,
    at: Instant,
    repeats: u32,
    kind: TunnelEventKind,
}

struct State {
    direct: Option<PathDiagnostics>,
    phase: RecoveryPhase,
    attempts: u64,
    failures: u64,
    failure: Option<RecoveryFailure>,
    success: Option<Instant>,
    latency: Option<Duration>,
    retry: Option<Instant>,
    /// Whether the last relay attempt succeeded, so only a change is recorded.
    ready: bool,
    /// The network generation last seen, so observers of one change record
    /// it once and a worker starting on an old one records nothing.
    network: Option<u64>,
    /// The stage and start of the latest direct attempt still in flight.
    attempt: Option<(DirectStage, Instant)>,
    in_flight: u32,
    seq: u64,
    events: VecDeque<Event>,
}

impl State {
    fn record(&mut self, kind: TunnelEventKind) {
        self.seq = self.seq.wrapping_add(1);
        let event = Event {
            seq: self.seq,
            at: Instant::now(),
            repeats: 1,
            kind,
        };
        if let Some(last) = self
            .events
            .back_mut()
            .filter(|last| kind.folds_into(&last.kind))
        {
            *last = Event {
                repeats: last.repeats.saturating_add(1),
                ..event
            };
            return;
        }
        if self.events.len() == EVENT_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }
}

#[derive(Clone)]
pub(crate) struct Diagnostics(Arc<Mutex<State>>);

impl Default for Diagnostics {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            direct: None,
            phase: RecoveryPhase::Starting,
            attempts: 0,
            failures: 0,
            failure: None,
            success: None,
            latency: None,
            retry: None,
            ready: false,
            network: None,
            attempt: None,
            in_flight: 0,
            seq: 0,
            events: VecDeque::new(),
        })))
    }
}

impl Diagnostics {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn has_active_direct(&self) -> bool {
        self.state()
            .direct
            .as_ref()
            .is_some_and(|p| p.active_direct != 0)
    }
    #[cfg(feature = "p2p")]
    pub(crate) fn path_started(&self, path: pb_mapper_protocol::direct::SessionPath) {
        let mut state = self.state();
        let p = state.direct.get_or_insert_default();
        match path {
            pb_mapper_protocol::direct::SessionPath::Direct => {
                p.direct_sessions += 1;
                p.active_direct += 1;
            }
            pb_mapper_protocol::direct::SessionPath::Relay => {
                p.relay_sessions += 1;
                p.active_relay += 1;
            }
        }
    }
    #[cfg(feature = "p2p")]
    pub(crate) fn path_closed(&self, path: pb_mapper_protocol::direct::SessionPath) {
        let mut state = self.state();
        let p = state.direct.get_or_insert_default();
        match path {
            pb_mapper_protocol::direct::SessionPath::Direct => {
                p.active_direct = p.active_direct.saturating_sub(1)
            }
            pb_mapper_protocol::direct::SessionPath::Relay => {
                p.active_relay = p.active_relay.saturating_sub(1)
            }
        }
    }
    /// Begin a direct attempt; the guard reports its stages and outcome.
    #[cfg(feature = "p2p")]
    pub(crate) fn direct_attempt(&self) -> DirectAttemptGuard {
        let started = Instant::now();
        let mut state = self.state();
        // The attempt rides on the path view, so it shows from the first one.
        state.direct.get_or_insert_default();
        state.in_flight = state.in_flight.saturating_add(1);
        state.attempt = Some((DirectStage::Negotiating, started));
        DirectAttemptGuard {
            diagnostics: self.clone(),
            stage: DirectStage::Negotiating,
            started,
        }
    }

    pub(crate) fn heard(&self) {
        self.state().success = Some(Instant::now());
    }
    pub(crate) fn responded(&self, elapsed: Duration) {
        let mut state = self.state();
        state.success = Some(Instant::now());
        state.latency = Some(elapsed);
    }
    pub(crate) fn phase(&self, phase: RecoveryPhase) {
        self.state().phase = phase;
    }
    pub(crate) fn attempt(&self) {
        let mut state = self.state();
        state.attempts = state.attempts.saturating_add(1);
        state.retry = None;
        state.phase = RecoveryPhase::Capacity;
    }
    pub(crate) fn succeeded(&self, elapsed: Duration) {
        let mut state = self.state();
        state.phase = RecoveryPhase::Ready;
        state.failures = 0;
        state.success = Some(Instant::now());
        state.latency = Some(elapsed);
        state.retry = None;
        if !state.ready {
            state.ready = true;
            state.record(TunnelEventKind::RelayReady {
                latency_ms: ms(elapsed),
            });
        }
    }
    pub(crate) fn failed(&self, failure: RecoveryFailure, retry: Duration) -> bool {
        let mut state = self.state();
        state.failures = state.failures.saturating_add(1);
        let emit = state.failure != Some(failure) || state.failures.is_power_of_two();
        state.phase = RecoveryPhase::Backoff;
        state.failure = Some(failure);
        state.retry = Some(Instant::now() + retry);
        state.ready = false;
        // A network change is recorded once, by whoever observes it first.
        if failure != RecoveryFailure::NetworkChanged {
            state.record(TunnelEventKind::RelayFailed {
                failure,
                retry_in_ms: ms(retry),
            });
        }
        emit
    }
    /// Note the network generation a worker starts on.
    pub(crate) fn network_seen(&self, generation: u64) {
        self.state().network.get_or_insert(generation);
    }
    /// Record a change of the local network to `generation`, once however
    /// many parts of this tunnel observe it.
    pub(crate) fn network_changed(&self, generation: u64) {
        let mut state = self.state();
        if state.network != Some(generation) {
            state.network = Some(generation);
            state.record(TunnelEventKind::NetworkChanged);
        }
    }
    pub(crate) fn snapshot(&self, endpoint: &RelayEndpoint) -> TunnelDiagnostics {
        let state = self.state();
        let now = Instant::now();
        let (active_control_setups, active_data_setups) = endpoint.active_setups();
        let attempt = state.attempt.map(|(stage, started)| DirectAttempt {
            stage,
            elapsed_ms: ms(now.saturating_duration_since(started)),
            in_flight: state.in_flight,
        });
        TunnelDiagnostics {
            relay_transport: endpoint.transport(),
            direct: state
                .direct
                .clone()
                .map(|direct| PathDiagnostics { attempt, ..direct }),
            sdk_version: env!("CARGO_PKG_VERSION"),
            last_attempt_phase: state.phase,
            attempts: state.attempts,
            consecutive_failures: state.failures,
            last_failure: state.failure,
            last_success_age_ms: state
                .success
                .map(|time| ms(now.saturating_duration_since(time))),
            last_setup_latency_ms: state.latency.map(ms),
            next_retry_in_ms: state
                .retry
                .map(|time| ms(time.saturating_duration_since(now))),
            dns_age_ms: endpoint.dns_age().map(ms),
            network_generation: endpoint.network_generation(),
            active_control_setups,
            active_data_setups,
            events: state
                .events
                .iter()
                .map(|event| TunnelEvent {
                    seq: event.seq,
                    age_ms: ms(now.saturating_duration_since(event.at)),
                    repeats: event.repeats,
                    kind: event.kind,
                })
                .collect(),
        }
    }
}

fn ms(time: Duration) -> u64 {
    time.as_millis().min(u128::from(u64::MAX)) as u64
}

/// One direct attempt in flight. Dropping it without an outcome (an error
/// after commit, a cancelled setup) records nothing.
#[cfg(feature = "p2p")]
pub(crate) struct DirectAttemptGuard {
    diagnostics: Diagnostics,
    stage: DirectStage,
    started: Instant,
}

#[cfg(feature = "p2p")]
impl DirectAttemptGuard {
    pub(crate) fn stage(&mut self, stage: DirectStage) {
        self.stage = stage;
        self.diagnostics.state().attempt = Some((stage, self.started));
    }
    pub(crate) fn established(self) {
        let setup_ms = ms(self.started.elapsed());
        self.diagnostics
            .state()
            .record(TunnelEventKind::DirectEstablished { setup_ms });
    }
    /// Fall back to the relay for `reason`, the legacy label kept in
    /// [`PathDiagnostics::last_fallback`].
    pub(crate) fn fell_back(self, reason: &str) {
        let mut state = self.diagnostics.state();
        let p = state.direct.get_or_insert_default();
        p.fallbacks += 1;
        p.last_fallback = Some(reason.into());
        state.record(TunnelEventKind::DirectFallback { stage: self.stage });
    }
}

#[cfg(feature = "p2p")]
impl Drop for DirectAttemptGuard {
    fn drop(&mut self) {
        let mut state = self.diagnostics.state();
        state.in_flight = state.in_flight.saturating_sub(1);
        if state.in_flight == 0 {
            state.attempt = None;
        }
    }
}

#[cfg(feature = "p2p")]
pub(crate) struct PathUsage(pub Diagnostics, pub pb_mapper_protocol::direct::SessionPath);
#[cfg(feature = "p2p")]
impl Drop for PathUsage {
    fn drop(&mut self) {
        self.0.path_closed(self.1);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn kinds(diagnostics: &Diagnostics) -> Vec<(TunnelEventKind, u32)> {
        diagnostics
            .snapshot(&RelayEndpoint::shared("127.0.0.1:1"))
            .events
            .into_iter()
            .map(|event| (event.kind, event.repeats))
            .collect()
    }

    #[tokio::test]
    async fn history_records_changes_and_folds_repeats() {
        let diagnostics = Diagnostics::default();
        diagnostics.succeeded(Duration::from_millis(40));
        // Steady success is not news: only a change is recorded.
        diagnostics.succeeded(Duration::from_millis(41));
        diagnostics.failed(RecoveryFailure::Transport, Duration::from_millis(200));
        diagnostics.failed(RecoveryFailure::Transport, Duration::from_millis(400));
        diagnostics.network_changed(1);
        diagnostics.network_changed(1);
        diagnostics.failed(RecoveryFailure::NetworkChanged, Duration::ZERO);
        diagnostics.succeeded(Duration::from_millis(55));
        assert_eq!(
            kinds(&diagnostics),
            vec![
                (TunnelEventKind::RelayReady { latency_ms: 40 }, 1),
                (
                    TunnelEventKind::RelayFailed {
                        failure: RecoveryFailure::Transport,
                        retry_in_ms: 400
                    },
                    2
                ),
                (TunnelEventKind::NetworkChanged, 1),
                (TunnelEventKind::RelayReady { latency_ms: 55 }, 1),
            ]
        );
    }

    #[tokio::test]
    async fn history_is_bounded_and_sequenced() {
        let diagnostics = Diagnostics::default();
        for generation in 1..=40 {
            diagnostics.network_changed(generation);
            diagnostics.failed(RecoveryFailure::Timeout, Duration::ZERO);
        }
        let events = diagnostics
            .snapshot(&RelayEndpoint::shared("127.0.0.1:1"))
            .events;
        assert_eq!(events.len(), EVENT_CAPACITY);
        assert!(events.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    }

    #[cfg(feature = "p2p")]
    #[tokio::test]
    async fn a_direct_attempt_shows_its_stage_until_it_settles() {
        let diagnostics = Diagnostics::default();
        let endpoint = RelayEndpoint::shared("127.0.0.1:1");
        let attempt_of = |d: &Diagnostics| d.snapshot(&endpoint).direct.and_then(|p| p.attempt);
        let mut first = diagnostics.direct_attempt();
        first.stage(DirectStage::Punching);
        assert_eq!(
            attempt_of(&diagnostics).unwrap().stage,
            DirectStage::Punching
        );
        let second = diagnostics.direct_attempt();
        let seen = attempt_of(&diagnostics).unwrap();
        assert_eq!((seen.stage, seen.in_flight), (DirectStage::Negotiating, 2));
        first.fell_back("direct_timeout_or_unreachable");
        assert_eq!(attempt_of(&diagnostics).unwrap().in_flight, 1);
        second.established();
        assert_eq!(attempt_of(&diagnostics), None);
        let direct = diagnostics.snapshot(&endpoint).direct.unwrap();
        assert_eq!(direct.fallbacks, 1);
        assert_eq!(
            direct.last_fallback.as_deref(),
            Some("direct_timeout_or_unreachable")
        );
        assert!(matches!(
            kinds(&diagnostics)[..],
            [
                (
                    TunnelEventKind::DirectFallback {
                        stage: DirectStage::Punching
                    },
                    1
                ),
                (TunnelEventKind::DirectEstablished { .. }, 1),
            ]
        ));
    }
}
