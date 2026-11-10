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
#[cfg(feature = "p2p")]
use pb_mapper_protocol::direct::DirectTransport;
use pb_mapper_protocol::direct::{LinkSummary, SessionPath};
use pb_mapper_protocol::forward::ByteMeter;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};
use tokio::time::{Duration, Instant};

mod events;
mod guards;
mod link;
mod metrics;
mod port_mapping;
mod report;
mod state;
use events::STREAM_CAPACITY;
pub use events::{
    DirectStage, EventsLagged, FallbackReason, LanTcpMiss, RecoveryFailure, RecoveryPhase,
    TunnelEvent, TunnelEventKind, TunnelEvents,
};
#[cfg(feature = "port-mapping")]
pub(crate) use guards::DeferClose;
pub(crate) use guards::SessionGuard;
#[cfg(feature = "p2p")]
pub(crate) use guards::{DirectAttemptGuard, LinkGuard};
#[cfg(feature = "p2p")]
pub(crate) use link::LinkProbe;
#[cfg(feature = "p2p")]
use link::LiveLink;
pub use link::{
    AddressFamily, CandidateCounts, DirectLink, InterfaceTier, LanEvidence, LinkKind, LinkStats,
    LinkVia, SetupTimings, StunOutcome,
};
use metrics::Bucket;
pub(crate) use metrics::sample_period;
pub use metrics::{
    ByteCount, LanCounts, LinkSample, PathTraffic, RelayHealth, SessionCounts, TunnelMetrics,
    TunnelMetricsStream,
};
pub use port_mapping::{
    PortMappingDiagnostics, PortMappingProtocol, PortMappingStage, PortMappingStatus,
};
pub use report::{DirectAttempt, PathDiagnostics, TunnelDiagnostics};
use state::{Lifecycle, OpenSession, State};

/// Fold `sample` into the smoothed round trip `smoothed`, as TCP does
/// (RFC 6298): an eighth of each new sample.
pub(crate) fn smooth(smoothed: Option<Duration>, sample: Duration) -> Duration {
    match smoothed {
        None => sample,
        Some(smoothed) => (smoothed * 7 + sample) / 8,
    }
}

/// How many open direct sessions a tunnel keeps the link of. A provider holds
/// no more sessions than that; a consumer has no such cap, and the newest is
/// what is shown.
#[cfg(feature = "p2p")]
const MAX_LIVE_LINKS: usize = 64;
#[cfg(feature = "p2p")]
const _: () = assert!(
    MAX_LIVE_LINKS >= crate::direct::MAX_PROVIDER_SESSIONS,
    "a provider's open sessions all keep their link"
);

#[derive(Clone)]
pub(crate) struct Diagnostics(Arc<Mutex<State>>);

impl Default for Diagnostics {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State::new())))
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
            crate::sdk::PortMapping::Always | crate::sdk::PortMapping::Off => {
                PortMappingStage::WaitingForDirect
            }
        };
        self.state().port_mapping.get_or_insert_with(|| {
            PortMappingDiagnostics::new(PortMappingStatus::new(stage, network_generation))
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
            .get_or_insert_with(|| PortMappingDiagnostics::new(status.clone()));
        match status.stage {
            PortMappingStage::Requesting => mapping.attempts += 1,
            PortMappingStage::Renewing => mapping.renewals += 1,
            PortMappingStage::Failed | PortMappingStage::ReleaseFailed => mapping.failures += 1,
            PortMappingStage::Standby
            | PortMappingStage::WaitingForDirect
            | PortMappingStage::Discovering
            | PortMappingStage::GatewayFound
            | PortMappingStage::Discovered
            | PortMappingStage::Active
            | PortMappingStage::Expired
            | PortMappingStage::Retrying
            | PortMappingStage::NetworkChanged
            | PortMappingStage::Releasing
            | PortMappingStage::Released
            | PortMappingStage::ReleaseSent
            | PortMappingStage::Cancelled
            | PortMappingStage::Stopped => {}
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
        let session = OpenSession::new(path, link, meter.clone());
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
        if session.bucket == Bucket::DirectLan {
            p.lan_sessions += 1;
            p.active_lan += 1;
        }
        if session.tcp {
            p.lan_tcp_sessions += 1;
            p.active_lan_tcp += 1;
        }
        state.sessions.insert(id, session);
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
        state.closed.add(session.bucket, totals);
        let p = state.direct.get_or_insert_default();
        match session.path {
            SessionPath::Direct => p.active_direct = p.active_direct.saturating_sub(1),
            SessionPath::Relay => p.active_relay = p.active_relay.saturating_sub(1),
        }
        if session.bucket == Bucket::DirectLan {
            p.active_lan = p.active_lan.saturating_sub(1);
        }
        if session.tcp {
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
        if state.links.len() == MAX_LIVE_LINKS {
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
            sdk_version: crate::sdk::SDK_VERSION,
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

#[cfg(test)]
mod tests {
    use super::events::EVENT_CAPACITY;
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

    /// A session is classed once, as it opens, and closing it takes back
    /// exactly what opening it counted, its bytes kept on its path.
    #[tokio::test]
    async fn a_closed_session_leaves_the_counts_it_entered() {
        use pb_mapper_protocol::direct::DirectTransport;
        let diagnostics = Diagnostics::default();
        let endpoint = RelayEndpoint::shared("127.0.0.1:1");
        let link = |kind, transport| {
            Some(LinkSummary {
                kind,
                family: AddressFamily::Ipv4,
                transport,
            })
        };
        let lan_tcp = diagnostics.session_opened(
            SessionPath::Direct,
            link(LinkKind::Lan, DirectTransport::Tcp),
        );
        let internet = diagnostics.session_opened(
            SessionPath::Direct,
            link(LinkKind::Internet, DirectTransport::Quic),
        );
        lan_tcp.meter().add_sent(5);
        internet.meter().add_sent(9);
        let open = diagnostics.snapshot(&endpoint);
        let paths = open.direct.unwrap();
        assert_eq!(
            (paths.active_lan_tcp, paths.active_lan, paths.active_direct),
            (1, 1, 2)
        );
        assert_eq!((open.traffic.lan.sent, open.traffic.direct.sent), (5, 14));
        drop(lan_tcp);
        drop(internet);
        let closed = diagnostics.snapshot(&endpoint);
        let paths = closed.direct.unwrap();
        assert_eq!(
            (paths.active_lan_tcp, paths.active_lan, paths.active_direct),
            (0, 0, 0)
        );
        assert_eq!(
            (
                paths.lan_tcp_sessions,
                paths.lan_sessions,
                paths.direct_sessions
            ),
            (1, 1, 2)
        );
        assert_eq!(closed.traffic, open.traffic);
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
