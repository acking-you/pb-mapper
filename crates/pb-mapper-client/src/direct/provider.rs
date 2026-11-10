use super::{
    conn::{DirectConn, QuicGuard, forward_direct},
    endpoint::{Arrival, Discovered, Network},
    grant_fits,
    lan::Assessment,
    link::{counts, direct_link, first_answers, lan_missed, log_path, stun_timings, summary},
    reports::{Latest, SessionReporter, Sink},
    tcp,
};
use crate::{
    diagnostics::{CandidateCounts, Diagnostics, LinkKind, SetupTimings, ms},
    server::LocalControlWrite,
};
use pb_mapper_core::config::ResolvedAddrs;
use pb_mapper_protocol::{
    direct::*,
    forward::{ByteMeter, StreamForward},
};
use std::{
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::mpsc,
    task::{JoinError, JoinSet},
    time::{Instant, error::Elapsed},
};
use tokio_util::sync::CancellationToken;
use uni_stream::stream::StreamProvider;

/// Coordination events queued for the actor; a full queue drops them.
const ENVELOPE_QUEUE: usize = 128;
/// Coordination events queued for one session; a full queue drops them.
const SESSION_EVENTS: usize = 8;
/// Setups and committed sessions one registration runs at once.
pub(crate) const MAX_SESSIONS: usize = 64;
/// Accepted connections still reading the grant they claim.
const MAX_ARRIVALS: usize = 64;
/// How long an accepted connection has to name its grant.
const BIND_TIMEOUT: Duration = Duration::from_secs(2);
/// How often reports no route took are offered again.
const REPORT_RETRY: Duration = Duration::from_millis(500);
/// Control connections reports may go out on.
const MAX_ROUTES: usize = 64;
/// Sessions whose newest report is kept for another try.
const MAX_UNSENT: usize = 1024;

type Reply = mpsc::Sender<LocalControlWrite>;
struct Envelope {
    event: Option<DirectEvent>,
    reply: Reply,
}
pub(crate) struct Provider {
    tx: mpsc::Sender<Envelope>,
    active: Arc<AtomicUsize>,
    shutdown: CancellationToken,
    done: CancellationToken,
    task: tokio_util::task::AbortOnDropHandle<()>,
}
/// A connection that claimed a session, with the binding it presented.
struct Incoming {
    conn: DirectConn,
    tag: [u8; 32],
}
enum SessionEvent {
    Granted,
    Incoming(Incoming),
    Relay,
    Cancel,
}
impl Provider {
    pub fn spawn<L: StreamProvider + Send + 'static>(
        local: ResolvedAddrs,
        key: Arc<str>,
        is_datagram: bool,
        shutdown: CancellationToken,
        idle: Option<Duration>,
        diagnostics: Diagnostics,
        policy: crate::direct_policy::DirectPolicy,
    ) -> Arc<Self>
    where
        L::Item: StreamForward,
    {
        let (tx, rx) = mpsc::channel(ENVELOPE_QUEUE);
        let active = Arc::new(AtomicUsize::new(0));
        let done = CancellationToken::new();
        let done_guard = done.clone().drop_guard();
        let actor = Actor {
            key,
            is_datagram,
            ctx: SessionContext {
                local,
                idle,
                diagnostics,
                active: active.clone(),
                reports: Arc::default(),
                policy,
            },
            spawn_session: spawn_session::<L>,
            network: None,
            sessions: HashMap::new(),
            tasks: JoinSet::new(),
            arrivals: JoinSet::new(),
        };
        let actor_shutdown = shutdown.clone();
        let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            let _done_guard = done_guard;
            actor.run(rx, actor_shutdown).await;
        }));
        Arc::new(Self {
            tx,
            active,
            shutdown,
            done,
            task,
        })
    }
    pub async fn stop(&self) {
        self.shutdown.cancel();
        self.done.cancelled().await;
    }

    pub fn event(&self, event: DirectEvent, reply: Reply) {
        let _ = self.tx.try_send(Envelope {
            event: Some(event),
            reply,
        });
    }
    pub fn connected(&self, reply: Reply) {
        let _ = self.tx.try_send(Envelope { event: None, reply });
    }
    pub fn has_active(&self) -> bool {
        self.active.load(Ordering::Acquire) != 0 && !self.task.is_finished()
    }
}

/// What every session of one registration shares.
#[derive(Clone)]
struct SessionContext {
    /// The local service, dialed once a session commits.
    local: ResolvedAddrs,
    idle: Option<Duration>,
    diagnostics: Diagnostics,
    /// Committed sessions, for [`Provider::has_active`].
    active: Arc<AtomicUsize>,
    reports: Arc<ProviderReports>,
    policy: crate::direct_policy::DirectPolicy,
}

/// An admitted offer: what its session starts from.
struct Offer {
    network: Arc<Network>,
    grant: SessionGrant,
    stun_addr: SocketAddr,
    /// The control connection the offer came on, which answers it.
    reply: Reply,
    events: mpsc::Receiver<SessionEvent>,
}

/// Starts an admitted offer's session: the one part of the actor that knows
/// the local service's stream type, so that the rest is built once.
type SpawnSession = fn(&mut JoinSet<SessionId>, SessionContext, Offer);

fn spawn_session<L: StreamProvider + Send + 'static>(
    tasks: &mut JoinSet<SessionId>,
    ctx: SessionContext,
    offer: Offer,
) where
    L::Item: StreamForward,
{
    let id = offer.grant.id;
    let session = session::<L>(ctx, offer);
    tasks.spawn(async move {
        if let Err(error) = session.await {
            tracing::debug!(%error, "provider direct session ended");
        }
        id
    });
}

/// An accepted connection's claim, once it named its grant or timed out.
type Bound = Result<io::Result<(SessionId, Incoming)>, Elapsed>;

/// What the actor does with a coordination event.
enum Dispatch {
    /// Admit an offer, and start its session.
    Offer {
        grant: SessionGrant,
        stun_addr: SocketAddr,
    },
    /// Pass an event to the session it names.
    Route(SessionId, SessionEvent),
    /// Nothing: the envelope only says which control connection is live, or
    /// carries an event meant for a consumer.
    Ignore,
}

fn dispatch(event: Option<DirectEvent>) -> Dispatch {
    match event {
        Some(DirectEvent::Offer { grant, stun_addr }) => Dispatch::Offer { grant, stun_addr },
        Some(DirectEvent::Granted { grant, .. }) => {
            Dispatch::Route(grant.id, SessionEvent::Granted)
        }
        Some(DirectEvent::RelaySelect { id }) => Dispatch::Route(id, SessionEvent::Relay),
        Some(DirectEvent::Cancel { id }) => Dispatch::Route(id, SessionEvent::Cancel),
        None
        | Some(
            DirectEvent::Ready(_)
            | DirectEvent::LegacyRelay { .. }
            | DirectEvent::Rejected { .. }
            | DirectEvent::RelaySelected { .. }
            | DirectEvent::Pong,
        ) => Dispatch::Ignore,
    }
}

/// One registration's direct sessions: the endpoint, opened for the first
/// offer admitted, and every session from setup to its end.
struct Actor {
    key: Arc<str>,
    is_datagram: bool,
    ctx: SessionContext,
    spawn_session: SpawnSession,
    network: Option<Arc<Network>>,
    /// Every setup and committed session, keyed by grant: the per-registration
    /// session cap counts committed data, not only pending setup.
    sessions: HashMap<SessionId, mpsc::Sender<SessionEvent>>,
    tasks: JoinSet<SessionId>,
    arrivals: JoinSet<Bound>,
}

impl Actor {
    async fn run(mut self, mut rx: mpsc::Receiver<Envelope>, shutdown: CancellationToken) {
        // Only polled while a report is undelivered, so it costs nothing otherwise.
        let mut report_retry = tokio::time::interval(REPORT_RETRY);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = report_retry.tick(), if self.ctx.reports.has_unsent() => {
                    self.ctx.reports.retry();
                }
                envelope = rx.recv() => {
                    let Some(Envelope { event, reply }) = envelope else { break };
                    self.envelope(event, reply);
                }
                arrival = accept(self.network.as_deref()) => {
                    if let Some(arrival) = arrival {
                        self.arrival(arrival);
                    }
                }
                Some(result) = self.arrivals.join_next(), if !self.arrivals.is_empty() => {
                    self.bound(result);
                }
                Some(result) = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    if let Ok(id) = result {
                        self.sessions.remove(&id);
                    }
                }
            }
        }
        self.tasks.shutdown().await;
        self.arrivals.shutdown().await;
        if let Some(network) = self.network {
            network.shutdown().await;
        }
    }

    /// Every envelope names a live route for reports, whatever it carries.
    fn envelope(&mut self, event: Option<DirectEvent>, reply: Reply) {
        self.ctx.reports.connected(reply.clone());
        match dispatch(event) {
            Dispatch::Offer { grant, stun_addr } => self.offer(grant, stun_addr, reply),
            Dispatch::Route(id, event) => self.route(id, event),
            Dispatch::Ignore => {}
        }
    }

    /// Start a session for an offer, unless one runs for its grant or the
    /// offer is refused.
    fn offer(&mut self, grant: SessionGrant, stun_addr: SocketAddr, reply: Reply) {
        if self.sessions.contains_key(&grant.id) {
            return;
        }
        let (port_mapping, diagnostics) = (self.ctx.policy.port_mapping, &self.ctx.diagnostics);
        let admitted = admit_offer(
            &grant,
            &self.key,
            self.is_datagram,
            self.sessions.len(),
            &mut self.network,
            &reply,
            || Network::new(true, port_mapping, diagnostics),
        );
        let Some(network) = admitted else { return };
        let (tx, events) = mpsc::channel(SESSION_EVENTS);
        self.sessions.insert(grant.id, tx);
        let offer = Offer {
            network,
            grant,
            stun_addr,
            reply,
            events,
        };
        (self.spawn_session)(&mut self.tasks, self.ctx.clone(), offer);
    }

    /// Best effort: a session whose queue is full, or that ended, misses it.
    fn route(&self, id: SessionId, event: SessionEvent) {
        if let Some(tx) = self.sessions.get(&id) {
            let _ = tx.try_send(event);
        }
    }

    fn arrival(&mut self, arrival: Arrival) {
        if self.arrivals.len() >= MAX_ARRIVALS {
            if let Arrival::Quic(connection) = arrival {
                connection.close(0u32.into(), b"session capacity");
            }
            return;
        }
        self.arrivals
            .spawn(tokio::time::timeout(BIND_TIMEOUT, session_bind(arrival)));
    }

    /// A connection named its grant: it goes to that grant's session.
    fn bound(&self, result: Result<Bound, JoinError>) {
        if let Ok(Ok(Ok((id, incoming)))) = result {
            self.route(id, SessionEvent::Incoming(incoming));
        }
    }
}

/// The next connection the endpoint accepted; never, until it is open.
async fn accept(network: Option<&Network>) -> Option<Arrival> {
    match network {
        Some(network) => network.accept().await,
        None => std::future::pending().await,
    }
}

/// Validate an offer and lazily create the shared endpoint with `open`, or
/// reject it.
fn admit_offer(
    grant: &SessionGrant,
    key: &str,
    is_datagram: bool,
    sessions: usize,
    network: &mut Option<Arc<Network>>,
    reply: &Reply,
    open: impl FnOnce() -> io::Result<Arc<Network>>,
) -> Option<Arc<Network>> {
    let reject = |reason: String| {
        send(
            reply,
            DirectProviderMessage::Reject {
                id: grant.id,
                reason,
            },
        );
        None
    };
    if sessions >= MAX_SESSIONS || !grant.consumer.valid() || !grant_fits(grant, key, is_datagram) {
        return reject("invalid offer or provider capacity reached".into());
    }
    if let Some(network) = network {
        return Some(network.clone());
    }
    match open() {
        Ok(created) => Some(network.insert(created).clone()),
        Err(error) => reject(error.to_string()),
    }
}
/// The first bytes on an accepted connection name the grant they claim.
/// Identity and binding are verified by the session that owns that grant.
async fn session_bind(arrival: Arrival) -> io::Result<(SessionId, Incoming)> {
    let mut conn = match arrival {
        Arrival::Quic(connection) => {
            tracing::debug!(peer = %connection.remote_address(), "direct TLS accepted");
            let connection = QuicGuard(connection);
            let (send, recv) = connection.0.accept_bi().await.map_err(io::Error::other)?;
            DirectConn::Quic {
                connection,
                send,
                recv,
            }
        }
        Arrival::Tcp { stream, rtt } => {
            tracing::debug!(peer = ?tcp::addresses(&stream).0.ok(), "direct TLS accepted over TCP");
            DirectConn::Tcp { stream, rtt }
        }
    };
    let mut id = [0; 16];
    conn.read_exact(&mut id).await?;
    let mut tag = [0; 32];
    conn.read_exact(&mut tag).await?;
    Ok((id, Incoming { conn, tag }))
}
/// Best effort: the consumer falls back without it once its wait expires.
fn send(reply: &Reply, message: DirectProviderMessage) -> bool {
    reply
        .try_send(LocalControlWrite::Direct(Box::new(message)))
        .is_ok()
}
/// Replies the coordinator waits on. A dropped `RelaySelected` would leave
/// the consumer waiting for a fallback that cannot start, so these wait for
/// room in the control writer, bounded by the setup deadline.
async fn send_until(
    reply: &Reply,
    message: DirectProviderMessage,
    deadline: Instant,
) -> io::Result<()> {
    let write = reply.send(LocalControlWrite::Direct(Box::new(message)));
    tokio::time::timeout_at(deadline, write)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "coordination reply timed out"))?
        .map_err(|_| io::Error::other("coordination route closed"))
}

/// One session, from its offer to its end. Only the backend dial and the
/// forwarding know the local service's stream type.
async fn session<L: StreamProvider>(ctx: SessionContext, offer: Offer) -> io::Result<()>
where
    L::Item: StreamForward,
{
    let Some(mut committed) = commit(&ctx, offer).await? else {
        return Ok(());
    };
    // A committed session belongs to the endpoints even while its backend dial
    // is pending. A simultaneous credential rejection must not tear it down.
    let _active = ActiveSession::new(ctx.active.clone());
    // This is the sole backend dial for direct. No backend is opened while probing.
    let dial = L::from_addr(ctx.local.as_slice());
    let local = match tokio::time::timeout_at(committed.deadline, dial).await {
        Ok(Ok(local)) => local,
        _ => {
            let _ = committed.incoming.conn.write_all(&[4]).await;
            return Err(io::Error::other("local service connect failed"));
        }
    };
    let Opened {
        framing,
        meter,
        mut report,
    } = open(&ctx, &mut committed).await?;
    let conn = committed.incoming.conn;
    report
        .during(forward_direct(local, conn, framing, ctx.idle, Some(meter)))
        .await
}

/// A session past its commit point. What it held while it set up, it holds
/// until it ends: the probe state, the coordination route and its events.
struct Committed {
    incoming: Incoming,
    lan: Assessment,
    discovered: Discovered,
    discover_ms: u64,
    started: Instant,
    deadline: Instant,
    probing_started: Instant,
    committing: Instant,
    _probe_guard: ProbeGuard,
    _events: mpsc::Receiver<SessionEvent>,
    _reply: Reply,
    grant: SessionGrant,
    network: Arc<Network>,
}

/// Answer an offer and run its setup to the commit point: discover this
/// end, probe the consumer until its connection is bound and the grant
/// confirmed, then commit. `None` when the setup ended without a session
/// and without failing: cancelled, or with the relay selected.
async fn commit(ctx: &SessionContext, offer: Offer) -> io::Result<Option<Committed>> {
    let Offer {
        network,
        mut grant,
        stun_addr,
        reply,
        mut events,
    } = offer;
    let started = Instant::now();
    let deadline = started + Duration::from_millis(grant.ttl_ms);
    let probe_guard = ProbeGuard {
        network: network.clone(),
        id: grant.id,
    };
    let discovered = network.peer(stun_addr).await;
    let discover_ms = ms(started.elapsed());
    grant.provider = discovered.peer.clone();
    network.authorize(&grant, true, deadline)?;
    let lan = Assessment::new(
        discovered.local.clone(),
        &grant.consumer.candidates,
        ctx.policy.lan_first,
    );
    if let Some(evidence) = lan.evidence {
        ctx.diagnostics.lan_detected(evidence, lan.prefers());
    }
    let tcp_offer = offer_tcp(&network, &mut grant, &lan, stun_addr, ctx.policy.lan_tcp);
    let accept = DirectProviderMessage::Accept {
        id: grant.id,
        peer: grant.provider.clone(),
    };
    send_until(&reply, accept, deadline).await?;
    let mut incoming: Option<Incoming> = None;
    let mut granted = false;
    let probing_started = Instant::now();
    {
        let probing = network.keep_probing(&grant, &lan);
        tokio::pin!(probing);
        loop {
            if granted && incoming.is_some() {
                break;
            }
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "provider setup expired"));
                }
                () = &mut probing => {}
                event = events.recv() => match event {
                    Some(SessionEvent::Granted) => {
                        tracing::debug!(id = ?grant.id, "direct grant accepted by provider");
                        granted = true;
                    }
                    Some(SessionEvent::Incoming(candidate)) if incoming.is_none() => {
                        tracing::debug!(id = ?grant.id, "direct session bind received");
                        match verify(&candidate, &grant) {
                            Ok(()) => incoming = Some(candidate),
                            Err(error) => tracing::debug!(%error, "direct session binding rejected"),
                        }
                    }
                    // A later bind for a session that has one is let go.
                    Some(SessionEvent::Incoming(_)) => {}
                    Some(SessionEvent::Relay) => {
                        // The consumer chose the relay before any path committed:
                        // the punch did not get through.
                        network.punching_failed();
                        lan_missed(&ctx.diagnostics, &lan, None);
                        let selected = DirectProviderMessage::RelaySelected { id: grant.id };
                        return send_until(&reply, selected, deadline).await.map(|()| None);
                    }
                    Some(SessionEvent::Cancel) | None => return Ok(None),
                }
            }
        }
    }
    let Some(mut incoming) = incoming else {
        return Err(io::Error::other("missing prepared connection"));
    };
    let committing = Instant::now();
    incoming.conn.write_all(&[1]).await?;
    // Keep the read future alive while processing probe ticks or coordination.
    {
        let start = async { incoming.conn.read_u8().await };
        tokio::pin!(start);
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "direct commit expired"));
                }
                result = &mut start => {
                    if result? != 2 {
                        return Err(io::Error::other("invalid direct commit"));
                    }
                    break;
                }
                event = events.recv() => match event {
                    Some(SessionEvent::Relay) => {
                        lan_missed(&ctx.diagnostics, &lan, None);
                        let selected = DirectProviderMessage::RelaySelected { id: grant.id };
                        return send_until(&reply, selected, deadline).await.map(|()| None);
                    }
                    Some(SessionEvent::Cancel) | None => return Ok(None),
                    Some(SessionEvent::Granted | SessionEvent::Incoming(_)) => {}
                }
            }
        }
    }
    // Committed: no other TCP connection is waited for.
    drop(tcp_offer);
    Ok(Some(Committed {
        incoming,
        lan,
        discovered,
        discover_ms,
        started,
        deadline,
        probing_started,
        committing,
        _probe_guard: probe_guard,
        _events: events,
        _reply: reply,
        grant,
        network,
    }))
}

/// A committed session, opened: what its forwarding takes, and its reports.
struct Opened {
    framing: [u8; 32],
    meter: Arc<ByteMeter>,
    report: SessionReporter,
}

/// Tell the consumer that the local service is connected, then describe the
/// session's path and start reporting it.
async fn open(ctx: &SessionContext, c: &mut Committed) -> io::Result<Opened> {
    c.incoming.conn.write_all(&[3]).await?;
    let framing = c.incoming.conn.framing_key(&c.grant)?;
    let learned = c.network.learned(&c.grant.id);
    let (first_pong, lan_first_pong) = first_answers(&learned, c.probing_started, &c.lan);
    let timings = SetupTimings {
        discover_ms: Some(c.discover_ms),
        first_pong_ms: first_pong.map(ms),
        lan_first_pong_ms: lan_first_pong.map(ms),
        commit_ms: Some(ms(c.committing.elapsed())),
        setup_ms: ms(c.started.elapsed()),
        ..stun_timings(SetupTimings::default(), &c.discovered)
    };
    let candidates = CandidateCounts {
        probed: c.network.reachable(&c.grant.consumer.candidates),
        answered: learned.len() as u32,
        ..counts(&c.discovered, &c.lan, &c.grant.consumer.candidates)
    };
    let link = direct_link(
        &c.incoming.conn,
        &c.lan,
        &c.grant.consumer.candidates,
        timings,
        candidates,
    )?;
    if link.kind != LinkKind::Lan {
        lan_missed(&ctx.diagnostics, &c.lan, Some(link.kind));
    }
    log_path(&c.grant.service, &link);
    let summary = summary(&link);
    let link_guard = ctx.diagnostics.link_opened(link, c.incoming.conn.probe());
    let session = ctx
        .diagnostics
        .session_opened(SessionPath::Direct, Some(summary));
    let meter = session.meter();
    let report = SessionReporter::start(
        Sink::Provider(ctx.reports.clone()),
        &c.grant,
        SessionPath::Direct,
        Some(summary),
        session,
        Some(link_guard),
    );
    Ok(Opened {
        framing,
        meter,
        report,
    })
}
/// The connection is the granted consumer's, by the one certificate it
/// presented, and is bound to this grant.
fn verify(incoming: &Incoming, grant: &SessionGrant) -> io::Result<()> {
    if incoming.conn.peer_certificate().as_deref() != Some(grant.consumer.certificate.as_slice()) {
        return Err(io::Error::other(
            "client identity does not match this session",
        ));
    }
    incoming.conn.verify_binding(grant, &incoming.tag)
}
/// Serve TCP within a LAN to a consumer that asked for it, when this end
/// does and the two share a network: as the offer showed, wherever the relay
/// is, with this end's LAN addresses; or, with neither evidence of one nor
/// anything but a relay on loopback, as this computer, with loopback ones.
/// They go into its answer, and its listeners admit connections while the
/// returned guard lives. A session across the internet is never offered TCP.
fn offer_tcp(
    network: &Network,
    grant: &mut SessionGrant,
    lan: &Assessment,
    stun: SocketAddr,
    serve: bool,
) -> Option<tcp::Offered> {
    let local = lan.evidence.is_none()
        && stun.ip().is_loopback()
        && grant
            .consumer
            .candidates
            .iter()
            .any(|addr| addr.ip().is_loopback());
    if !serve || grant.consumer.lan_tcp.is_none() || (lan.evidence.is_none() && !local) {
        return None;
    }
    let (offered, addrs) = network.lan_tcp(local)?.offer(&lan.local.interfaces);
    if addrs.is_empty() {
        return None;
    }
    grant.provider.lan_tcp = Some(addrs);
    Some(offered)
}
struct ProbeGuard {
    network: Arc<Network>,
    id: SessionId,
}
impl Drop for ProbeGuard {
    fn drop(&mut self) {
        self.network.forget(&self.id);
    }
}
#[derive(Default)]
pub(super) struct ProviderReports {
    latest: Latest,
    // Alternative pooled control connections to ONE coordinator. A report
    // needs one live route, not broadcast copies on every worker connection.
    routes: std::sync::Mutex<Vec<Reply>>,
    /// Sessions whose newest report no route accepted. A final `Closed` has
    /// no later tick to carry it, so these are retried until delivered.
    unsent: std::sync::Mutex<HashSet<SessionId>>,
}
impl ProviderReports {
    fn connected(&self, reply: Reply) {
        let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
        routes.retain(|route| !route.is_closed());
        if !routes.iter().any(|route| route.same_channel(&reply)) {
            if routes.len() < MAX_ROUTES {
                routes.push(reply.clone());
            }
            drop(routes);
            for update in self.latest.snapshot() {
                let id = update.report.id;
                if !send(&reply, DirectProviderMessage::Report(update)) {
                    self.mark(id, false);
                }
            }
        }
    }
    pub(super) fn put(&self, update: SessionUpdate) {
        self.latest.put(update.clone());
        let id = update.report.id;
        let delivered = self.deliver(update);
        self.mark(id, delivered);
    }
    fn deliver(&self, update: SessionUpdate) -> bool {
        let routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
        let mut message =
            LocalControlWrite::Direct(Box::new(DirectProviderMessage::Report(update)));
        for route in routes.iter() {
            match route.try_send(message) {
                Ok(()) => return true,
                Err(error) => message = error.into_inner(),
            }
        }
        false
    }
    fn mark(&self, id: SessionId, delivered: bool) {
        let mut unsent = self.unsent.lock().unwrap_or_else(|e| e.into_inner());
        if delivered {
            unsent.remove(&id);
        } else if unsent.len() < MAX_UNSENT {
            unsent.insert(id);
        }
    }
    fn has_unsent(&self) -> bool {
        !self
            .unsent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
    /// Resend the newest report of each undelivered session, stopping while
    /// every route is still full.
    fn retry(&self) {
        let ids: Vec<_> = self
            .unsent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect();
        for id in ids {
            let Some(update) = self.latest.get(&id) else {
                self.mark(id, true);
                continue;
            };
            if !self.deliver(update) {
                return;
            }
            self.mark(id, true);
        }
    }
}
struct ActiveSession(Arc<AtomicUsize>);
impl ActiveSession {
    fn new(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::AcqRel);
        Self(active)
    }
}
impl Drop for ActiveSession {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{io::AsyncReadExt, net::TcpListener};
    use tokio_util::task::AbortOnDropHandle;
    use uni_stream::stream::TcpStreamProvider;

    #[tokio::test]
    async fn reports_refused_by_a_full_route_are_retried() {
        let reports = ProviderReports::default();
        let (route, mut writes) = mpsc::channel(1);
        reports.connected(route);
        let update = |state| {
            let id = random_session_id();
            SessionUpdate {
                session: SessionSummary {
                    server_instance: id,
                    id,
                    namespace: 0,
                    service: "svc".into(),
                    is_datagram: false,
                    consumer_key: 0,
                    provider_key: 0,
                },
                secret: SessionSecret::random(),
                report: SessionReport {
                    id,
                    seq: 1,
                    path: SessionPath::Direct,
                    state,
                    reason: None,
                    link: None,
                },
            }
        };
        reports.put(update(SessionState::Active));
        let closed = update(SessionState::Closed);
        let closed_id = closed.report.id;
        reports.put(closed);
        assert!(
            reports.has_unsent(),
            "a full route must not lose the final report"
        );
        reports.retry();
        assert!(reports.has_unsent(), "still full");
        let _ = writes.recv().await;
        reports.retry();
        assert!(!reports.has_unsent());
        let Some(LocalControlWrite::Direct(message)) = writes.recv().await else {
            panic!("expected the retried report");
        };
        assert!(matches!(*message, DirectProviderMessage::Report(u) if u.report.id == closed_id));
    }

    #[tokio::test]
    async fn committed_direct_cannot_acknowledge_relay_or_dial_backend_twice() {
        let provider = Network::new(
            true,
            crate::sdk::PortMapping::Off,
            &crate::diagnostics::Diagnostics::default(),
        )
        .unwrap();
        let consumer = Network::new(
            false,
            crate::sdk::PortMapping::Off,
            &crate::diagnostics::Diagnostics::default(),
        )
        .unwrap();
        let stun = "127.0.0.1:9".parse().unwrap();
        let mut grant = SessionGrant {
            id: random_session_id(),
            server_instance: random_session_id(),
            generation: 1,
            namespace: 0,
            service: "commit-boundary".into(),
            is_datagram: false,
            consumer_key: 0,
            provider_key: 0,
            consumer: consumer.peer(stun).await.peer,
            provider: PeerInfo {
                certificate: vec![],
                candidates: vec![],
                lan_tcp: None,
            },
            secret: SessionSecret::random(),
            ttl_ms: SETUP_TTL_MS,
        };
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (replies, mut reply_rx) = mpsc::channel(16);
        let (events, event_rx) = mpsc::channel(8);
        let active = Arc::new(AtomicUsize::new(0));
        let ctx = SessionContext {
            local: ResolvedAddrs::from(backend.local_addr().unwrap()),
            idle: None,
            diagnostics: Diagnostics::default(),
            active: active.clone(),
            reports: Arc::new(ProviderReports::default()),
            policy: crate::direct_policy::DirectPolicy {
                port_mapping: crate::sdk::PortMapping::Off,
                lan_first: true,
                lan_transport: crate::sdk::LanTransport::Quic,
                lan_tcp: true,
            },
        };
        let offer = Offer {
            network: provider.clone(),
            grant: grant.clone(),
            stun_addr: stun,
            reply: replies,
            events: event_rx,
        };
        let task = AbortOnDropHandle::new(tokio::spawn(session::<TcpStreamProvider>(ctx, offer)));
        let Some(LocalControlWrite::Direct(accepted)) = reply_rx.recv().await else {
            panic!("expected accepted offer");
        };
        let DirectProviderMessage::Accept { peer, .. } = *accepted else {
            panic!("expected accepted offer");
        };
        grant.provider = peer;
        // Race all discovered paths. A losing TLS handshake never sends a
        // session binding, so only the winner may reach the commit worker.
        let port = grant
            .provider
            .candidates
            .iter()
            .find(|addr| addr.is_ipv4() && addr.ip().is_loopback())
            .expect("test requires a discovered IPv4 loopback path")
            .port();
        grant
            .provider
            .candidates
            .push(std::net::SocketAddr::from(([127, 0, 0, 2], port)));
        let deadline = Instant::now() + Duration::from_secs(3);
        consumer.authorize(&grant, false, deadline).unwrap();
        events.send(SessionEvent::Granted).await.unwrap();
        let dispatch = {
            let provider = provider.clone();
            let events = events.clone();
            AbortOnDropHandle::new(tokio::spawn(async move {
                let mut arrivals = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        connection = provider.accept() => {
                            let Some(connection) = connection else { break };
                            arrivals.spawn(tokio::time::timeout_at(deadline, session_bind(connection)));
                        }
                        Some(result) = arrivals.join_next(), if !arrivals.is_empty() => {
                            if let Ok(Ok(Ok((_, incoming)))) = result {
                                // A test that already gave up reports its own
                                // timeout; a send failure here would hide it.
                                let _ = events.send(SessionEvent::Incoming(incoming)).await;
                                break;
                            }
                        }
                    }
                }
            }))
        };
        let (conn, _) = consumer
            .connect(
                &grant,
                deadline,
                &Assessment::default(),
                &mut tcp::Race::idle(),
            )
            .await
            .unwrap();
        let DirectConn::Quic {
            connection,
            mut send,
            mut recv,
        } = conn
        else {
            panic!("a consumer that did not ask for TCP takes QUIC");
        };
        send.write_all(&grant.id).await.unwrap();
        send.write_all(&super::super::binding::binding(&connection.0, &grant).unwrap())
            .await
            .unwrap();
        tokio::time::timeout_at(deadline, dispatch)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recv.read_u8().await.unwrap(), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), backend.accept())
                .await
                .is_err(),
            "preparation must not connect the backend"
        );
        send.write_all(&[2]).await.unwrap();
        let (_backend, _) = tokio::time::timeout(Duration::from_secs(2), backend.accept())
            .await
            .unwrap()
            .unwrap();
        // The consumer loses Started. A late relay request must not authorize
        // replay, whether the provider is forwarding or notices the reset first.
        let _ = recv.stop(0u32.into());
        let _ = events.send(SessionEvent::Relay).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), backend.accept())
                .await
                .is_err()
        );
        connection.0.close(0u32.into(), b"test complete");
        let _ = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap();
        while let Ok(message) = reply_rx.try_recv() {
            assert!(!matches!(
                message,
                LocalControlWrite::Direct(message)
                    if matches!(*message, DirectProviderMessage::RelaySelected { .. })
            ));
        }
        assert_eq!(active.load(Ordering::Acquire), 0);
        // Serving TCP costs nothing until a consumer asks for it.
        assert!(!provider.lan_tcp_opened());
    }
}
