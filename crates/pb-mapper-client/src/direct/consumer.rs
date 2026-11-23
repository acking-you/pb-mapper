//! The consuming end of a direct session: whether to try direct at all, the
//! setup up to the commit point or the relay fallback, and the session's
//! forwarding and reports.
use super::{
    conn::{DirectConn, forward_direct},
    control::Control,
    endpoint::{Dialed, Discovered, Network},
    grant_fits,
    lan::Assessment,
    link::{counts, direct_link, lan_missed, log_path, stun_timings, summary},
    memory::{Cooldown, Pauses, SharedPauses},
    reports::{self, REPORT_INTERVAL, SessionReporter, Sink},
    tcp,
};
use crate::{
    client::stream::{Subscribed, subscribe_request},
    diagnostics::{
        CandidateCounts, Diagnostics, DirectAttemptGuard, DirectLink, DirectStage, FallbackReason,
        LanTcpMiss, LinkKind, SetupTimings, ms,
    },
    endpoint::RelayEndpoint,
    sdk::LanTransport,
};
use parking_lot::{MappedMutexGuard, MutexGuard};
use pb_mapper_auth::ADMIN_NAMESPACE;
use pb_mapper_core::{checksum::Credential, timeout::RetryBackoff};
use pb_mapper_protocol::{
    command::{PbConnRequest, PbConnStatusReq, PbConnStatusResp},
    data::DataCodec,
    direct::*,
    forward::{ByteMeter, StreamForward},
};
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, OnceCell},
    time::Instant,
};
use uni_stream::stream::NetworkStream;

/// How long a setup may take to get a grant: the coordinator's control
/// connection, discovery and the grant itself. Past it the connection takes
/// the relay, as [`FallbackReason::NegotiationTimeout`].
const NEGOTIATION_BUDGET: Duration = Duration::from_secs(3);
/// How long a granted setup may search for a path before it falls back.
pub(super) const PUNCH_BUDGET: Duration = Duration::from_secs(2);

/// How long one delivery of reports may take, reconnecting included.
const REPORT_ATTEMPT: Duration = Duration::from_secs(3);
/// The first wait after a delivery failed, doubling up to the interval.
const REPORT_RETRY: Duration = Duration::from_secs(1);
const _: () = assert!(
    !REPORT_RETRY.is_zero() && REPORT_INTERVAL.as_nanos() >= REPORT_RETRY.as_nanos(),
    "RetryBackoff needs a first wait, and a cap no shorter"
);

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
    /// When direct, and TCP within a LAN, may be tried again: a listener's
    /// own, or shared with its client's other `connect_tcp` calls to the
    /// service.
    pauses: SharedPauses,
    policy: crate::direct_policy::DirectPolicy,
}
impl Consumer {
    #[expect(
        clippy::too_many_arguments,
        reason = "a tunnel's settings, and the pauses it shares"
    )]
    pub fn new(
        remote: RelayEndpoint,
        credential: Credential,
        namespace: Option<u64>,
        is_datagram: bool,
        keep_alive: bool,
        diagnostics: Diagnostics,
        policy: crate::direct_policy::DirectPolicy,
        pauses: SharedPauses,
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
            pauses,
            policy,
        })
    }
    /// The pauses as they stand on the relay's network now.
    fn pauses(&self) -> MutexGuard<'_, Pauses> {
        let mut pauses = self.pauses.lock();
        pauses.on_network(self.remote.network_generation());
        pauses
    }
    fn cooldown(&self) -> MappedMutexGuard<'_, Cooldown> {
        MutexGuard::map(self.pauses(), |pauses| &mut pauses.direct)
    }
    pub(super) fn tcp_cooldown(&self) -> MappedMutexGuard<'_, Cooldown> {
        MutexGuard::map(self.pauses(), |pauses| &mut pauses.tcp)
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
        self.diagnostics.direct_cooldown(cooldown.until());
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
        .map_err(io::Error::other)?;
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
        // The fallback may have been another connection's, so say until when.
        let paused = self
            .cooldown()
            .until()
            .filter(|until| Instant::now() < *until);
        if paused.is_some() {
            self.diagnostics.direct_cooldown(paused);
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
        let negotiation = self.negotiate(key, ask_tcp, &mut attempt, &mut timings);
        let negotiated = match tokio::time::timeout_at(
            deadline.min(Instant::now() + NEGOTIATION_BUDGET),
            negotiation,
        )
        .await
        {
            Ok(Ok(Some(negotiated))) => Ok(negotiated),
            Ok(Ok(None)) => Err(FallbackReason::NotOffered),
            Ok(Err(error)) => {
                tracing::debug!(%error, "direct coordination or discovery failed before a grant");
                Err(FallbackReason::NegotiationFailed)
            }
            Err(_) => Err(FallbackReason::NegotiationTimeout),
        };
        let (control, network, discovered, grant) = match negotiated {
            Ok(negotiated) => negotiated,
            Err(reason) => {
                tracing::info!(event = "session_path_selected", path = "relay", service = %key, reason = reason.as_str());
                attempt.fell_back(reason);
                self.back_off();
                return Ok(None);
            }
        };
        let deadline = deadline.min(Instant::now() + Duration::from_millis(grant.ttl_ms));
        if !self.grant_matches(&grant, key, &network) {
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
        let direct_deadline = deadline.min(Instant::now() + PUNCH_BUDGET);
        let candidate = tokio::time::timeout_at(
            direct_deadline,
            claim(&network, &grant, direct_deadline, &lan, &mut tcp),
        )
        .await;
        // What TCP came to, with its attempts still in flight let go.
        let tcp_missed = tcp.outcome(Instant::now());
        drop(tcp);
        let (mut conn, dialed) = match candidate {
            Ok(Ok(claimed)) => claimed,
            failed => {
                tracing::debug!(reason = ?failed.as_ref().map(|r| r.as_ref().map(|_| ())), "direct attempt failed before commit");
                lan_missed(&self.diagnostics, &lan, None);
                self.tcp_settled(tcp_missed, None);
                attempt.fell_back(FallbackReason::Unreachable);
                self.back_off();
                network.punching_failed();
                return self
                    .relay(key, &control, grant, &mut pending)
                    .await
                    .map(Some);
            }
        };
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
        if link.kind != LinkKind::Lan {
            lan_missed(&self.diagnostics, &lan, Some(link.kind));
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
        }))
    }
    /// Everything up to a grant: the coordinator's control connection, this
    /// end's endpoint and discovery, and the grant itself. `None` when direct
    /// is not offered.
    async fn negotiate(
        &self,
        key: &str,
        ask_tcp: bool,
        attempt: &mut DirectAttemptGuard,
        timings: &mut SetupTimings,
    ) -> io::Result<Option<Negotiated>> {
        let stage = Instant::now();
        let Some(control) = self.control(key).await? else {
            return Ok(None);
        };
        let network = self
            .network
            .get_or_try_init(|| async {
                let network = Network::new(false, self.policy.port_mapping, &self.diagnostics)?;
                #[cfg(test)]
                if self.pauses.lock().block_quic {
                    network.block_quic();
                }
                Ok::<_, io::Error>(network)
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
        Ok(Some((control, network, discovered, grant)))
    }
    /// Whether a grant is the one this setup asked for: for this service and
    /// kind, with a usable TTL, issued to this end's key and identity, in a
    /// namespace it can check, and with a provider that offers a valid path.
    fn grant_matches(&self, grant: &SessionGrant, key: &str, network: &Network) -> bool {
        // Only a namespace this client named, or the administrator's, can be checked
        // here. A temporary credential's is the relay's to know: one issued into a
        // namespace does not carry it, and its key id names something else.
        let expected_namespace = self
            .namespace
            .or_else(|| self.credential.is_admin().then_some(ADMIN_NAMESPACE));
        grant_fits(grant, key, self.is_datagram)
            && grant.consumer_key == self.credential.key_id()
            && expected_namespace.is_none_or(|namespace| grant.namespace == namespace)
            && grant.consumer.certificate == network.identity.certificate
            && grant.provider.valid()
    }
    /// Fall back to the relay before the commit point: have the coordinator
    /// select it for the grant, then subscribe to the session there. The
    /// setup completes only once subscribed, so a failure still cancels the
    /// grant through `pending`.
    async fn relay(
        self: &Arc<Self>,
        key: &str,
        control: &Control,
        grant: SessionGrant,
        pending: &mut PendingSession,
    ) -> io::Result<Prepared> {
        control.relay(grant.id).await?;
        let Subscribed {
            stream: socket,
            codec,
            ..
        } = subscribe_request(
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
        .map_err(io::Error::other)?;
        pending.completed = true;
        tracing::info!(event = "session_path_selected", path = "relay", service = %key, reason = FallbackReason::Unreachable.as_str());
        Ok(Prepared {
            path: DataPath::Relay { socket, codec },
            grant,
            owner: self.clone(),
        })
    }
}
/// What a setup negotiated: the coordinator's control connection, this
/// end's endpoint, what it discovered, and the grant.
type Negotiated = (Arc<Control>, Arc<Network>, Discovered, SessionGrant);

/// Dial the provider for `grant`, and claim it on the path that connected:
/// the grant's id, then its binding to this TLS session. The provider
/// answers once the binding checks out and the grant is confirmed.
async fn claim(
    network: &Network,
    grant: &SessionGrant,
    deadline: Instant,
    lan: &Assessment,
    tcp: &mut tcp::Race,
) -> io::Result<(DirectConn, Dialed)> {
    let (mut conn, dialed) = network.connect(grant, deadline, lan, tcp).await?;
    tracing::debug!(id=?grant.id, peer=?conn.remote_address(), transport = conn.transport().as_str(), "direct TLS connected");
    let tag = conn.binding(grant)?;
    conn.write_all(&grant.id).await?;
    conn.write_all(&tag).await?;
    if conn.read_u8().await? != 1 {
        return Err(io::Error::other("direct path was not ready"));
    }
    Ok((conn, dialed))
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
}
impl Prepared {
    pub async fn forward<L: NetworkStream + StreamForward>(
        self,
        mut local: L,
        credential: Credential,
    ) -> io::Result<()> {
        let (path, meter, mut report) = self.open();
        let forwarding = async {
            match path {
                DataPath::Direct {
                    conn,
                    framing_key,
                    link: _,
                    _network,
                } => forward_direct(local, conn, framing_key, None, Some(meter)).await,
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
                            Some(meter),
                        )
                        .await
                        .map(|_| ())
                        .map_err(io::Error::other)
                    })
                }
            }
        };
        report.during(forwarding).await
    }
    /// Open the session in diagnostics, on its path, and start reporting it:
    /// the path to forward on, where to count its bytes, and its reporter.
    fn open(self) -> (DataPath, Arc<ByteMeter>, SessionReporter) {
        let diagnostics = &self.owner.diagnostics;
        let (path, link) = match &self.path {
            DataPath::Direct { conn, link, .. } => (
                SessionPath::Direct,
                Some(diagnostics.link_opened((**link).clone(), conn.probe())),
            ),
            DataPath::Relay { .. } => (SessionPath::Relay, None),
        };
        let summary = match &self.path {
            DataPath::Direct { link, .. } => Some(summary(link)),
            DataPath::Relay { .. } => None,
        };
        let session = diagnostics.session_opened(path, summary);
        let meter = session.meter();
        let report = SessionReporter::start(
            Sink::Consumer(self.owner),
            &self.grant,
            path,
            summary,
            session,
            link,
        );
        (self.path, meter, report)
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
    let mut delay = REPORT_INTERVAL;
    // Jittered: the consumers of every process that lost the relay together
    // deliver to the same relay again.
    let mut retry = RetryBackoff::new(REPORT_RETRY, REPORT_INTERVAL).with_jitter();
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
        match tokio::time::timeout(REPORT_ATTEMPT, attempt).await {
            Ok(Ok(())) => {
                delay = REPORT_INTERVAL;
                retry.reset();
            }
            _ => {
                // A failed reconnect must not cost another full reporting
                // interval. Retry independently of data with bounded backoff.
                delay = retry.next_delay();
            }
        }
    }
}
