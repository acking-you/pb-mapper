//! Bounded, credential-free recovery counters exposed separately from lifecycle status.
//!
//! Besides counters, a tunnel keeps the direct attempt it is making right now
//! and a short history of what happened to it ([`TunnelEvent`]), so a caller can
//! show recovery as it unfolds instead of a bare "retrying"; [`TunnelEvents`]
//! delivers each event as it happens. All are written only on control-path
//! transitions, never per packet: the one per-packet figure, a session's
//! bytes, is a relaxed atomic its forwarding loop adds to and a read sums.
//! [`TunnelMetrics`] is the part that keeps changing, for sampling.

use crate::endpoint::RelayEndpoint;
use crate::relay::RelayTransport;
use pb_mapper_protocol::direct::{DirectTransport, LinkSummary, SessionPath};
use pb_mapper_protocol::forward::ByteMeter;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};
use tokio::time::{Duration, Instant};

mod link;
mod metrics;
mod port_mapping;
pub use link::{
    AddressFamily, CandidateCounts, DirectLink, InterfaceTier, LanEvidence, LinkKind, LinkStats,
    LinkVia, SetupTimings, StunOutcome,
};
use metrics::Bucket;
pub use metrics::{
    ByteCount, LanCounts, LinkSample, PathTraffic, RelayHealth, SessionCounts, TunnelMetrics,
    TunnelMetricsStream,
};
pub use port_mapping::{
    PortMappingDiagnostics, PortMappingProtocol, PortMappingStage, PortMappingStatus,
};

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

/// Why a direct attempt gave way to the relay. Serialized, like
/// [`Self::as_str`], as the label [`PathDiagnostics::last_fallback`] has
/// always carried.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum FallbackReason {
    /// The relay or the publisher does not offer direct, or offers another
    /// version of it.
    #[serde(rename = "direct_not_offered")]
    NotOffered,
    /// Asking for direct, opening its control link, discovering this end's
    /// addresses or the grant failed.
    #[serde(rename = "direct_negotiation_failed")]
    NegotiationFailed,
    /// Those took longer than their budget of 3 s.
    #[serde(rename = "direct_negotiation_timeout")]
    NegotiationTimeout,
    /// No path through both NATs was set up in time.
    #[serde(rename = "direct_timeout_or_unreachable")]
    Unreachable,
}

impl FallbackReason {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotOffered => "direct_not_offered",
            Self::NegotiationFailed => "direct_negotiation_failed",
            Self::NegotiationTimeout => "direct_negotiation_timeout",
            Self::Unreachable => "direct_timeout_or_unreachable",
        }
    }
}

/// Why a direct setup that asked for TCP within a local network did not run
/// over it, and took QUIC or the relay instead.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LanTcpMiss {
    /// The publisher offered none: it, or the relay, predates TCP; it does
    /// not serve it; or it shares no network with this end as it sees it.
    NotOffered,
    /// It offered addresses, none on a network of this end's.
    NoSharedLan,
    /// Its address refused the connection, or could not be reached.
    Refused,
    /// Nothing answered in time: a firewall dropping it, typically.
    TimedOut,
    /// TCP connected, and TLS failed.
    Handshake,
}

impl LanTcpMiss {
    /// Stable diagnostic label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotOffered => "not_offered",
            Self::NoSharedLan => "no_shared_lan",
            Self::Refused => "refused",
            Self::TimedOut => "timed_out",
            Self::Handshake => "handshake",
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
///
/// The first six are outcomes, kept in [`TunnelDiagnostics::events`] as well
/// as delivered to [`TunnelEvents`]. The rest are steps along the way,
/// delivered live only: kept, they would crowd the outcomes out of a history
/// of 16.
///
/// Non-exhaustive: kinds are added as the SDK observes more, so a `match`
/// needs a wildcard arm.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
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
    /// A session gave up on direct at `stage`, for `reason`, and used the
    /// relay.
    DirectFallback {
        stage: DirectStage,
        reason: FallbackReason,
    },
    /// The tunnel stopped, and whatever it was cleaning up has reported. The
    /// last event of every stream.
    Stopped,
    /// A relay attempt entered `phase`. Once the tunnel is ready, its routine
    /// re-checks are not announced: the next change is `backoff`, when one
    /// fails.
    PhaseChanged { phase: RecoveryPhase },
    /// A direct attempt entered `stage`.
    DirectStageChanged { stage: DirectStage },
    /// A direct attempt ended at `stage` with neither a direct session nor a
    /// fallback: its setup was cancelled, or it failed after committing, when
    /// the relay can no longer take its place.
    DirectAbandoned { stage: DirectStage },
    /// Session number `session` of this tunnel opened on `path`; a direct
    /// one across `link`.
    SessionOpened {
        session: u64,
        path: SessionPath,
        link: Option<LinkKind>,
    },
    /// Session `session` closed, after `duration_ms`, having carried these
    /// application bytes: sent from this end's local side, and received.
    SessionClosed {
        session: u64,
        path: SessionPath,
        link: Option<LinkKind>,
        duration_ms: u64,
        sent_bytes: u64,
        received_bytes: u64,
    },
    /// The gateway mapping of the tunnel's direct endpoint changed.
    PortMappingChanged { mapping: Box<PortMappingStatus> },
    /// A direct setup found the peer on one of this end's local networks,
    /// before any packet. With preferred, this end's LAN-first is on: the
    /// peer's LAN addresses are probed first.
    LanDetected {
        evidence: LanEvidence,
        preferred: bool,
    },
    /// A direct session committed to a path: across `link`, over `family`,
    /// to an address learned `via`, carried by `transport`.
    PathSelected {
        link: LinkKind,
        family: AddressFamily,
        via: LinkVia,
        transport: DirectTransport,
    },
    /// A direct setup that found the peer on a local network, by
    /// `evidence`, did not end there: it took a path across `took`, or the
    /// relay when `None`.
    LanMissed {
        evidence: LanEvidence,
        took: Option<LinkKind>,
    },
    /// A direct setup that asked for TCP within a local network did not get
    /// it, for `reason`: it took `took`, or the relay when `None`.
    LanTcpMissed {
        reason: LanTcpMiss,
        took: Option<DirectTransport>,
    },
}

impl TunnelEventKind {
    /// Whether a repeat of `self` folds into the entry for `other`.
    fn folds_into(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::RelayFailed { failure: a, .. }, Self::RelayFailed { failure: b, .. }) => a == b,
            (
                Self::DirectFallback {
                    stage: a,
                    reason: x,
                },
                Self::DirectFallback {
                    stage: b,
                    reason: y,
                },
            ) => (a, x) == (b, y),
            (Self::RelayReady { .. }, Self::RelayReady { .. })
            | (Self::NetworkChanged, Self::NetworkChanged)
            | (Self::DirectEstablished { .. }, Self::DirectEstablished { .. }) => true,
            _ => false,
        }
    }

    /// Whether this is a step delivered live only, never kept in the history.
    fn is_live_only(&self) -> bool {
        matches!(
            self,
            Self::PhaseChanged { .. }
                | Self::DirectStageChanged { .. }
                | Self::DirectAbandoned { .. }
                | Self::SessionOpened { .. }
                | Self::SessionClosed { .. }
                | Self::PortMappingChanged { .. }
                | Self::LanDetected { .. }
                | Self::PathSelected { .. }
                | Self::LanMissed { .. }
                | Self::LanTcpMissed { .. }
        )
    }
}

/// One event of a tunnel. In [`TunnelDiagnostics::events`], oldest first,
/// consecutive repeats fold into one entry, so a long outage cannot push out
/// what came before it; [`TunnelEvents`] delivers every occurrence.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TunnelEvent {
    /// Increases with every event, a fold included, so a reader can tell
    /// what it has not seen yet. A stream and the history share it.
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

/// How many events a subscriber may fall behind before it misses some.
const STREAM_CAPACITY: usize = 128;

/// A tunnel's events as they happen, after the history it has kept: from
/// `subscribe_events` on a [`Registration`](crate::sdk::Registration) or a
/// [`Connection`](crate::sdk::Connection).
///
/// Every subscriber reads at its own pace from its own queue. One that falls
/// more than 128 events behind is told how many it missed, and goes on from
/// there. A subscription does not keep its tunnel alive: once the tunnel
/// stops and its cleanup has reported, the stream ends.
pub struct TunnelEvents {
    replay: VecDeque<Event>,
    receiver: broadcast::Receiver<Event>,
}

impl TunnelEvents {
    /// The next event, or `None` once the stream has ended. An error says
    /// events were missed; the tunnel's `diagnostics()` is the state to
    /// reconcile with, and the stream goes on. Cancel-safe.
    pub async fn recv(&mut self) -> Option<Result<TunnelEvent, EventsLagged>> {
        // Aged as it is read, however long it waited in the queue.
        if let Some(event) = self.replay.pop_front() {
            return Some(Ok(event.public(Instant::now())));
        }
        match self.receiver.recv().await {
            Ok(event) => Some(Ok(event.public(Instant::now()))),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                Some(Err(EventsLagged { skipped }))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

/// A [`TunnelEvents`] subscriber fell behind and missed `skipped` events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventsLagged {
    pub skipped: u64,
}

impl std::fmt::Display for EventsLagged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "missed {} tunnel events", self.skipped)
    }
}

impl std::error::Error for EventsLagged {}

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
    pub traffic: PathTraffic,
    /// The smoothed round trip of its control exchanges with the relay; see
    /// [`RelayHealth::rtt_ms`].
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

#[derive(Clone)]
struct Event {
    seq: u64,
    at: Instant,
    repeats: u32,
    kind: TunnelEventKind,
}

impl Event {
    fn public(&self, now: Instant) -> TunnelEvent {
        TunnelEvent {
            seq: self.seq,
            age_ms: ms(now.saturating_duration_since(self.at)),
            repeats: self.repeats,
            kind: self.kind.clone(),
        }
    }
}

/// How far a tunnel is from the end of its event stream.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Running,
    /// Stopped, while a cleanup that reports may still be running.
    Stopping,
    /// The stream has ended.
    Closed,
}

/// A session open now: its bytes are read from its meter until it closes.
struct OpenSession {
    path: SessionPath,
    link: Option<LinkSummary>,
    opened: Instant,
    meter: Arc<ByteMeter>,
}

struct State {
    lifecycle: Lifecycle,
    started: Instant,
    /// Cleanups that hold the stream open past a stop.
    cleanups: usize,
    /// Present while someone subscribes: a tunnel nobody listens to sends
    /// nothing, and records no live-only event.
    stream: Option<broadcast::Sender<Event>>,
    /// Bumped on every recorded event, whoever subscribes, so a metrics
    /// stream samples a change at once; dropped when the event stream ends,
    /// which ends the metrics streams too.
    revision: Option<watch::Sender<u64>>,
    /// The counters behind [`TunnelDiagnostics::direct`] and
    /// [`TunnelMetrics::sessions`].
    direct: Option<PathDiagnostics>,
    sessions: HashMap<u64, OpenSession>,
    next_session: u64,
    /// The bytes of the sessions that have closed, by [`Bucket`].
    closed: [ByteCount; Bucket::ALL],
    /// The smoothed round trip of control exchanges with the relay.
    relay_rtt: Option<Duration>,
    /// Until when sessions skip direct after a fallback, and how many have.
    cooldown: Option<Instant>,
    cooldown_skips: u64,
    /// Whether direct setups try a shared local network first.
    lan_first: bool,
    /// What they ask a shared local network to carry.
    lan_transport: crate::sdk::LanTransport,
    port_mapping: Option<PortMappingDiagnostics>,
    phase: RecoveryPhase,
    /// The phase last announced to subscribers. A ready tunnel's routine
    /// re-checks pass through the attempt phases without being announced.
    announced: RecoveryPhase,
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
    /// Direct sessions open now, oldest first, each held only while it is:
    /// the connection is a clone, and Quinn has no weak one.
    #[cfg(feature = "p2p")]
    links: Vec<LiveLink>,
    #[cfg(feature = "p2p")]
    next_link: u64,
    seq: u64,
    events: VecDeque<Event>,
}

impl State {
    fn record(&mut self, kind: TunnelEventKind) {
        if self.lifecycle == Lifecycle::Closed {
            return;
        }
        if let Some(revision) = &self.revision {
            revision.send_modify(|revision| *revision = revision.wrapping_add(1));
        }
        let live_only = kind.is_live_only();
        if live_only && self.stream.is_none() {
            return;
        }
        self.seq = self.seq.wrapping_add(1);
        let event = Event {
            seq: self.seq,
            at: Instant::now(),
            repeats: 1,
            kind,
        };
        if live_only {
            self.send(event);
            return;
        }
        if self.stream.is_some() {
            self.send(event.clone());
        }
        if let Some(last) = self
            .events
            .back_mut()
            .filter(|last| event.kind.folds_into(&last.kind))
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

    fn send(&mut self, event: Event) {
        // Sending fails only once every subscription is gone.
        if let Some(stream) = &self.stream
            && stream.send(event).is_err()
        {
            self.stream = None;
        }
    }

    /// A worker's report, which a stopped tunnel no longer takes.
    fn running(&self) -> bool {
        self.lifecycle == Lifecycle::Running
    }

    fn enter(&mut self, phase: RecoveryPhase) {
        self.phase = phase;
        let recheck = self.ready
            && matches!(
                phase,
                RecoveryPhase::Capacity
                    | RecoveryPhase::Resolving
                    | RecoveryPhase::Connecting
                    | RecoveryPhase::Handshake
            );
        if !recheck && self.announced != phase {
            self.announced = phase;
            self.record(TunnelEventKind::PhaseChanged { phase });
        }
    }

    fn close_if_drained(&mut self) {
        if self.lifecycle == Lifecycle::Stopping && self.cleanups == 0 {
            self.record(TunnelEventKind::Stopped);
            self.lifecycle = Lifecycle::Closed;
            self.stream = None;
            self.revision = None;
        }
    }

    /// Application bytes by path: the closed sessions', and so far the
    /// open ones'.
    fn traffic(&self) -> PathTraffic {
        let mut buckets = self.closed;
        for session in self.sessions.values() {
            let bucket = Bucket::of(session.path, session.link.map(|link| link.kind)).index();
            buckets[bucket] = buckets[bucket].add(session.meter.totals());
        }
        Bucket::traffic(buckets)
    }

    fn metrics(&self, transport: RelayTransport, now: Instant) -> TunnelMetrics {
        let none = PathDiagnostics::default();
        let paths = self.direct.as_ref().unwrap_or(&none);
        #[cfg(feature = "p2p")]
        let link = self.links.last().map(LiveLink::metrics);
        #[cfg(not(feature = "p2p"))]
        let link = None;
        TunnelMetrics {
            uptime_ms: ms(now.saturating_duration_since(self.started)),
            event_seq: self.seq,
            sessions: SessionCounts {
                active_relay: paths.active_relay,
                active_direct: paths.active_direct,
                active_lan: paths.active_lan,
                relay: paths.relay_sessions,
                direct: paths.direct_sessions,
                lan: paths.lan_sessions,
            },
            traffic: self.traffic(),
            relay: RelayHealth {
                transport,
                phase: self.phase,
                consecutive_failures: self.failures,
                rtt_ms: self.relay_rtt.map(ms),
                last_success_age_ms: self
                    .success
                    .map(|time| ms(now.saturating_duration_since(time))),
            },
            link,
            lan: LanCounts {
                detected: paths.lan_detected,
                missed: paths.lan_missed,
            },
            direct_cooldown_ms: self
                .cooldown
                .filter(|until| *until > now)
                .map(|until| ms(until - now)),
            cooldown_skips: self.cooldown_skips,
            lan_first: self.lan_first,
        }
    }
}

/// Fold `sample` into the smoothed round trip `smoothed`, as TCP does
/// (RFC 6298): an eighth of each new sample.
pub(crate) fn smooth(smoothed: Option<Duration>, sample: Duration) -> Duration {
    match smoothed {
        None => sample,
        Some(smoothed) => (smoothed * 7 + sample) / 8,
    }
}

#[derive(Clone)]
pub(crate) struct Diagnostics(Arc<Mutex<State>>);

impl Default for Diagnostics {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            lifecycle: Lifecycle::Running,
            started: Instant::now(),
            cleanups: 0,
            stream: None,
            revision: Some(watch::channel(0).0),
            direct: None,
            sessions: HashMap::new(),
            next_session: 0,
            closed: [ByteCount::default(); Bucket::ALL],
            relay_rtt: None,
            cooldown: None,
            cooldown_skips: 0,
            lan_first: false,
            lan_transport: crate::sdk::LanTransport::Quic,
            port_mapping: None,
            phase: RecoveryPhase::Starting,
            announced: RecoveryPhase::Starting,
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
            #[cfg(feature = "p2p")]
            links: Vec::new(),
            #[cfg(feature = "p2p")]
            next_link: 0,
            seq: 0,
            events: VecDeque::new(),
        })))
    }
}

impl Diagnostics {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// The history kept so far, then every event from now on, with nothing
    /// lost or repeated between the two.
    pub(crate) fn subscribe(&self) -> TunnelEvents {
        let mut state = self.state();
        let receiver = match state.lifecycle {
            // Its sender is gone at once: the history, then the end.
            Lifecycle::Closed => broadcast::channel(1).1,
            Lifecycle::Running | Lifecycle::Stopping => state
                .stream
                .get_or_insert_with(|| broadcast::channel(STREAM_CAPACITY).0)
                .subscribe(),
        };
        TunnelEvents {
            replay: state.events.iter().cloned().collect(),
            receiver,
        }
    }
    /// What changes with every recorded event; `None` once the tunnel has
    /// stopped and its cleanup has reported.
    fn revision(&self) -> Option<watch::Receiver<u64>> {
        self.state().revision.as_ref().map(watch::Sender::subscribe)
    }
    /// The figures that keep changing, sampled now.
    pub(crate) fn metrics(&self, transport: RelayTransport) -> TunnelMetrics {
        self.state().metrics(transport, Instant::now())
    }
    /// Sample [`Self::metrics`] every `every`, and as the tunnel changes.
    pub(crate) fn subscribe_metrics(
        &self,
        transport: RelayTransport,
        every: Duration,
    ) -> TunnelMetricsStream {
        TunnelMetricsStream::new(self.clone(), transport, every)
    }
    /// The tunnel stopped. Later reports from its workers are ignored, and the
    /// stream ends once every cleanup still running has reported.
    pub(crate) fn stop(&self) {
        let mut state = self.state();
        if state.running() {
            state.lifecycle = Lifecycle::Stopping;
            state.phase = RecoveryPhase::Stopped;
        }
        state.close_if_drained();
    }
    /// Hold the stream open past [`Self::stop`] until the guard drops, so a
    /// cleanup can say how it went.
    #[cfg(feature = "port-mapping")]
    pub(crate) fn defer_close(&self) -> DeferClose {
        self.state().cleanups += 1;
        DeferClose(self.clone())
    }
    /// Whether this tunnel's direct setups try a shared local network first,
    /// and what they ask it to carry.
    pub(crate) fn prefer_lan(&self, prefer: bool, transport: crate::sdk::LanTransport) {
        let mut state = self.state();
        state.lan_first = prefer;
        state.lan_transport = transport;
    }
    /// This tunnel may map its direct endpoint's port: once that opens, or
    /// (`Auto`) once punching through has failed.
    pub(crate) fn port_mapping_requested(
        &self,
        mode: crate::sdk::PortMapping,
        network_generation: u64,
    ) {
        let stage = match mode {
            crate::sdk::PortMapping::Auto => PortMappingStage::Standby,
            _ => PortMappingStage::WaitingForDirect,
        };
        self.state()
            .port_mapping
            .get_or_insert_with(|| PortMappingDiagnostics {
                current: PortMappingStatus::new(stage, network_generation),
                attempts: 0,
                renewals: 0,
                failures: 0,
            });
    }
    #[cfg(feature = "port-mapping")]
    pub(crate) fn port_mapping_changed(&self, mut status: PortMappingStatus) {
        if let Some(error) = &mut status.error
            && let Some((cut, _)) = error.char_indices().nth(256)
        {
            error.truncate(cut);
        }
        let mut state = self.state();
        if state.lifecycle == Lifecycle::Closed {
            return;
        }
        let mapping = state
            .port_mapping
            .get_or_insert_with(|| PortMappingDiagnostics {
                current: status.clone(),
                attempts: 0,
                renewals: 0,
                failures: 0,
            });
        match status.stage {
            PortMappingStage::Requesting => mapping.attempts += 1,
            PortMappingStage::Renewing => mapping.renewals += 1,
            PortMappingStage::Failed | PortMappingStage::ReleaseFailed => mapping.failures += 1,
            _ => {}
        }
        mapping.current = status.clone();
        state.record(TunnelEventKind::PortMappingChanged {
            mapping: Box::new(status),
        });
    }
    pub(crate) fn has_active_direct(&self) -> bool {
        self.state()
            .direct
            .as_ref()
            .is_some_and(|p| p.active_direct != 0)
    }
    /// A session opened on `path`, a direct one across `link`: counted
    /// until the guard drops, its bytes through the guard's meter.
    pub(crate) fn session_opened(
        &self,
        path: SessionPath,
        link: Option<LinkSummary>,
    ) -> SessionGuard {
        let meter = Arc::new(ByteMeter::default());
        let mut state = self.state();
        state.next_session += 1;
        let id = state.next_session;
        let direct = |kind: fn(&LinkSummary) -> bool| {
            path == SessionPath::Direct && link.as_ref().is_some_and(kind)
        };
        let lan = direct(|link| link.kind == LinkKind::Lan);
        let tcp = direct(|link| link.transport == DirectTransport::Tcp);
        let p = state.direct.get_or_insert_default();
        match path {
            SessionPath::Direct => {
                p.direct_sessions += 1;
                p.active_direct += 1;
            }
            SessionPath::Relay => {
                p.relay_sessions += 1;
                p.active_relay += 1;
            }
        }
        if lan {
            p.lan_sessions += 1;
            p.active_lan += 1;
        }
        if tcp {
            p.lan_tcp_sessions += 1;
            p.active_lan_tcp += 1;
        }
        state.sessions.insert(
            id,
            OpenSession {
                path,
                link,
                opened: Instant::now(),
                meter: meter.clone(),
            },
        );
        state.record(TunnelEventKind::SessionOpened {
            session: id,
            path,
            link: link.map(|link| link.kind),
        });
        SessionGuard {
            diagnostics: self.clone(),
            id,
            meter,
        }
    }
    fn session_closed(&self, id: u64) {
        let mut state = self.state();
        let Some(session) = state.sessions.remove(&id) else {
            return;
        };
        let totals = session.meter.totals();
        let bucket = Bucket::of(session.path, session.link.map(|link| link.kind)).index();
        state.closed[bucket] = state.closed[bucket].add(totals);
        let p = state.direct.get_or_insert_default();
        match session.path {
            SessionPath::Direct => p.active_direct = p.active_direct.saturating_sub(1),
            SessionPath::Relay => p.active_relay = p.active_relay.saturating_sub(1),
        }
        if bucket == Bucket::DirectLan.index() {
            p.active_lan = p.active_lan.saturating_sub(1);
        }
        if session
            .link
            .is_some_and(|link| link.transport == DirectTransport::Tcp)
        {
            p.active_lan_tcp = p.active_lan_tcp.saturating_sub(1);
        }
        state.record(TunnelEventKind::SessionClosed {
            session: id,
            path: session.path,
            link: session.link.map(|link| link.kind),
            duration_ms: ms(session.opened.elapsed()),
            sent_bytes: totals.sent,
            received_bytes: totals.received,
        });
    }
    /// A setup found the peer on one of this end's local networks.
    #[cfg(feature = "p2p")]
    pub(crate) fn lan_detected(&self, evidence: LanEvidence, preferred: bool) {
        let mut state = self.state();
        state.direct.get_or_insert_default().lan_detected += 1;
        state.record(TunnelEventKind::LanDetected {
            evidence,
            preferred,
        });
    }
    /// A setup that found the peer on a local network, by `evidence`, took
    /// a path across `took` instead, or the relay.
    #[cfg(feature = "p2p")]
    pub(crate) fn lan_missed(&self, evidence: LanEvidence, took: Option<LinkKind>) {
        let mut state = self.state();
        state.direct.get_or_insert_default().lan_missed += 1;
        state.record(TunnelEventKind::LanMissed { evidence, took });
    }
    /// A setup that asked for TCP within a local network did not get it,
    /// for `reason`, and took `took` instead, or the relay.
    #[cfg(feature = "p2p")]
    pub(crate) fn lan_tcp_missed(&self, reason: LanTcpMiss, took: Option<DirectTransport>) {
        self.state()
            .record(TunnelEventKind::LanTcpMissed { reason, took });
    }
    /// Direct is paused after a fallback until `until`, or no longer.
    #[cfg(feature = "p2p")]
    pub(crate) fn direct_cooldown(&self, until: Option<Instant>) {
        self.state().cooldown = until;
    }
    /// A session went straight to the relay because direct is paused.
    #[cfg(feature = "p2p")]
    pub(crate) fn cooldown_skip(&self) {
        self.state().cooldown_skips += 1;
    }
    /// A control exchange with `endpoint` took `rtt`, there and back: this
    /// tunnel's figure, and the endpoint's, which its other tunnels share.
    pub(crate) fn relay_rtt(&self, endpoint: &RelayEndpoint, rtt: Duration) {
        endpoint.control_rtt(rtt);
        let mut state = self.state();
        state.relay_rtt = Some(smooth(state.relay_rtt, rtt));
    }
    /// A direct session is open on `link`: kept, with `probe` for its live
    /// figures, until the guard drops.
    #[cfg(feature = "p2p")]
    pub(crate) fn link_opened(&self, link: DirectLink, probe: LinkProbe) -> LinkGuard {
        let mut state = self.state();
        state.next_link += 1;
        let id = state.next_link;
        state.record(TunnelEventKind::PathSelected {
            link: link.kind,
            family: link.family,
            via: link.via,
            transport: link.transport,
        });
        // A provider holds at most 64 sessions; a consumer has no such cap,
        // and the newest is what is shown.
        if state.links.len() == 64 {
            state.links.remove(0);
        }
        state.links.push(LiveLink { id, link, probe });
        LinkGuard {
            diagnostics: self.clone(),
            id,
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
        state.record(TunnelEventKind::DirectStageChanged {
            stage: DirectStage::Negotiating,
        });
        DirectAttemptGuard {
            diagnostics: self.clone(),
            stage: DirectStage::Negotiating,
            started,
            settled: false,
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
        let mut state = self.state();
        if state.running() {
            state.enter(phase);
        }
    }
    pub(crate) fn attempt(&self) {
        let mut state = self.state();
        if !state.running() {
            return;
        }
        state.attempts = state.attempts.saturating_add(1);
        state.retry = None;
        state.enter(RecoveryPhase::Capacity);
    }
    pub(crate) fn succeeded(&self, elapsed: Duration) {
        let mut state = self.state();
        if !state.running() {
            return;
        }
        state.enter(RecoveryPhase::Ready);
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
        if !state.running() {
            return false;
        }
        state.failures = state.failures.saturating_add(1);
        let emit = state.failure != Some(failure) || state.failures.is_power_of_two();
        state.enter(RecoveryPhase::Backoff);
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
        #[cfg(feature = "p2p")]
        let link = state.links.last().map(LiveLink::sample);
        #[cfg(not(feature = "p2p"))]
        let link = None;
        TunnelDiagnostics {
            relay_transport: endpoint.transport(),
            lan_first: state.lan_first,
            lan_transport: state.lan_transport,
            direct: state.direct.clone().map(|direct| PathDiagnostics {
                attempt,
                link,
                ..direct
            }),
            traffic: state.traffic(),
            relay_rtt_ms: state.relay_rtt.map(ms),
            port_mapping: state.port_mapping.clone(),
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
            events: state.events.iter().map(|event| event.public(now)).collect(),
        }
    }
}

pub(crate) fn ms(time: Duration) -> u64 {
    time.as_millis().min(u128::from(u64::MAX)) as u64
}

/// Holds a tunnel's event stream open past its stop; see
/// [`Diagnostics::defer_close`].
#[cfg(feature = "port-mapping")]
pub(crate) struct DeferClose(Diagnostics);

#[cfg(feature = "port-mapping")]
impl Drop for DeferClose {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.cleanups = state.cleanups.saturating_sub(1);
        state.close_if_drained();
    }
}

/// One direct attempt in flight. Dropped without an outcome (an error after
/// commit, a cancelled setup), it tells subscribers the attempt was abandoned,
/// so that one following its stages does not wait on it forever.
#[cfg(feature = "p2p")]
pub(crate) struct DirectAttemptGuard {
    diagnostics: Diagnostics,
    stage: DirectStage,
    started: Instant,
    settled: bool,
}

#[cfg(feature = "p2p")]
impl DirectAttemptGuard {
    pub(crate) fn stage(&mut self, stage: DirectStage) {
        self.stage = stage;
        let mut state = self.diagnostics.state();
        state.attempt = Some((stage, self.started));
        state.record(TunnelEventKind::DirectStageChanged { stage });
    }
    /// Time since the attempt began.
    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
    /// The attempt went direct, on a path of `kind`, after `setup`.
    pub(crate) fn established(mut self, setup: Duration, kind: LinkKind) {
        self.settled = true;
        let mut state = self.diagnostics.state();
        state.direct.get_or_insert_default().last_kind = Some(kind);
        state.record(TunnelEventKind::DirectEstablished {
            setup_ms: ms(setup),
        });
    }
    /// Fall back to the relay for `reason`, whose label
    /// [`PathDiagnostics::last_fallback`] keeps.
    pub(crate) fn fell_back(mut self, reason: FallbackReason) {
        self.settled = true;
        let mut state = self.diagnostics.state();
        let p = state.direct.get_or_insert_default();
        p.fallbacks += 1;
        p.last_fallback = Some(reason.as_str().into());
        state.record(TunnelEventKind::DirectFallback {
            stage: self.stage,
            reason,
        });
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
        if !self.settled {
            state.record(TunnelEventKind::DirectAbandoned { stage: self.stage });
        }
    }
}

/// Where a direct session's live figures come from.
#[cfg(feature = "p2p")]
pub(crate) enum LinkProbe {
    /// QUIC's own, read from the connection as they are now.
    Quic(quinn::Connection),
    /// TCP keeps its figures in the kernel; the round trip of the
    /// connection's handshake stands in for them.
    Tcp { handshake_rtt: Duration },
}

/// A direct session's path and what its figures are read from.
#[cfg(feature = "p2p")]
struct LiveLink {
    id: u64,
    link: DirectLink,
    probe: LinkProbe,
}

#[cfg(feature = "p2p")]
impl LiveLink {
    /// The carrier's figures as they are now.
    fn stats(&self) -> LinkStats {
        let connection = match &self.probe {
            LinkProbe::Quic(connection) => connection,
            LinkProbe::Tcp { handshake_rtt } => {
                return LinkStats {
                    rtt_us: micros(*handshake_rtt),
                    min_rtt_us: micros(*handshake_rtt),
                    ..LinkStats::default()
                };
            }
        };
        let stats = connection.stats();
        LinkStats {
            rtt_us: micros(connection.rtt()),
            min_rtt_us: micros(stats.path.min_rtt),
            congestion_window: stats.path.cwnd,
            mtu: stats.path.current_mtu,
            sent_packets: stats.path.sent_packets,
            lost_packets: stats.path.lost_packets,
            congestion_events: stats.path.congestion_events,
            sent_bytes: stats.udp_tx.bytes,
            received_bytes: stats.udp_rx.bytes,
            lost_bytes: stats.path.lost_bytes,
            bandwidth_bps: stats.path.bandwidth_estimate,
        }
    }

    /// The link with its carrier's figures as they are now.
    fn sample(&self) -> DirectLink {
        DirectLink {
            stats: self.stats(),
            ..self.link.clone()
        }
    }

    /// What [`TunnelMetrics::link`] reports of it, figures as they are now.
    fn metrics(&self) -> LinkSample {
        let link = &self.link;
        LinkSample {
            kind: link.kind,
            family: link.family,
            via: link.via,
            transport: link.transport,
            peer: link.peer,
            local_ip: link.local_ip,
            same_lan: link.same_lan,
            setup_ms: link.timings.setup_ms,
            stats: self.stats(),
        }
    }
}

#[cfg(feature = "p2p")]
fn micros(time: Duration) -> u64 {
    time.as_micros().min(u128::from(u64::MAX)) as u64
}

/// A direct session's link, forgotten, and its connection let go, when this
/// drops with the session.
#[cfg(feature = "p2p")]
pub(crate) struct LinkGuard {
    diagnostics: Diagnostics,
    id: u64,
}

#[cfg(feature = "p2p")]
impl Drop for LinkGuard {
    fn drop(&mut self) {
        self.diagnostics
            .state()
            .links
            .retain(|live| live.id != self.id);
    }
}

/// A session open on a path, from [`Diagnostics::session_opened`]: its
/// forwarding counts into [`Self::meter`], and dropping it closes the
/// session with the bytes counted, however the forwarding ended.
pub(crate) struct SessionGuard {
    diagnostics: Diagnostics,
    id: u64,
    meter: Arc<ByteMeter>,
}

impl SessionGuard {
    /// Where the session's forwarding counts its bytes.
    pub(crate) fn meter(&self) -> Arc<ByteMeter> {
        self.meter.clone()
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.diagnostics.session_closed(self.id);
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
        let mut events = diagnostics.subscribe();
        let mut first = diagnostics.direct_attempt();
        first.stage(DirectStage::Punching);
        assert_eq!(
            attempt_of(&diagnostics).unwrap().stage,
            DirectStage::Punching
        );
        let second = diagnostics.direct_attempt();
        let seen = attempt_of(&diagnostics).unwrap();
        assert_eq!((seen.stage, seen.in_flight), (DirectStage::Negotiating, 2));
        first.fell_back(FallbackReason::Unreachable);
        assert_eq!(attempt_of(&diagnostics).unwrap().in_flight, 1);
        second.established(Duration::from_millis(40), LinkKind::Lan);
        assert_eq!(attempt_of(&diagnostics), None);
        let direct = diagnostics.snapshot(&endpoint).direct.unwrap();
        assert_eq!(direct.fallbacks, 1);
        assert_eq!(
            direct.last_fallback.as_deref(),
            Some("direct_timeout_or_unreachable")
        );
        // Its stages are steps, delivered live; the history keeps outcomes.
        assert!(matches!(
            kinds(&diagnostics)[..],
            [
                (
                    TunnelEventKind::DirectFallback {
                        stage: DirectStage::Punching,
                        reason: FallbackReason::Unreachable,
                    },
                    1
                ),
                (TunnelEventKind::DirectEstablished { .. }, 1),
            ]
        ));
        // One that ends with neither outcome says so, live, and only it does.
        let mut third = diagnostics.direct_attempt();
        third.stage(DirectStage::Committing);
        drop(third);
        assert_eq!(attempt_of(&diagnostics), None);
        let abandoned: Vec<_> = std::iter::from_fn(|| events.receiver.try_recv().ok())
            .map(|event| event.kind)
            .filter(|kind| matches!(kind, TunnelEventKind::DirectAbandoned { .. }))
            .collect();
        assert_eq!(
            abandoned,
            [TunnelEventKind::DirectAbandoned {
                stage: DirectStage::Committing
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_event_is_aged_when_it_is_read() {
        let diagnostics = Diagnostics::default();
        let mut live = diagnostics.subscribe();
        diagnostics.network_changed(1);
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(live.recv().await.unwrap().unwrap().age_ms, 5_000);
        let mut replayed = diagnostics.subscribe();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(replayed.recv().await.unwrap().unwrap().age_ms, 6_000);
    }

    #[tokio::test]
    async fn subscribers_replay_history_then_follow_every_event_and_learn_of_lag() {
        let diagnostics = Diagnostics::default();
        diagnostics.network_changed(1);
        let mut fast = diagnostics.subscribe();
        let mut slow = diagnostics.subscribe();
        assert_eq!(fast.recv().await.unwrap().unwrap().seq, 1);
        assert_eq!(slow.recv().await.unwrap().unwrap().seq, 1);
        for generation in 2..=140 {
            diagnostics.network_changed(generation);
            let event = fast.recv().await.unwrap().unwrap();
            assert_eq!((event.seq, event.repeats), (generation, 1));
        }
        // Steps reach a subscriber without displacing the history.
        diagnostics.phase(RecoveryPhase::Connecting);
        assert_eq!(
            fast.recv().await.unwrap().unwrap().kind,
            TunnelEventKind::PhaseChanged {
                phase: RecoveryPhase::Connecting
            }
        );
        assert_eq!(
            slow.recv().await.unwrap(),
            Err(EventsLagged { skipped: 12 })
        );
        diagnostics.stop();
        assert_eq!(
            fast.recv().await.unwrap().unwrap().kind,
            TunnelEventKind::Stopped
        );
        assert!(fast.recv().await.is_none());
        let history = kinds(&diagnostics);
        assert!(history.iter().all(|(kind, _)| !kind.is_live_only()));
        assert_eq!(history.last().unwrap().0, TunnelEventKind::Stopped);
    }

    #[tokio::test]
    async fn a_ready_tunnel_announces_a_failure_but_not_its_routine_rechecks() {
        let diagnostics = Diagnostics::default();
        diagnostics.attempt();
        diagnostics.succeeded(Duration::from_millis(30));
        let mut events = diagnostics.subscribe();
        let _ = events.recv().await; // The relay answered.
        for _ in 0..3 {
            diagnostics.attempt();
            diagnostics.phase(RecoveryPhase::Handshake);
            diagnostics.succeeded(Duration::from_millis(30));
        }
        diagnostics.attempt();
        diagnostics.failed(RecoveryFailure::Timeout, Duration::from_secs(1));
        diagnostics.attempt();
        let phases: Vec<_> = std::iter::from_fn(|| events.replay.pop_front())
            .chain(std::iter::from_fn(|| events.receiver.try_recv().ok()))
            .filter_map(|event| match event.kind {
                TunnelEventKind::PhaseChanged { phase } => Some(phase),
                _ => None,
            })
            .collect();
        assert_eq!(phases, [RecoveryPhase::Backoff, RecoveryPhase::Capacity]);
    }

    #[cfg(feature = "port-mapping")]
    #[tokio::test]
    async fn a_stopped_tunnel_reports_its_mapping_release_before_the_stream_ends() {
        let diagnostics = Diagnostics::default();
        let cleanup = diagnostics.defer_close();
        let mut events = diagnostics.subscribe();
        diagnostics.stop();
        diagnostics.port_mapping_changed(PortMappingStatus::new(PortMappingStage::Released, 1));
        drop(cleanup);
        assert!(matches!(
            events.recv().await.unwrap().unwrap().kind,
            TunnelEventKind::PortMappingChanged { .. }
        ));
        assert_eq!(
            events.recv().await.unwrap().unwrap().kind,
            TunnelEventKind::Stopped
        );
        assert!(events.recv().await.is_none());
    }
}
