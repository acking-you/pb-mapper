//! A warm standby on the other path. Beside the session's live connection
//! the viewer keeps a second, ordinary session of the same host, paused,
//! which the host knows as the live one's standby (`Authenticate::
//! standby_for`): it holds no slot, causes no keyframe, shows in no status
//! and carries nothing but stats, probes and pings. When the live
//! connection dies, the standby takes its place: one `Resume`, one IDR, and
//! the room, its display mode and the viewer's control carry on.
//!
//! - Beside a session that runs direct across the internet (punched or
//!   through a port mapping, QUIC), the standby runs on the relay: a path
//!   that dies while the relay lives (UDP blocked, a NAT that rebinds, a
//!   mapping lost) costs about `SILENCE_MIN` instead of a reconnect. A
//!   session within a LAN has none.
//! - Beside a session the relay carries because direct fell back for a
//!   while (it timed out or failed negotiating, or found no path), or
//!   because its direct path died, the standby is direct only
//!   ([`Client::connect_tcp_direct`]), under the client's pauses: once it
//!   commits it takes the session's place at once, which moves the session
//!   off the relay. Direct the relay or the publisher does not offer is
//!   never tried.
//!
//! The live connection gives way when it fails, or when it has read no
//! byte for `max(SILENCE_MIN, 4 × base RTT)` while the standby read a whole
//! message within that time. Neither connection survives a change of this
//! viewer's own network: both are bound to it.
//!
//! The viewer's status, view and video sink stay what they are across a
//! switch; its tunnel metrics follow the live connection, so a
//! `subscribe_metrics` stream taken before a switch ends with it.
use super::{
    ConnectError, CursorSlot, PeerCursorsSlot, ViewerConfig, ViewerReason,
    outbound::{Barrier, Lifecycle},
    session::{Accepted, Ended, Leg, Shared, handshake, run_leg},
};
use crate::{
    Invitation, VideoCodec,
    protocol::{CAPABILITY_ARRIVAL_TIME, CAPABILITY_KEYFRAME_SEQ, CAPABILITY_STANDBY},
    security,
};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{
    Client, ConnectOptions, Connection, DirectDialer, DirectOnlyError, DirectTransport,
    FallbackReason, LinkKind, PathPolicy, TunnelEventKind,
};
use pb_mapper_core::timeout::RetryBackoff;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite};

/// The least a live connection is silent before a standby takes its place.
pub(crate) const SILENCE_MIN: Duration = Duration::from_millis(1500);
/// How many base round trips of silence a slower path is given.
const SILENCE_RTTS: u32 = 4;
/// How often the coordinator looks at the live connection's silence.
const CHECK: Duration = Duration::from_millis(100);
/// How often it looks again whether a standby is wanted.
const WANTED_CHECK: Duration = Duration::from_secs(1);
/// The first wait before a relay standby is opened again, doubling up to
/// `RELAY_RETRY_MAX`.
const RELAY_RETRY: Duration = Duration::from_secs(1);
const RELAY_RETRY_MAX: Duration = Duration::from_secs(30);
/// The first wait before a direct standby is tried again after a failure
/// that paused nothing, doubling up to `DIRECT_RETRY_MAX`: the pauses'.
const DIRECT_RETRY: Duration = Duration::from_secs(10);
const DIRECT_RETRY_MAX: Duration = Duration::from_secs(300);
/// The TLS handshake of a standby, pinning included.
const TLS_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a connection a standby replaced is kept, reading and dropping
/// what still comes, so that its host session ends by the promotion rather
/// than by its close: the host's own read timeout.
const RETIRE_GRACE: Duration = crate::session::HOST_READ_TIMEOUT;
/// Why a session whose task panicked ended.
const SESSION_PANICKED: &str = "the desktop session ended unexpectedly";
const _: () = assert!(
    !RELAY_RETRY.is_zero()
        && RELAY_RETRY.as_nanos() <= RELAY_RETRY_MAX.as_nanos()
        && !DIRECT_RETRY.is_zero()
        && DIRECT_RETRY.as_nanos() <= DIRECT_RETRY_MAX.as_nanos(),
    "RetryBackoff::new's preconditions"
);
const _: () = assert!(
    SILENCE_MIN.as_nanos() > crate::session::STATS_INTERVAL.as_nanos(),
    "a live connection hears the host's Stats inside the silence it is given"
);

/// The tunnel the live connection rides on, which the viewer's metrics read.
pub(super) type ConnectionSlot = Arc<Mutex<Option<Arc<Connection>>>>;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What a viewer opens standbys with: the client, the invitation and the
/// options it connected with, and the live session's ticket.
pub(super) struct Plan {
    pub(super) client: Client,
    pub(super) invitation: Invitation,
    pub(super) options: ConnectOptions,
    pub(super) config: ViewerConfig,
    pub(super) ticket: String,
    #[cfg(test)]
    pub(super) hooks: Hooks,
}
impl Plan {
    /// The plan for a session `accepted` from a host that gave a ticket, of
    /// a viewer that may take more than the relay; `None` for any other.
    pub(super) fn new(base: Option<PlanBase>, accepted: &Accepted) -> Option<Self> {
        let PlanBase {
            client,
            invitation,
            options,
            config,
            #[cfg(test)]
            hooks,
        } = base?;
        #[cfg(test)]
        let any = hooks.relay.is_some() || hooks.fell_back;
        #[cfg(not(test))]
        let any = false;
        if (options.path != PathPolicy::Auto && !any) || !accepted.listed(CAPABILITY_STANDBY) {
            return None;
        }
        Some(Self {
            client,
            invitation,
            options,
            config,
            ticket: accepted.ticket.clone()?,
            #[cfg(test)]
            hooks,
        })
    }
}

/// What `Viewer::connect` keeps for a plan, until the host accepted.
pub(super) struct PlanBase {
    pub(super) client: Client,
    pub(super) invitation: Invitation,
    pub(super) options: ConnectOptions,
    pub(super) config: ViewerConfig,
    #[cfg(test)]
    pub(super) hooks: Hooks,
}

/// Faults and paths a test gives a viewer's standbys.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct Hooks {
    /// Open relay standbys through this client, whatever path the live
    /// connection takes.
    pub(crate) relay: Option<Client>,
    /// Take a relayed live connection as one direct fell back from.
    pub(crate) fell_back: bool,
}

/// Which path a standby takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Relay,
    Direct,
}
impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Relay => "relay",
            Self::Direct => "direct",
        }
    }
}

/// Why a standby took the live connection's place.
#[derive(Clone, Copy, Debug)]
enum Cause {
    /// The live connection failed or closed.
    Transport,
    /// It read nothing for too long, while the standby did.
    Silence,
    /// A direct standby committed beside a relayed session.
    Upgrade,
}
impl Cause {
    fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Silence => "silence",
            Self::Upgrade => "upgrade",
        }
    }
}

/// A standby, opened.
struct Opened {
    leg: Arc<Leg>,
    run: BoxFuture<Ended>,
    accepted: Accepted,
}

/// Why something failed, for the log.
type Detail = Box<dyn std::error::Error + Send + Sync>;

/// Why no standby opened.
enum Missed {
    /// Try again after the kind's next backoff.
    Retry(Detail),
    /// Try again at `until`, or once the network changes.
    Until(Instant),
    /// Direct is not offered: never a direct standby again.
    NotOffered,
    /// The host refused the ticket, or answered otherwise than for this
    /// session: never a standby again.
    Refused(Detail),
}

enum Stage {
    /// None is opening or standing by.
    Idle,
    Opening {
        kind: Kind,
        open: BoxFuture<Result<Opened, Missed>>,
    },
    Ready {
        kind: Kind,
        opened: Opened,
    },
}

/// What came of the standby.
enum Event {
    Opened(Result<Opened, Missed>),
    Ended(Ended),
}

/// The live connection, and where it came from.
struct Live {
    leg: Arc<Leg>,
    run: BoxFuture<Ended>,
    /// The standby kind it took the place of a live connection as, if any.
    promoted: Option<Kind>,
    /// What standby it wants, once it streamed; `None` until then.
    wants: Option<Option<Kind>>,
}

/// The standbys of one session: what is open or opening, and when the next
/// of each kind may be.
struct Standby {
    plan: Plan,
    stage: Stage,
    relay_retry: RetryBackoff,
    direct_retry: RetryBackoff,
    /// When the next of each kind may open.
    relay_at: Instant,
    direct_at: Instant,
    /// Direct is not offered: no direct standby again.
    direct_off: bool,
    /// The host refused a standby, or ended one itself: none again.
    off: bool,
    /// The direct setups' consumer and endpoint, kept across attempts.
    dialer: Option<Arc<DirectDialer>>,
    next_wanted: Instant,
}
impl Standby {
    fn new(plan: Plan, now: Instant) -> Self {
        Self {
            plan,
            stage: Stage::Idle,
            relay_retry: RetryBackoff::new(RELAY_RETRY, RELAY_RETRY_MAX).with_jitter(),
            direct_retry: RetryBackoff::new(DIRECT_RETRY, DIRECT_RETRY_MAX).with_jitter(),
            relay_at: now,
            direct_at: now,
            direct_off: false,
            off: false,
            dialer: None,
            next_wanted: now,
        }
    }

    /// Wait for what comes of the standby; never, while none is opening or
    /// standing by.
    async fn event(stage: &mut Stage) -> Event {
        match stage {
            Stage::Idle => std::future::pending().await,
            Stage::Opening { open, .. } => Event::Opened(open.await),
            Stage::Ready { opened, .. } => Event::Ended((&mut opened.run).await),
        }
    }

    /// The standby, standing by, if it read a whole message within `window`.
    fn fresh(&self, window: Duration, now: Instant) -> bool {
        matches!(&self.stage, Stage::Ready { opened, .. } if opened.leg.quiet_for(now) < window)
    }

    /// What the live connection wants beside it, read once it streams.
    fn wanted(&mut self, live: &Live, shared: &Shared) -> Option<Option<Kind>> {
        if !shared.shared.state.lock().streaming() {
            return None;
        }
        #[cfg(test)]
        {
            let hooks = &self.plan.hooks;
            if hooks.fell_back && live.promoted.is_none() {
                return Some(Some(Kind::Direct));
            }
            if hooks.relay.is_some() && live.promoted != Some(Kind::Relay) {
                return Some(Some(Kind::Relay));
            }
        }
        let connection = live.leg.connection.as_ref()?;
        let metrics = connection.metrics();
        if metrics.sessions.active_direct > 0 {
            // Within a LAN, the relay is no better a path.
            let internet = metrics.link.is_some_and(|link| {
                link.kind != LinkKind::Lan && link.transport == DirectTransport::Quic
            });
            return Some(internet.then_some(Kind::Relay));
        }
        // Relayed: worth a direct standby only when direct missed for a
        // while, or this connection replaced a direct one that died.
        if live.promoted == Some(Kind::Relay) {
            return Some(Some(Kind::Direct));
        }
        let mut transient = metrics.cooldown_skips > 0;
        for event in connection.diagnostics().events {
            if let TunnelEventKind::DirectFallback { reason, .. } = event.kind {
                match reason {
                    FallbackReason::NotOffered => self.direct_off = true,
                    FallbackReason::NegotiationFailed
                    | FallbackReason::NegotiationTimeout
                    | FallbackReason::Unreachable => transient = true,
                    _ => {}
                }
            }
        }
        Some(transient.then_some(Kind::Direct))
    }

    /// Start opening a standby of `kind`, at its next turn.
    fn open(&mut self, kind: Kind, shared: &Arc<Shared>) {
        let mut config = self.plan.config.clone();
        // The preset the viewer has now, which the standby's session starts
        // at.
        config.quality = shared.shared.state.lock().quality;
        let invitation = self.plan.invitation.clone();
        let ticket = self.plan.ticket.clone();
        let codec = shared.codec;
        let session = shared.clone();
        let open: BoxFuture<Result<Opened, Missed>> = match kind {
            Kind::Relay => {
                #[cfg(test)]
                let client = self
                    .plan
                    .hooks
                    .relay
                    .clone()
                    .unwrap_or_else(|| self.plan.client.clone());
                #[cfg(not(test))]
                let client = self.plan.client.clone();
                let options = ConnectOptions {
                    path: PathPolicy::RelayOnly,
                    ..self.plan.options
                };
                let at = self.relay_at;
                Box::pin(async move {
                    tokio::time::sleep_until(at.into()).await;
                    let (socket, connection) = client
                        .connect_tcp(invitation.service.clone(), options)
                        .await
                        .map_err(|failure| Missed::Retry(failure.into()))?;
                    let connection = Arc::new(connection);
                    authenticate(
                        socket,
                        connection,
                        &invitation,
                        ticket,
                        &config,
                        codec,
                        session,
                    )
                    .await
                })
            }
            Kind::Direct => {
                let dialer = match &self.dialer {
                    Some(dialer) => dialer.clone(),
                    None => match self
                        .plan
                        .client
                        .direct_dialer(self.plan.invitation.service.clone(), self.plan.options)
                    {
                        Ok(dialer) => {
                            let dialer = Arc::new(dialer);
                            self.dialer = Some(dialer.clone());
                            dialer
                        }
                        Err(_) => {
                            self.direct_off = true;
                            return;
                        }
                    },
                };
                let client = self.plan.client.clone();
                let at = self.direct_at;
                Box::pin(async move {
                    dialer.wait(at).await;
                    let (socket, connection) = client.connect_tcp_direct(&dialer).await.map_err(
                        |failure| match failure {
                            DirectOnlyError::NotOffered => Missed::NotOffered,
                            DirectOnlyError::Paused { until }
                            | DirectOnlyError::Failed {
                                until: Some(until), ..
                            } => Missed::Until(until),
                            failure => Missed::Retry(failure.into()),
                        },
                    )?;
                    let connection = Arc::new(connection);
                    authenticate(
                        socket,
                        connection,
                        &invitation,
                        ticket,
                        &config,
                        codec,
                        session,
                    )
                    .await
                })
            }
        };
        self.stage = Stage::Opening { kind, open };
    }

    /// No standby of `kind` opened, or the one standing by ended: when the
    /// next may, if ever.
    fn missed(&mut self, kind: Kind, missed: Missed, now: Instant) {
        let outcome = match &missed {
            Missed::Retry(_) => "failed",
            Missed::Until(_) => "paused",
            Missed::NotOffered => "not_offered",
            Missed::Refused(_) => "refused",
        };
        tracing::trace!(
            target: "pb_mapper_desktop::session",
            event = "desktop_viewer_standby",
            route = kind.as_str(),
            outcome,
        );
        match (missed, kind) {
            (Missed::Retry(detail), _) => {
                tracing::debug!(
                    target: "pb_mapper_desktop::session",
                    event = "desktop_viewer_standby_failed",
                    route = kind.as_str(),
                    detail = %detail,
                );
                match kind {
                    Kind::Relay => self.relay_at = now + self.relay_retry.next_delay(),
                    Kind::Direct => self.direct_at = now + self.direct_retry.next_delay(),
                }
            }
            (Missed::Until(until), Kind::Direct) => self.direct_at = until.max(now),
            (Missed::Until(until), Kind::Relay) => self.relay_at = until.max(now),
            (Missed::NotOffered, _) => self.direct_off = true,
            (Missed::Refused(detail), _) => {
                tracing::debug!(
                    target: "pb_mapper_desktop::session",
                    event = "desktop_viewer_standby_refused",
                    detail = %detail,
                );
                self.off = true;
            }
        }
    }
}

/// TLS, pinned to the invitation, then `Authenticate` as the standby of the
/// session `ticket` names: a standby, opened, whose leg has yet to run.
async fn authenticate(
    socket: tokio::net::TcpStream,
    connection: Arc<Connection>,
    invitation: &Invitation,
    ticket: String,
    config: &ViewerConfig,
    codec: VideoCodec,
    session: Arc<Shared>,
) -> Result<Opened, Missed> {
    let name = quinn::rustls::pki_types::ServerName::try_from(security::NAME)
        .map_err(|failure| Missed::Refused(failure.into()))?;
    let tls = security::client(invitation.fingerprint)
        .map_err(|failure| Missed::Retry(failure.into()))?;
    let mut stream = tokio::time::timeout(TLS_TIMEOUT, tls.connect(name, socket))
        .await
        .map_err(|_| Missed::Retry("the standby's TLS handshake timed out".into()))?
        .map_err(|failure| match security::handshake_failure(&failure) {
            Some(_) => Missed::Refused(failure.into()),
            None => Missed::Retry(failure.into()),
        })?;
    let accepted = handshake(&mut stream, invitation.token_text(), config, Some(ticket))
        .await
        .map_err(|failure| match failure {
            // The host restarted, or the pair ended: its ticket is gone.
            ConnectError::Denied | ConnectError::InvalidRequest | ConnectError::Protocol(_) => {
                Missed::Refused(failure.into())
            }
            failure => Missed::Retry(failure.into()),
        })?;
    if accepted.codec != codec {
        return Err(Missed::Refused(
            "the standby's codec is not the session's".into(),
        ));
    }
    let leg = Leg::standby(
        Instant::now(),
        accepted.listed(CAPABILITY_ARRIVAL_TIME),
        accepted.listed(CAPABILITY_KEYFRAME_SEQ),
        Some(connection),
    );
    let run = Box::pin(run_leg(stream, leg.clone(), session));
    Ok(Opened { leg, run, accepted })
}

/// The live connection's silence that gives its place to a standby:
/// `SILENCE_MIN`, or four of its base round trips on a slower path.
fn silence(shared: &Shared) -> Duration {
    let base = shared
        .shared
        .stats
        .lock()
        .host
        .and_then(|host| host.base_rtt_ms)
        .map_or(Duration::ZERO, |ms| Duration::from_millis(ms.into()));
    SILENCE_MIN.max(base * SILENCE_RTTS)
}

/// The standby takes the session: everything the old host session was told
/// or owes means nothing to its own, which hears first the `Resume` that
/// promotes it, then the viewer's choices it has yet to hear.
fn switch(shared: &Shared, leg: &Leg, accepted: &Accepted) {
    let now = Instant::now();
    let viewer = &shared.shared;
    let cap = shared.sink.switch();
    viewer.outbound.restart(
        accepted
            .listed(CAPABILITY_ARRIVAL_TIME)
            .then_some(leg.epoch),
        accepted.listed(CAPABILITY_KEYFRAME_SEQ),
    );
    {
        let mut state = viewer.state.lock();
        state.capabilities.clone_from(&accepted.capabilities);
        if let Some(route) = leg.route() {
            state.route = Some(route);
        }
        state.link = None;
        if state.quality != accepted.quality {
            viewer.outbound.quality(state.quality);
        }
        if state.paused {
            viewer.outbound.lifecycle(Lifecycle::Pause);
        }
        // Under the state lock, as every change of control pushes its
        // barrier. The host carries control over to a standby it promotes.
        if state.control_requested {
            viewer.outbound.barrier(Barrier::Grant);
        } else if state.host.as_ref().is_some_and(|host| host.controlling) {
            viewer.outbound.barrier(Barrier::Revoke);
        }
    }
    if let Some(cap) = cap {
        viewer.outbound.fps_cap(cap);
    }
    {
        let mut cursor = viewer.cursor.lock();
        *cursor = CursorSlot {
            seq: cursor.seq + 1,
            ..CursorSlot::default()
        };
    }
    {
        let mut peers = viewer.peer_cursors.lock();
        *peers = PeerCursorsSlot {
            seq: peers.seq + 1,
            ..PeerCursorsSlot::default()
        };
    }
    viewer.shapes.lock().forget_slots();
    viewer.stats.lock().on_switch(now);
    leg.go_live(&viewer.outbound);
}

/// Ends the session however `drive` ends: returned, dropped with its
/// runtime (polled or not) or unwinding from a panic.
struct Ends(Arc<Shared>);
impl Drop for Ends {
    fn drop(&mut self) {
        // Each step is idempotent and the first stop wins, so after
        // `drive`'s own end this changes nothing; otherwise the consumer
        // exits and the status says stopped. A drop with the runtime reads
        // as a local stop, with no reason; a panic as a transport failure,
        // which a supervisor retries as it did the stall the panic used to
        // leave.
        self.0.cancel.cancel();
        self.0.sink.close();
        if std::thread::panicking() {
            self.0
                .shared
                .mark_stopped(Some(ViewerReason::Transport), SESSION_PANICKED.into());
        } else {
            self.0.shared.mark_stopped(None, String::new());
        }
    }
}

/// What the coordinator does next.
enum Step {
    LiveEnded(Ended),
    /// A replaced connection ended, or its grace ran out.
    Retired,
    Standby(Event),
    Check,
}

/// Run the session on `stream`, its first live connection, with standbys
/// when there is a `plan`, until it ends.
pub(super) async fn drive<S: AsyncRead + AsyncWrite + Send + 'static>(
    stream: S,
    session: Arc<Shared>,
    connection: ConnectionSlot,
    plan: Option<Plan>,
) {
    let started = Instant::now();
    let _ends = Ends(session.clone());
    let leg = Leg::live(connection.lock().clone());
    let mut live = Live {
        run: Box::pin(run_leg(stream, leg.clone(), session.clone())),
        leg,
        promoted: None,
        wants: None,
    };
    let mut standby = plan.map(|plan| Standby::new(plan, started));
    let mut promotions = 0u32;
    let mut retiring: Option<BoxFuture<()>> = None;
    let ended = loop {
        let watching = standby.as_ref().is_some_and(|standby| !standby.off);
        let step = tokio::select! {
            biased;
            ended = &mut live.run => Step::LiveEnded(ended),
            event = async {
                match standby.as_mut() {
                    Some(standby) => Standby::event(&mut standby.stage).await,
                    None => std::future::pending().await,
                }
            } => Step::Standby(event),
            () = async {
                match retiring.as_mut() {
                    Some(retired) => retired.await,
                    None => std::future::pending().await,
                }
            } => Step::Retired,
            () = tokio::time::sleep(CHECK), if watching => Step::Check,
        };
        let now = Instant::now();
        let Some(coordinator) = standby.as_mut() else {
            match step {
                Step::LiveEnded(ended) => break ended,
                Step::Standby(_) | Step::Check | Step::Retired => continue,
            }
        };
        let cause = match step {
            Step::Retired => {
                retiring = None;
                continue;
            }
            Step::LiveEnded(ended) => {
                // A failed connection gives way to a standby that lives; a
                // host that said why, a local stop or a broken protocol
                // ends the session.
                if !session.cancel.is_cancelled()
                    && matches!(ended, Ended::Transport(..))
                    && coordinator.fresh(silence(&session), now)
                {
                    Cause::Transport
                } else {
                    break ended;
                }
            }
            Step::Standby(Event::Opened(Ok(opened))) => {
                let Stage::Opening { kind, .. } = coordinator.stage else {
                    continue;
                };
                tracing::trace!(
                    target: "pb_mapper_desktop::session",
                    event = "desktop_viewer_standby",
                    route = kind.as_str(),
                    outcome = "opened",
                );
                coordinator.stage = Stage::Ready { kind, opened };
                // A committed direct standby beside a relayed session takes
                // its place at once: direct comes first.
                if kind == Kind::Direct {
                    Cause::Upgrade
                } else {
                    continue;
                }
            }
            Step::Standby(Event::Opened(Err(missed))) => {
                if let Stage::Opening { kind, .. } =
                    std::mem::replace(&mut coordinator.stage, Stage::Idle)
                {
                    coordinator.missed(kind, missed, now);
                }
                continue;
            }
            Step::Standby(Event::Ended(ended)) => {
                let Stage::Ready { kind, .. } =
                    std::mem::replace(&mut coordinator.stage, Stage::Idle)
                else {
                    continue;
                };
                let missed = match ended {
                    // The host ended it itself: it ends the session too.
                    Ended::Host(_, message) | Ended::Protocol(message) => {
                        Missed::Refused(message.into())
                    }
                    Ended::Transport(_, message) => Missed::Retry(message.into()),
                    Ended::Local(_) => continue,
                };
                coordinator.missed(kind, missed, now);
                continue;
            }
            Step::Check => {
                let window = silence(&session);
                if matches!(coordinator.stage, Stage::Ready { .. })
                    && live.leg.silent_for(now) >= window
                    && coordinator.fresh(window, now)
                {
                    Cause::Silence
                } else {
                    if matches!(coordinator.stage, Stage::Idle)
                        && now >= coordinator.next_wanted
                        && !coordinator.off
                    {
                        coordinator.next_wanted = now + WANTED_CHECK;
                        if live.wants.is_none() {
                            live.wants = coordinator.wanted(&live, &session);
                        }
                        match live.wants.flatten() {
                            Some(Kind::Direct) if coordinator.direct_off => {}
                            Some(kind) => coordinator.open(kind, &session),
                            None => {}
                        }
                    }
                    continue;
                }
            }
        };
        // The standby takes the live connection's place.
        let Stage::Ready { kind, opened } = std::mem::replace(&mut coordinator.stage, Stage::Idle)
        else {
            continue;
        };
        let outage = match cause {
            Cause::Upgrade => Duration::ZERO,
            Cause::Transport | Cause::Silence => live.leg.silent_for(now),
        };
        let Opened { leg, run, accepted } = opened;
        let old = std::mem::replace(
            &mut live,
            Live {
                run,
                leg: leg.clone(),
                promoted: Some(kind),
                wants: None,
            },
        );
        // A connection that still runs touches nothing of the session from
        // now on, and stays open until its host session ends, which the
        // promotion does: the host hears the `Resume` before the close.
        if !matches!(cause, Cause::Transport) {
            old.leg.retire();
            let run = old.run;
            retiring = Some(Box::pin(async move {
                let _ = tokio::time::timeout(RETIRE_GRACE, run).await;
            }));
        }
        switch(&session, &leg, &accepted);
        *connection.lock() = leg.connection.clone();
        match kind {
            Kind::Relay => coordinator.relay_retry.reset(),
            Kind::Direct => coordinator.direct_retry.reset(),
        }
        coordinator.next_wanted = now;
        promotions += 1;
        tracing::trace!(
            target: "pb_mapper_desktop::session",
            event = "desktop_viewer_promoted",
            cause = cause.as_str(),
            route = kind.as_str(),
            outage_ms = pb_mapper_core::time::saturating_millis(outage),
        );
    };
    let shared = &session.shared;
    let (reason, message) = match ended {
        // The host's close after a local stop is not a failure.
        Ended::Host(..) | Ended::Transport(..) | Ended::Protocol(_)
            if session.cancel.is_cancelled() =>
        {
            (None, String::new())
        }
        ended => ended.into_stop(),
    };
    session.cancel.cancel();
    session.sink.close();
    shared.mark_stopped(reason, message);
    // The first stop recorded, not this one: a local stop after a failure
    // keeps the failure. No reason: stopped here.
    let super::Stopped {
        reason,
        message: detail,
    } = shared.state.lock().stopped.clone().unwrap_or_default();
    let keyframes = shared.stats.lock().host.map_or(0, |host| host.keyframes);
    let local = session.sink.snapshot(Instant::now());
    tracing::info!(
        target: "pb_mapper_desktop::session",
        event = "desktop_viewer_ended",
        reason = ?reason,
        detail = ?detail,
        seconds = started.elapsed().as_secs(),
        keyframes,
        keyframes_requested = local.keyframes_requested,
        backlog_drops = local.backlog_drops,
        promotions,
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        Backend, DesktopContext, Display, EncodedFrame, Host, HostOptions, Input, Pull,
        QualityPreset, VideoFormat, VideoLimits, VideoRate, Viewer,
        test_util::{status_until, until_within},
    };
    use pb_mapper_testkit::{Relay, Shape, ShapedLink, admin_credential, shaped_link};
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    };

    /// When a backend encoded each IDR.
    #[derive(Default)]
    pub(crate) struct Keyframes(StdMutex<Vec<Instant>>);
    impl Keyframes {
        pub(crate) fn count(&self) -> usize {
            self.0.lock().unwrap().len()
        }
        /// Those encoded from `from` up to `to`.
        pub(crate) fn between(&self, from: Instant, to: Instant) -> usize {
            let at = self.0.lock().unwrap();
            at.iter().filter(|at| **at >= from && **at < to).count()
        }
    }

    /// A 1080p H.264 desktop whose units are 64 bytes each, noting when it
    /// encoded each IDR.
    pub(crate) struct Counting(pub(crate) Arc<Keyframes>);
    impl Backend for Counting {
        fn display(&mut self) -> crate::Result<Display> {
            Ok(Display {
                id: 1,
                name: "standby".into(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            })
        }
        fn capture(&mut self) -> crate::Result<Vec<u8>> {
            Ok(vec![1])
        }
        fn capture_video(
            &mut self,
            _: &DesktopContext,
            codec: VideoCodec,
            keyframe: bool,
            _: VideoRate,
        ) -> crate::Result<Option<EncodedFrame>> {
            if keyframe {
                self.0.0.lock().unwrap().push(Instant::now());
            }
            Ok(Some(EncodedFrame {
                format: VideoFormat {
                    codec,
                    width: 1920,
                    height: 1080,
                    keyframe,
                    ..VideoFormat::default()
                },
                bytes: vec![if keyframe { 0x65 } else { 0x41 }; 64],
            }))
        }
        fn input(&mut self, _: &Input) -> crate::Result<()> {
            Ok(())
        }
        fn release(&mut self) {}
    }

    /// When a renderer presented each unit, and when the unit was read.
    #[derive(Default)]
    pub(crate) struct Shown {
        pub(crate) units: StdMutex<Vec<(Instant, Instant, bool, bool)>>,
    }
    impl Shown {
        /// The first unit read after `after`: when it was presented, and
        /// whether it was a keyframe after a discontinuity.
        pub(crate) fn first_read_after(&self, after: Instant) -> Option<(Instant, bool)> {
            self.units
                .lock()
                .unwrap()
                .iter()
                .find(|(_, read, _, _)| *read > after)
                .map(|(shown, _, keyframe, discontinuity)| (*shown, *keyframe && *discontinuity))
        }
    }

    /// A renderer that presents every unit at once, noting each.
    pub(crate) fn render(
        viewer: &Viewer,
        stop: Arc<AtomicBool>,
    ) -> (Arc<Shown>, std::thread::JoinHandle<()>) {
        let consumer = viewer.video_sink().attach().unwrap();
        let shown = Arc::new(Shown::default());
        let log = shown.clone();
        let thread = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                if let Pull::Unit(unit) = consumer.next(Duration::from_millis(20), 4 << 20) {
                    consumer.presented(unit.generation, unit.frame_id);
                    log.units.lock().unwrap().push((
                        Instant::now(),
                        unit.received_at,
                        unit.keyframe,
                        unit.discontinuity,
                    ));
                }
            }
        });
        (shown, thread)
    }

    /// A relay, a host of `Counting` that allows control, and two shaped
    /// links to the relay: the live session's, and its standby's.
    pub(crate) struct Pair {
        pub(crate) host: Host,
        pub(crate) keyframes: Arc<Keyframes>,
        pub(crate) live: Arc<Shape>,
        pub(crate) clients: [Client; 2],
        _links: [ShapedLink; 2],
        _relay: Relay,
    }
    impl Pair {
        pub(crate) async fn start(name: &str) -> Self {
            let relay = Relay::start(&format!("desktop-{name}")).await;
            let client = |addr: std::net::SocketAddr| {
                Client::from_credential(addr.to_string(), admin_credential(), true, None)
            };
            let keyframes = Arc::new(Keyframes::default());
            let counted = keyframes.clone();
            let mut options = HostOptions::new(format!("desktop:{name}"));
            options.allow_control = true;
            options.allow_direct = false;
            let host = Host::builder(options)
                .backend(move || Ok(Counting(counted.clone())))
                .start(&client(relay.addr()))
                .await
                .unwrap();
            let live = Arc::new(Shape::default());
            let links = [
                shaped_link(relay.addr(), live.clone()).await,
                shaped_link(relay.addr(), Arc::new(Shape::default())).await,
            ];
            Self {
                host,
                keyframes,
                live,
                clients: [client(links[0].addr()), client(links[1].addr())],
                _links: links,
                _relay: relay,
            }
        }
        /// A viewer through the first link, relay only, with a standby
        /// through the second when `standby`.
        pub(crate) async fn viewer(&self, standby: bool) -> Viewer {
            let options = ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..ConnectOptions::default()
            };
            let mut config = ViewerConfig::new(vec![VideoCodec::H264]);
            config.limits = VideoLimits::for_viewer(60.0);
            config.quality = QualityPreset::Balanced;
            let hooks = Hooks {
                relay: standby.then(|| self.clients[1].clone()),
                fell_back: false,
            };
            Viewer::connect_with_hooks(
                &self.clients[0],
                self.host.invitation(),
                options,
                config,
                hooks,
            )
            .await
            .unwrap()
        }
        /// The host's relayed sessions: a viewer's live one and its
        /// standby's.
        pub(crate) fn relayed(&self) -> u64 {
            self.host.metrics().sessions.active_relay
        }
    }

    /// A live connection that goes silent while the relay lives gives way to
    /// the standby on the other link within the silence it is allowed and
    /// an IDR: the picture comes back, from a keyframe after a
    /// discontinuity, before the read timeout would have ended the session,
    /// control comes back with it, and the host still holds one viewer.
    /// While the live connection is healthy, its standby takes nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_standby_takes_over_a_silent_session_on_the_other_path() {
        let pair = Pair::start("standby-silent").await;
        let viewer = pair.viewer(true).await;
        let stop = Arc::new(AtomicBool::new(false));
        let (shown, renderer) = render(&viewer, stop.clone());
        status_until(&viewer, |status| status.fresh).await;
        viewer.request_control(true).unwrap();
        status_until(&viewer, |status| status.controlling && status.fresh).await;
        until_within(Duration::from_secs(10), || pair.relayed() == 2).await;
        let started = pair.host.status().viewers[0].started_unix_ms;
        tokio::time::sleep(SILENCE_MIN * 2).await;
        assert_eq!(pair.host.status().viewers[0].started_unix_ms, started);

        let blocked = Instant::now();
        pair.live.blocked.store(true, Ordering::Relaxed);
        until_within(Duration::from_secs(4), || {
            shown.first_read_after(blocked + SILENCE_MIN).is_some()
        })
        .await;
        let (back, keyframe) = shown.first_read_after(blocked + SILENCE_MIN).unwrap();
        assert!(keyframe, "the first picture after the switch is an IDR");
        assert!(
            back.duration_since(blocked) < super::super::session::VIEWER_HEADER_TIMEOUT,
            "{:?}",
            back.duration_since(blocked)
        );
        let status = status_until(&viewer, |status| status.controlling && status.fresh).await;
        assert_ne!(status.state, crate::ViewerState::Stopped);
        let host = pair.host.status();
        assert_eq!(host.viewers.len(), 1);
        assert_ne!(host.viewers[0].started_unix_ms, started);
        viewer.stop().await.unwrap();
        stop.store(true, Ordering::Release);
        renderer.join().unwrap();
        pair.host.stop().await.unwrap();
    }

    /// A relayed session whose direct fell back for a while gets a direct
    /// standby, which takes its place as soon as it commits: the session
    /// runs direct from then, its picture back from an IDR.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_committed_direct_standby_takes_a_relayed_sessions_place() {
        let relay = Relay::start_with_direct(
            "desktop-standby-upgrade",
            Some(pb_mapper_testkit::loopback_direct()),
        )
        .await;
        let client =
            Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
        let keyframes = Arc::new(Keyframes::default());
        let counted = keyframes.clone();
        let mut options = HostOptions::new("desktop:standby-upgrade");
        options.allow_control = true;
        let host = Host::builder(options)
            .backend(move || Ok(Counting(counted.clone())))
            .start(&client)
            .await
            .unwrap();
        let viewer = Viewer::connect_with_hooks(
            &client,
            host.invitation(),
            ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..ConnectOptions::default()
            },
            ViewerConfig::new(vec![VideoCodec::H264]),
            Hooks {
                relay: None,
                fell_back: true,
            },
        )
        .await
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (_shown, renderer) = render(&viewer, stop.clone());
        let relayed = status_until(&viewer, |status| status.fresh).await;
        assert_eq!(relayed.stats.path, "relay");
        let direct = status_until(&viewer, |status| {
            status.fresh && status.stats.path == "direct"
        })
        .await;
        assert_ne!(direct.state, crate::ViewerState::Stopped);
        assert_eq!(host.status().viewers.len(), 1);
        assert_eq!(viewer.metrics().unwrap().sessions.active_direct, 1);
        viewer.stop().await.unwrap();
        stop.store(true, Ordering::Release);
        renderer.join().unwrap();
        host.stop().await.unwrap();
    }
}
