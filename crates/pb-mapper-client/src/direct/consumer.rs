//! The consuming end of a direct session: whether to try direct at all, the
//! setup up to the commit point or the relay fallback, and the session's
//! forwarding and reports.
use super::{
    conn::{DirectConn, forward_direct},
    control::{Control, io_error},
    endpoint::Network,
    lan::Assessment,
    link::{counts, direct_link, log_path, stun_timings, summary},
    reports, tcp,
};
use crate::{
    diagnostics::{
        CandidateCounts, Diagnostics, DirectLink, DirectStage, FallbackReason, LanTcpMiss,
        LinkGuard, LinkKind, SessionGuard, SetupTimings, ms,
    },
    endpoint::RelayEndpoint,
    sdk::LanTransport,
};
use pb_mapper_auth::ADMIN_NAMESPACE;
use pb_mapper_core::checksum::Credential;
use pb_mapper_protocol::{
    command::{PbConnRequest, PbConnStatusReq, PbConnStatusResp},
    data::DataCodec,
    direct::*,
    forward::StreamForward,
};
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, OnceCell},
    time::Instant,
};
use uni_stream::stream::NetworkStream;

/// First pause after a direct setup falls back to the relay.
pub(super) const COOLDOWN_INITIAL: Duration = Duration::from_secs(10);
/// Longest pause between direct attempts on a network that keeps refusing them.
pub(super) const COOLDOWN_MAX: Duration = Duration::from_secs(300);

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
pub(super) struct Cooldown {
    until: Option<Instant>,
    next: Duration,
}

impl Cooldown {
    pub(super) fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// Start the next pause, unless one is running. A burst of connections
    /// that were all mid-attempt when the path failed is one failure, not one
    /// per connection: counting each would jump straight to the cap.
    pub(super) fn back_off(&mut self, now: Instant) {
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

    pub(super) fn clear(&mut self) {
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
    pub(super) reports: Arc<reports::Latest>,
    pub(super) report_wake: Arc<tokio::sync::Notify>,
    _reporter: tokio_util::task::AbortOnDropHandle<()>,
    pub(super) network: OnceCell<Arc<Network>>,
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
    pub(super) fn tcp_cooldown(&self) -> std::sync::MutexGuard<'_, Cooldown> {
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

pub(super) enum DataPath {
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
    pub(super) path: DataPath,
    pub(super) grant: SessionGrant,
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
