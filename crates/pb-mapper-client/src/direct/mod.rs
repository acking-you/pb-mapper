//! Client-owned direct sessions. Control sockets never own an established data task.
mod candidates;
mod control;
mod endpoint;
mod identity;
mod lan;
#[cfg(feature = "port-mapping")]
mod mapping;
mod provider;
mod reports;
mod socket;
mod tcp;

use crate::{
    diagnostics::{
        CandidateCounts, Diagnostics, DirectLink, DirectStage, FallbackReason, LanEvidence,
        LanTcpMiss, LinkGuard, LinkKind, LinkProbe, LinkStats, LinkVia, SessionGuard, SetupTimings,
        ms,
    },
    endpoint::RelayEndpoint,
    sdk::LanTransport,
};
use control::{Control, io_error};
use endpoint::{Discovered, Network};
use lan::Assessment;
use pb_mapper_auth::ADMIN_NAMESPACE;
use pb_mapper_core::checksum::Credential;
use pb_mapper_protocol::{
    command::{PbConnRequest, PbConnStatusReq, PbConnStatusResp},
    data::DataCodec,
    direct::*,
    forward::{ByteMeter, StreamForward},
};
pub(crate) use provider::Provider;
use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tcp::TlsTcp;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, OnceCell},
    time::Instant,
};
use uni_stream::stream::NetworkStream;

/// How long a TCP session's end waits for the peer's, after its own: the
/// TCP twin of waiting for QUIC to acknowledge the last bytes.
const TCP_DRAIN: Duration = Duration::from_secs(3);

/// First pause after a direct setup falls back to the relay.
const COOLDOWN_INITIAL: Duration = Duration::from_secs(10);
/// Longest pause between direct attempts on a network that keeps refusing them.
const COOLDOWN_MAX: Duration = Duration::from_secs(300);

/// When a tunnel may try direct again after falling back.
///
/// A path that cannot be punched — a symmetric NAT, blocked UDP, a relay or
/// publisher without direct — fails the same way for every connection, and
/// each failure costs that connection its whole setup budget before the relay
/// takes over. Pausing direct after a fallback sends the next connections
/// straight to the relay instead. The pause doubles while fallbacks repeat; a
/// direct success or a network change, which can make a path punchable,
/// clears it.
#[derive(Debug, Default)]
struct Cooldown {
    until: Option<Instant>,
    next: Duration,
}

impl Cooldown {
    fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// Start the next pause, unless one is running. A burst of connections
    /// that were all mid-attempt when the path failed is one failure, not one
    /// per connection: counting each would jump straight to the cap.
    fn back_off(&mut self, now: Instant) {
        if self.active(now) {
            return;
        }
        let pause = if self.next.is_zero() {
            COOLDOWN_INITIAL
        } else {
            self.next
        };
        self.until = Some(now + pause);
        self.next = (pause * 2).min(COOLDOWN_MAX);
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

pub(crate) struct Consumer {
    remote: RelayEndpoint,
    credential: Credential,
    namespace: Option<u64>,
    is_datagram: bool,
    keep_alive: bool,
    control: Arc<Mutex<Option<Arc<Control>>>>,
    reports: Arc<reports::Latest>,
    report_wake: Arc<tokio::sync::Notify>,
    _reporter: tokio_util::task::AbortOnDropHandle<()>,
    network: OnceCell<Arc<Network>>,
    diagnostics: Diagnostics,
    cooldown: std::sync::Mutex<Cooldown>,
    /// When sessions may ask for TCP again, after one whose attempts missed:
    /// a provider that refuses TCP, or whose firewall drops it, does so for
    /// every session, and each would pay the head start. Never the relay's
    /// pause: QUIC is still tried.
    tcp_cooldown: std::sync::Mutex<Cooldown>,
    policy: crate::sdk::DirectPolicy,
}
impl Consumer {
    pub fn new(
        remote: RelayEndpoint,
        credential: Credential,
        namespace: Option<u64>,
        is_datagram: bool,
        keep_alive: bool,
        diagnostics: Diagnostics,
        policy: crate::sdk::DirectPolicy,
    ) -> Arc<Self> {
        let control = Arc::new(Mutex::new(None::<Arc<Control>>));
        let reports = Arc::new(reports::Latest::default());
        let report_wake = Arc::new(tokio::sync::Notify::new());
        let reporter = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(report_loop(
            remote.clone(),
            credential,
            namespace,
            control.clone(),
            reports.clone(),
            report_wake.clone(),
        )));
        Arc::new(Self {
            remote,
            credential,
            namespace,
            is_datagram,
            keep_alive,
            control,
            reports,
            report_wake,
            _reporter: reporter,
            network: OnceCell::new(),
            diagnostics,
            cooldown: std::sync::Mutex::default(),
            tcp_cooldown: std::sync::Mutex::default(),
            policy,
        })
    }
    fn cooldown(&self) -> std::sync::MutexGuard<'_, Cooldown> {
        self.cooldown.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn tcp_cooldown(&self) -> std::sync::MutexGuard<'_, Cooldown> {
        self.tcp_cooldown.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// A setup that asked for TCP took `took`, or the relay: note why TCP
    /// was missed, pausing it when the miss cost the head start, or clear
    /// the pause when TCP carried it.
    fn tcp_settled(&self, missed: Option<LanTcpMiss>, took: Option<DirectTransport>) {
        match missed {
            Some(miss) => {
                self.diagnostics.lan_tcp_missed(miss, took);
                if tcp::Race::costly(miss) {
                    self.tcp_cooldown().back_off(Instant::now());
                }
            }
            None if took == Some(DirectTransport::Tcp) => self.tcp_cooldown().clear(),
            None => {}
        }
    }
    /// The TCP attempts of a setup that asked for TCP: one per address the
    /// provider offered on a network this end shares, or loopback when the
    /// relay is on it.
    fn tcp_race(
        network: &Network,
        grant: &SessionGrant,
        lan: &Assessment,
        stun: SocketAddr,
    ) -> tcp::Race {
        let offered = grant.provider.lan_tcp.as_deref().unwrap_or_default();
        if offered.is_empty() {
            return tcp::Race::missed(LanTcpMiss::NotOffered);
        }
        let targets = tcp::targets(offered, |addr| lan.is_lan(addr), stun.ip().is_loopback());
        #[cfg(test)]
        let targets = network.tcp_redirect.get().map_or(targets, |to| vec![*to]);
        if targets.is_empty() {
            return tcp::Race::missed(LanTcpMiss::NoSharedLan);
        }
        match network.identity.tcp_client(&grant.provider.certificate) {
            Ok(config) => tcp::Race::start(&targets, config),
            // QUIC builds its own from the same certificate, and fails alike.
            Err(_) => tcp::Race::missed(LanTcpMiss::Handshake),
        }
    }
    /// Pause direct after a fallback, and say until when.
    fn back_off(&self) {
        let mut cooldown = self.cooldown();
        cooldown.back_off(Instant::now());
        self.diagnostics.direct_cooldown(cooldown.until);
    }
    fn clear_cooldown(&self) {
        self.cooldown().clear();
        self.diagnostics.direct_cooldown(None);
    }
    async fn control(&self, key: &str) -> io::Result<Option<Arc<Control>>> {
        let mut slot = self.control.lock().await;
        if let Some(control) = slot.as_ref().filter(|c| c.alive()) {
            return Ok(Some(control.clone()));
        }
        // Concurrent setups queue here. If the one ahead learned that direct
        // is unavailable, the rest go to the relay instead of asking again.
        if self.cooldown().active(Instant::now()) {
            return Ok(None);
        }
        let mut stream = self.remote.connect(self.credential, &mut None).await?;
        let response = crate::client::status::get_status_with_credential(
            &mut stream,
            PbConnStatusReq::Service {
                key: key.into(),
                direct_query: Some(true),
            },
            self.namespace,
            &self.credential,
        )
        .await
        .map_err(io_error)?;
        let PbConnStatusResp::Service {
            direct: Some(capability),
            ..
        } = response
        else {
            tracing::debug!(service = %key, "relay or publisher does not offer direct");
            *slot = None;
            // Set before the lock is released, so no queued setup repeats the query.
            self.back_off();
            return Ok(None);
        };
        if capability.version != DIRECT_VERSION {
            tracing::debug!(
                version = capability.version,
                "unsupported direct coordination version"
            );
            self.back_off();
            return Ok(None);
        }
        let control = Control::connect(&self.remote, self.credential, self.namespace).await?;
        *slot = Some(control.clone());
        Ok(Some(control))
    }
    pub fn invalidate(&self) {
        self.clear_cooldown();
        self.tcp_cooldown().clear();
        if let Some(network) = self.network.get() {
            network.invalidate();
        }
    }
    pub async fn stop(&self) {
        if let Some(network) = self.network.get() {
            network.shutdown().await;
        }
    }
    pub async fn prepare(
        self: &Arc<Self>,
        key: &str,
        deadline: Instant,
    ) -> io::Result<Option<Prepared>> {
        // A recent fallback says this path will not punch: take the ordinary
        // relay path without spending the connection's setup budget on it.
        if self.cooldown().active(Instant::now()) {
            self.diagnostics.cooldown_skip();
            return Ok(None);
        }
        // Before a grant arrives no endpoint can have committed, so a failed
        // capability/control/discovery attempt can use the ordinary relay path.
        let mut attempt = self.diagnostics.direct_attempt();
        let mut timings = SetupTimings::default();
        // TCP within a LAN is asked for, unless a recent miss paused it.
        let ask_tcp = self.policy.lan_transport == LanTransport::Tcp
            && !self.tcp_cooldown().active(Instant::now());
        let negotiation = async {
            let stage = Instant::now();
            let Some(control) = self.control(key).await? else {
                return Ok(None);
            };
            let network = self
                .network
                .get_or_try_init(|| async {
                    Network::new(false, self.policy.port_mapping, &self.diagnostics)
                })
                .await?
                .clone();
            timings.negotiate_ms = Some(ms(stage.elapsed()));
            attempt.stage(DirectStage::Discovering);
            let stage = Instant::now();
            let discovered = network.peer(control.capability.stun_addr).await;
            timings.discover_ms = Some(ms(stage.elapsed()));
            let stage = Instant::now();
            let mut peer = discovered.peer.clone();
            if ask_tcp {
                peer.lan_tcp = Some(Vec::new());
            }
            let Some(grant) = control.open(key.into(), self.is_datagram, peer).await? else {
                return Ok(None);
            };
            timings.grant_ms = Some(ms(stage.elapsed()));
            Ok::<_, io::Error>(Some((control, network, discovered, grant)))
        };
        let negotiation = tokio::time::timeout_at(
            deadline.min(Instant::now() + Duration::from_secs(3)),
            negotiation,
        )
        .await;
        let (control, network, discovered, grant) = match negotiation {
            Ok(Ok(Some(prepared))) => prepared,
            result => {
                let reason = match result {
                    Ok(Ok(None)) => FallbackReason::NotOffered,
                    Ok(Err(error)) => {
                        tracing::debug!(%error, "direct coordination or discovery failed before a grant");
                        FallbackReason::NegotiationFailed
                    }
                    Err(_) => FallbackReason::NegotiationTimeout,
                    Ok(Ok(Some(_))) => unreachable!(),
                };
                tracing::info!(event = "session_path_selected", path = "relay", service = %key, reason = reason.as_str());
                attempt.fell_back(reason);
                self.back_off();
                return Ok(None);
            }
        };
        let deadline = deadline.min(Instant::now() + Duration::from_millis(grant.ttl_ms));
        // Only a namespace this client named, or the administrator's, can be checked
        // here. A temporary credential's is the relay's to know: one issued into a
        // namespace does not carry it, and its key id names something else.
        let expected_namespace = self
            .namespace
            .or_else(|| self.credential.is_admin().then_some(ADMIN_NAMESPACE));
        if grant.service != key
            || grant.is_datagram != self.is_datagram
            || grant.consumer_key != self.credential.key_id()
            || expected_namespace.is_some_and(|namespace| grant.namespace != namespace)
            || grant.ttl_ms == 0
            || grant.ttl_ms > SETUP_TTL_MS
            || grant.consumer.certificate != network.identity.certificate
            || !grant.provider.valid()
        {
            control.cancel(grant.id);
            return Err(io::Error::other("invalid direct session grant"));
        }
        network.authorize(&grant, false, deadline)?;
        let mut pending = PendingSession {
            network: network.clone(),
            control: control.clone(),
            id: grant.id,
            completed: false,
        };
        let lan = Assessment::new(
            discovered.local.clone(),
            &grant.provider.candidates,
            self.policy.lan_first,
        );
        if let Some(evidence) = lan.evidence {
            self.diagnostics.lan_detected(evidence, lan.prefers());
        }
        let mut tcp = if ask_tcp {
            Self::tcp_race(&network, &grant, &lan, control.capability.stun_addr)
        } else {
            tcp::Race::idle()
        };
        attempt.stage(DirectStage::Punching);
        let direct_deadline = deadline.min(Instant::now() + Duration::from_secs(2));
        let candidate = tokio::time::timeout_at(direct_deadline, async {
            let (mut conn, dialed) = network
                .connect(&grant, direct_deadline, &lan, &mut tcp)
                .await?;
            tracing::debug!(id=?grant.id, peer=?conn.remote_address(), transport = conn.transport().as_str(), "direct TLS connected");
            let tag = conn.binding(&grant)?;
            conn.write_all(&grant.id).await?;
            conn.write_all(&tag).await?;
            if conn.read_u8().await? != 1 {
                return Err(io::Error::other("direct path was not ready"));
            }
            Ok::<_, io::Error>((conn, dialed))
        })
        .await;
        // What TCP came to, with its attempts still in flight let go.
        let tcp_missed = tcp.outcome(Instant::now());
        drop(tcp);
        if let Ok(Ok((mut conn, dialed))) = candidate {
            // From this point on, timeout/cancellation/ACK loss MUST NOT select relay.
            pending.completed = true;
            attempt.stage(DirectStage::Committing);
            let committing = Instant::now();
            conn.write_all(&[2]).await?;
            if conn.read_u8().await? != 3 {
                return Err(io::Error::other(
                    "provider could not open the local service",
                ));
            }
            let framing_key = conn.framing_key(&grant)?;
            let setup = attempt.elapsed();
            let timings = SetupTimings {
                first_pong_ms: dialed.first_pong.map(ms),
                lan_first_pong_ms: dialed.lan_first_pong.map(ms),
                handshake_ms: Some(ms(dialed.handshake)),
                commit_ms: Some(ms(committing.elapsed())),
                setup_ms: ms(setup),
                ..stun_timings(timings, &discovered)
            };
            let candidates = CandidateCounts {
                probed: dialed.probed,
                answered: dialed.answered,
                handshakes: dialed.handshakes,
                ..counts(&discovered, &lan, &grant.provider.candidates)
            };
            let link = direct_link(&conn, &lan, &grant.provider.candidates, timings, candidates)?;
            if let Some(evidence) = lan.evidence
                && link.kind != LinkKind::Lan
            {
                self.diagnostics.lan_missed(evidence, Some(link.kind));
            }
            self.tcp_settled(tcp_missed, Some(link.transport));
            attempt.established(setup, link.kind);
            self.clear_cooldown();
            log_path(key, &link);
            Ok(Some(Prepared {
                path: DataPath::Direct {
                    conn,
                    framing_key,
                    link: Box::new(link),
                    _network: network,
                },
                grant,
                owner: self.clone(),
                diagnostics: self.diagnostics.clone(),
            }))
        } else {
            tracing::debug!(reason = ?candidate.as_ref().map(|r| r.as_ref().map(|_| ())), "direct attempt failed before commit");
            if let Some(evidence) = lan.evidence {
                self.diagnostics.lan_missed(evidence, None);
            }
            self.tcp_settled(tcp_missed, None);
            attempt.fell_back(FallbackReason::Unreachable);
            self.back_off();
            network.punching_failed();
            control.relay(grant.id).await?;
            let ready = crate::client::stream::subscribe_request(
                PbConnRequest::DirectRelay {
                    id: grant.id,
                    secret: grant.secret,
                    namespace: self.namespace,
                    data_protocol: Some(pb_mapper_protocol::data::DATA_PROTOCOL_V2),
                },
                self.remote.clone(),
                self.keep_alive,
                self.credential,
                true,
                &mut None,
            )
            .await
            .map_err(io_error)?;
            pending.completed = true;
            tracing::info!(event = "session_path_selected", path = "relay", service = %key, reason = FallbackReason::Unreachable.as_str());
            Ok(Some(Prepared {
                path: DataPath::Relay {
                    socket: ready.0,
                    codec: ready.1,
                },
                grant,
                owner: self.clone(),
                diagnostics: self.diagnostics.clone(),
            }))
        }
    }
}
struct PendingSession {
    network: Arc<Network>,
    control: Arc<Control>,
    id: SessionId,
    completed: bool,
}
impl Drop for PendingSession {
    fn drop(&mut self) {
        self.network.forget(&self.id);
        if !self.completed {
            self.control.cancel(self.id);
        }
    }
}
pub(super) struct QuicGuard(pub quinn::Connection);
impl Drop for QuicGuard {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"session finished");
    }
}

/// A direct session's connection on the carrier that won: one QUIC
/// connection's single stream, or one TLS stream over TCP. The commit bytes
/// and the binding are the same on both; forwarding picks its loop once.
pub(super) enum DirectConn {
    Quic {
        connection: QuicGuard,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
    },
    Tcp {
        stream: Box<TlsTcp>,
        /// The handshake's round trip, which stands in for TCP's figures.
        rtt: Duration,
    },
}

impl DirectConn {
    fn transport(&self) -> DirectTransport {
        match self {
            Self::Quic { .. } => DirectTransport::Quic,
            Self::Tcp { .. } => DirectTransport::Tcp,
        }
    }
    fn remote_address(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Quic { connection, .. } => Ok(connection.0.remote_address()),
            Self::Tcp { stream, .. } => tcp::addresses(stream).0,
        }
    }
    fn local_ip(&self) -> Option<IpAddr> {
        match self {
            Self::Quic { connection, .. } => connection.0.local_ip(),
            Self::Tcp { stream, .. } => tcp::addresses(stream).1,
        }
    }
    fn probe(&self) -> LinkProbe {
        match self {
            Self::Quic { connection, .. } => LinkProbe::Quic(connection.0.clone()),
            Self::Tcp { rtt, .. } => LinkProbe::Tcp {
                handshake_rtt: *rtt,
            },
        }
    }
    /// The certificate the other end presented, when it presented one.
    pub(super) fn peer_certificate(&self) -> Option<Vec<u8>> {
        match self {
            Self::Quic { connection, .. } => {
                let certs = connection
                    .0
                    .peer_identity()?
                    .downcast::<Vec<quinn::rustls::pki_types::CertificateDer<'static>>>()
                    .ok()?;
                match certs.as_slice() {
                    [certificate] => Some(certificate.to_vec()),
                    _ => None,
                }
            }
            Self::Tcp { stream, .. } => tcp::peer_certificate(stream).map(<[u8]>::to_vec),
        }
    }
    pub(super) fn binding(&self, grant: &SessionGrant) -> io::Result<[u8; 32]> {
        match self {
            Self::Quic { connection, .. } => endpoint::binding(&connection.0, grant),
            Self::Tcp { stream, .. } => endpoint::binding(&**stream, grant),
        }
    }
    pub(super) fn verify_binding(&self, grant: &SessionGrant, tag: &[u8]) -> io::Result<()> {
        match self {
            Self::Quic { connection, .. } => endpoint::verify_binding(&connection.0, grant, tag),
            Self::Tcp { stream, .. } => endpoint::verify_binding(&**stream, grant, tag),
        }
    }
    pub(super) fn framing_key(&self, grant: &SessionGrant) -> io::Result<[u8; 32]> {
        match self {
            Self::Quic { connection, .. } => endpoint::framing_key(&connection.0, grant),
            Self::Tcp { stream, .. } => endpoint::framing_key(&**stream, grant),
        }
    }
    /// Setup bytes, sent at once: over TLS, flushed rather than left with
    /// its records.
    pub(super) async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Quic { send, .. } => send.write_all(bytes).await.map_err(io_error),
            Self::Tcp { stream, .. } => {
                stream.write_all(bytes).await?;
                stream.flush().await
            }
        }
    }
    pub(super) async fn read_u8(&mut self) -> io::Result<u8> {
        match self {
            Self::Quic { recv, .. } => recv.read_u8().await,
            Self::Tcp { stream, .. } => stream.read_u8().await,
        }
    }
    pub(super) async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        match self {
            Self::Quic { recv, .. } => recv.read_exact(buf).await.map_err(io_error),
            Self::Tcp { stream, .. } => stream.read_exact(buf).await.map(|_| ()),
        }
    }
}

enum DataPath {
    Direct {
        conn: DirectConn,
        framing_key: [u8; 32],
        link: Box<DirectLink>,
        _network: Arc<Network>,
    },
    Relay {
        socket: pb_mapper_protocol::transport::RelayStream,
        codec: Option<DataCodec>,
    },
}
pub(crate) struct Prepared {
    path: DataPath,
    grant: SessionGrant,
    owner: Arc<Consumer>,
    diagnostics: Diagnostics,
}
impl Prepared {
    pub async fn forward<L: NetworkStream + StreamForward>(
        self,
        mut local: L,
        credential: Credential,
    ) -> io::Result<()> {
        let (path, link) = match &self.path {
            DataPath::Direct { conn, link, .. } => (
                SessionPath::Direct,
                Some(self.diagnostics.link_opened((**link).clone(), conn.probe())),
            ),
            DataPath::Relay { .. } => (SessionPath::Relay, None),
        };
        let summary = match &self.path {
            DataPath::Direct { link, .. } => Some(summary(link)),
            DataPath::Relay { .. } => None,
        };
        let session = self.diagnostics.session_opened(path, summary);
        let meter = Some(session.meter());
        let mut report = ConsumerReport {
            owner: self.owner,
            session: (&self.grant).into(),
            secret: self.grant.secret,
            path,
            link: summary,
            seq: 0,
            _session: session,
            _link: link,
        };
        report.active();
        let forwarding = async {
            match self.path {
                DataPath::Direct {
                    conn,
                    framing_key,
                    link: _,
                    _network,
                } => forward_direct(local, conn, framing_key, None, meter).await,
                DataPath::Relay { mut socket, codec } => {
                    pb_mapper_protocol::with_relay_stream!(&mut socket, socket, {
                        let (read, write) = local.split();
                        let (remote_read, remote_write) = socket.split();
                        L::forward_local_to_remote_metered(
                            codec,
                            *credential.key(),
                            read,
                            write,
                            remote_read,
                            remote_write,
                            meter,
                        )
                        .await
                        .map(|_| ())
                        .map_err(io_error)
                    })
                }
            }
        };
        tokio::pin!(forwarding);
        let mut ticker = tokio::time::interval(Duration::from_secs(15));
        ticker.tick().await;
        loop {
            tokio::select! { result = &mut forwarding => return result, _ = ticker.tick() => report.active() }
        }
    }
}
/// Forward a committed direct session between `local` and the peer, on the
/// carrier its connection runs over.
async fn forward_direct<L: NetworkStream + StreamForward>(
    mut local: L,
    conn: DirectConn,
    framing: [u8; 32],
    idle: Option<Duration>,
    meter: Option<Arc<ByteMeter>>,
) -> io::Result<()> {
    let (read, write) = local.split();
    match conn {
        DirectConn::Quic {
            connection: _connection,
            mut send,
            recv,
        } => {
            let forward = L::forward_local_to_remote_metered(
                None, framing, read, write, recv, &mut send, meter,
            );
            idle_bounded(idle, forward).await?;
            // FIN queues bytes but does not acknowledge delivery. Keep the
            // connection alive until Quinn confirms the final response; local
            // stop still cancels us.
            let _ = send.finish();
            let _ = send.stopped().await;
        }
        DirectConn::Tcp { stream, .. } => {
            let (mut remote_read, mut remote_write) = tcp::split(*stream);
            let forward = L::forward_local_to_remote_metered(
                None,
                framing,
                read,
                write,
                &mut remote_read,
                &mut remote_write,
                meter,
            );
            idle_bounded(idle, forward).await?;
            // The TCP twin of the above: close_notify and FIN after the last
            // byte, then the peer's end read, so that nothing left unread
            // here turns this end's close into a reset that loses what it
            // sent. Bounded, as an unreachable peer must not hold a stop.
            let _ = tokio::time::timeout(TCP_DRAIN, async {
                let _ = remote_write.shutdown().await;
                let _ = tokio::io::copy(&mut remote_read, &mut tokio::io::sink()).await;
            })
            .await;
        }
    }
    Ok(())
}

async fn idle_bounded<T>(
    idle: Option<Duration>,
    forward: impl std::future::Future<Output = pb_mapper_core::error::Result<T>>,
) -> io::Result<()> {
    match idle {
        Some(idle) => pb_mapper_protocol::forward::with_tunnel_idle_timeout(idle, forward)
            .await
            .map(|_| ())
            .map_err(io_error),
        None => forward.await.map(|_| ()).map_err(io_error),
    }
}

struct ConsumerReport {
    owner: Arc<Consumer>,
    session: SessionSummary,
    secret: SessionSecret,
    path: SessionPath,
    link: Option<LinkSummary>,
    seq: u64,
    /// Closes the session in diagnostics after the final report.
    _session: SessionGuard,
    _link: Option<LinkGuard>,
}
impl ConsumerReport {
    fn update(&mut self, state: SessionState) {
        self.seq += 1;
        self.owner.reports.put(SessionUpdate {
            session: self.session.clone(),
            secret: self.secret,
            report: SessionReport {
                id: self.session.id,
                seq: self.seq,
                path: self.path,
                state,
                reason: None,
                link: self.link,
            },
        });
        self.owner.report_wake.notify_one();
    }
    fn active(&mut self) {
        self.update(SessionState::Active);
    }
}
impl Drop for ConsumerReport {
    fn drop(&mut self) {
        self.update(SessionState::Closed);
    }
}
async fn report_loop(
    remote: RelayEndpoint,
    credential: Credential,
    namespace: Option<u64>,
    control: Arc<Mutex<Option<Arc<Control>>>>,
    reports: Arc<reports::Latest>,
    wake: Arc<tokio::sync::Notify>,
) {
    let mut delay = Duration::from_secs(15);
    let mut retry = Duration::from_secs(1);
    loop {
        tokio::select! { () = wake.notified() => {}, () = tokio::time::sleep(delay) => {} }
        let snapshot = reports.snapshot();
        if snapshot.is_empty() {
            continue;
        }
        let attempt = async {
            let current = {
                let mut slot = control.lock().await;
                if slot.as_ref().is_none_or(|c| !c.alive()) {
                    let _permit = remote.control_permit().await;
                    *slot = Some(Control::connect(&remote, credential, namespace).await?);
                }
                slot.clone()
            };
            if let Some(control) = current {
                for report in snapshot {
                    // Backpressure belongs to this independent reporter. A
                    // snapshot larger than the channel must not repeatedly lose
                    // the same tail, or hold the setup mutex while it drains.
                    control.report(report).await?;
                }
            }
            Ok::<_, io::Error>(())
        };
        match tokio::time::timeout(Duration::from_secs(3), attempt).await {
            Ok(Ok(())) => {
                delay = Duration::from_secs(15);
                retry = Duration::from_secs(1);
            }
            _ => {
                // A failed reconnect must not cost another full reporting
                // interval. Retry independently of data with bounded backoff.
                delay = retry;
                retry = (retry * 2).min(Duration::from_secs(15));
            }
        }
    }
}

/// The figures from before the search: discovery's, with how STUN went.
fn stun_timings(timings: SetupTimings, discovered: &Discovered) -> SetupTimings {
    SetupTimings {
        stun_ms: discovered.stun_ms,
        stun: discovered.stun,
        ..timings
    }
}

/// The candidates each end offered.
fn counts(discovered: &Discovered, lan: &Assessment, peer: &[SocketAddr]) -> CandidateCounts {
    let (local, local_lan, truncated) = discovered.counts;
    CandidateCounts {
        local,
        local_lan,
        truncated,
        peer: peer.len() as u32,
        peer_lan: peer.iter().filter(|addr| lan.is_lan(**addr)).count() as u32,
        ..CandidateCounts::default()
    }
}

/// What a committed session's path is, as this end sees it. `offered` is
/// what the peer offered.
fn direct_link(
    conn: &DirectConn,
    lan: &Assessment,
    offered: &[SocketAddr],
    timings: SetupTimings,
    candidates: CandidateCounts,
) -> io::Result<DirectLink> {
    let peer = conn.remote_address()?;
    let local_ip = conn.local_ip();
    let interface = local_ip.and_then(|ip| lan.local.interfaces.local(ip));
    Ok(DirectLink {
        kind: lan.kind(local_ip, peer.ip()),
        family: AddressFamily::of(peer.ip()),
        via: via(peer, offered),
        transport: conn.transport(),
        peer,
        local_ip,
        interface: interface.map(|interface| interface.name.clone()),
        tier: interface.map(|interface| interface.tier),
        same_lan: lan.evidence,
        timings,
        candidates,
        stats: LinkStats::default(),
    })
}

/// How the peer address was learned. A peer offers its router mapping, then
/// what STUN saw, then its interfaces: two public IPv4 addresses in front
/// mean the first is the mapping.
fn via(peer: SocketAddr, offered: &[SocketAddr]) -> LinkVia {
    let Some(index) = offered.iter().position(|addr| *addr == peer) else {
        return LinkVia::PeerReflexive;
    };
    let public_v4 = |addr: &SocketAddr| addr.is_ipv4() && lan::is_global(addr.ip());
    if !public_v4(&peer) {
        LinkVia::Host
    } else if index == 0 && offered.get(1).is_some_and(public_v4) {
        LinkVia::Mapped
    } else {
        LinkVia::Public
    }
}

/// What a report says of a path: no address.
fn summary(link: &DirectLink) -> LinkSummary {
    LinkSummary {
        kind: link.kind,
        family: link.family,
        transport: link.transport,
    }
}

/// The line that says which way a session went, which the app's Logs page
/// shows. It carries the figures a LAN-first A/B compares: the setup time,
/// how soon a LAN address answered (absent when none did) and the handshakes
/// this end started (none on the publishing end, which is dialed).
fn log_path(service: &str, link: &DirectLink) {
    tracing::info!(
        event = "session_path_selected",
        path = "direct",
        service = %service,
        peer = %link.peer,
        kind = link.kind.as_str(),
        family = link.family.as_str(),
        transport = link.transport.as_str(),
        local_ip = %link.local_ip.map_or_else(|| "-".to_string(), |ip| ip.to_string()),
        evidence = link.same_lan.map_or("none", LanEvidence::as_str),
        setup_ms = link.timings.setup_ms,
        lan_first_pong_ms = link.timings.lan_first_pong_ms,
        // Only the dialing end makes handshakes; the dialed end has none to count.
        handshakes = link.timings.handshake_ms.map(|_| link.candidates.handshakes),
    );
}

#[cfg(test)]
mod tests;
