//! The recording state behind a tunnel's [`Diagnostics`](super::Diagnostics),
//! and the bookkeeping every report goes through.

use super::events::{EVENT_CAPACITY, Event};
#[cfg(feature = "p2p")]
use super::link::LiveLink;
use super::metrics::{Bucket, BucketBytes};
use super::{
    DirectStage, LanCounts, PathDiagnostics, PathTraffic, PortMappingDiagnostics, RecoveryFailure,
    RecoveryPhase, RelayHealth, SessionCounts, TunnelEventKind, TunnelMetrics, ms,
};
use crate::relay::RelayTransport;
use pb_mapper_protocol::direct::{DirectTransport, LinkSummary, SessionPath};
use pb_mapper_protocol::forward::ByteMeter;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tokio::time::{Duration, Instant};

/// How far a tunnel is from the end of its event stream.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Lifecycle {
    Running,
    /// Stopped, while a cleanup that reports may still be running.
    Stopping,
    /// The stream has ended.
    Closed,
}

/// A session open now: its bytes are read from its meter until it closes.
pub(super) struct OpenSession {
    pub(super) path: SessionPath,
    pub(super) link: Option<LinkSummary>,
    /// Where its bytes count, and whether it is a direct session over TCP:
    /// decided once, as it opens, so that closing undoes exactly that.
    pub(super) bucket: Bucket,
    pub(super) tcp: bool,
    pub(super) opened: Instant,
    pub(super) meter: Arc<ByteMeter>,
}

impl OpenSession {
    /// A session opening now on `path`, a direct one across `link`.
    pub(super) fn new(path: SessionPath, link: Option<LinkSummary>, meter: Arc<ByteMeter>) -> Self {
        Self {
            path,
            link,
            bucket: Bucket::of(path, link.map(|link| link.kind)),
            tcp: path == SessionPath::Direct
                && link.is_some_and(|link| link.transport == DirectTransport::Tcp),
            opened: Instant::now(),
            meter,
        }
    }
}

pub(super) struct State {
    pub(super) lifecycle: Lifecycle,
    pub(super) started: Instant,
    /// Cleanups that hold the stream open past a stop.
    pub(super) cleanups: usize,
    /// Present while someone subscribes: a tunnel nobody listens to sends
    /// nothing, and records no live-only event.
    pub(super) stream: Option<broadcast::Sender<Event>>,
    /// Bumped on every recorded event, whoever subscribes, so a metrics
    /// stream samples a change at once; dropped when the event stream ends,
    /// which ends the metrics streams too.
    pub(super) revision: Option<watch::Sender<u64>>,
    /// The counters behind [`TunnelDiagnostics::direct`] and
    /// [`TunnelMetrics::sessions`].
    ///
    /// [`TunnelDiagnostics::direct`]: super::TunnelDiagnostics::direct
    pub(super) direct: Option<PathDiagnostics>,
    pub(super) sessions: HashMap<u64, OpenSession>,
    pub(super) next_session: u64,
    /// The bytes of the sessions that have closed.
    pub(super) closed: BucketBytes,
    /// The smoothed round trip of control exchanges with the relay.
    pub(super) relay_rtt: Option<Duration>,
    /// Until when sessions skip direct after a fallback, and how many have.
    pub(super) cooldown: Option<Instant>,
    pub(super) cooldown_skips: u64,
    /// Whether direct setups try a shared local network first.
    pub(super) lan_first: bool,
    /// What they ask a shared local network to carry.
    pub(super) lan_transport: crate::sdk::LanTransport,
    pub(super) port_mapping: Option<PortMappingDiagnostics>,
    pub(super) phase: RecoveryPhase,
    /// The phase last announced to subscribers. A ready tunnel's routine
    /// re-checks pass through the attempt phases without being announced.
    pub(super) announced: RecoveryPhase,
    pub(super) attempts: u64,
    pub(super) failures: u64,
    pub(super) failure: Option<RecoveryFailure>,
    pub(super) success: Option<Instant>,
    pub(super) latency: Option<Duration>,
    pub(super) retry: Option<Instant>,
    /// Whether the last relay attempt succeeded, so only a change is recorded.
    pub(super) ready: bool,
    /// The network generation last seen, so observers of one change record
    /// it once and a worker starting on an old one records nothing.
    pub(super) network: Option<u64>,
    /// The stage and start of the latest direct attempt still in flight.
    pub(super) attempt: Option<(DirectStage, Instant)>,
    pub(super) in_flight: u32,
    /// Direct sessions open now, oldest first, each held only while it is:
    /// the connection is a clone, and Quinn has no weak one.
    #[cfg(feature = "p2p")]
    pub(super) links: Vec<LiveLink>,
    #[cfg(feature = "p2p")]
    pub(super) next_link: u64,
    pub(super) seq: u64,
    pub(super) events: VecDeque<Event>,
}

impl State {
    /// A tunnel starting now, with nothing recorded yet.
    pub(super) fn new() -> Self {
        Self {
            lifecycle: Lifecycle::Running,
            started: Instant::now(),
            cleanups: 0,
            stream: None,
            revision: Some(watch::channel(0).0),
            direct: None,
            sessions: HashMap::new(),
            next_session: 0,
            closed: BucketBytes::default(),
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
        }
    }

    pub(super) fn record(&mut self, kind: TunnelEventKind) {
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
    pub(super) fn running(&self) -> bool {
        self.lifecycle == Lifecycle::Running
    }

    pub(super) fn enter(&mut self, phase: RecoveryPhase) {
        self.phase = phase;
        let attempt_step = match phase {
            RecoveryPhase::Capacity
            | RecoveryPhase::Resolving
            | RecoveryPhase::Connecting
            | RecoveryPhase::Handshake => true,
            RecoveryPhase::Starting
            | RecoveryPhase::Ready
            | RecoveryPhase::Backoff
            | RecoveryPhase::Stopped => false,
        };
        let recheck = self.ready && attempt_step;
        if !recheck && self.announced != phase {
            self.announced = phase;
            self.record(TunnelEventKind::PhaseChanged { phase });
        }
    }

    pub(super) fn close_if_drained(&mut self) {
        if self.lifecycle == Lifecycle::Stopping && self.cleanups == 0 {
            self.record(TunnelEventKind::Stopped);
            self.lifecycle = Lifecycle::Closed;
            self.stream = None;
            self.revision = None;
        }
    }

    /// Whether the event stream, and with it every metrics stream, has
    /// ended.
    pub(super) fn stream_ended(&self) -> bool {
        self.revision.is_none()
    }

    /// Application bytes by path: the closed sessions', and so far the
    /// open ones'.
    pub(super) fn traffic(&self) -> PathTraffic {
        let mut bytes = self.closed;
        for session in self.sessions.values() {
            bytes.add(session.bucket, session.meter.totals());
        }
        bytes.traffic()
    }

    pub(super) fn metrics(&self, transport: RelayTransport, now: Instant) -> TunnelMetrics {
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
