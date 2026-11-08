use super::{
    candidates::{Candidates, Host},
    identity::Identity,
    lan::{Assessment, Interfaces, Local},
    socket::{self, Socket},
};
use crate::diagnostics::{LanEvidence, StunOutcome, ms};
use pb_mapper_protocol::direct::{PeerInfo, SessionGrant, SessionId};
use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Notify, mpsc},
    task::JoinSet,
    time::Instant,
};

/// How long a path that is not on the LAN waits after its first answer,
/// when the peer has shown it shares this end's network: a LAN handshake
/// that finishes in the meantime saves a handshake to every other path.
pub(super) const LAN_HEAD_START: Duration = Duration::from_millis(150);
/// When paths on the LAN are probed again after the first round, on any
/// evidence of one: a first probe lost to Wi-Fi, or dropped by a firewall
/// the peer's own probe had not opened yet, costs 20 ms, not 120.
const LAN_REPROBES: [Duration; 2] = [Duration::from_millis(20), Duration::from_millis(50)];

struct Discovery {
    at: Instant,
    server: SocketAddr,
    generation: (u64, u64),
    candidates: Candidates,
    local: Arc<Local>,
}

/// What this end offers for one setup, and what that took.
pub(super) struct Discovered {
    pub peer: PeerInfo,
    pub local: Arc<Local>,
    /// Candidates offered, on a LAN interface, and left out past the limit.
    pub counts: (u32, u32, u32),
    pub stun: StunOutcome,
    pub stun_ms: Option<u64>,
}

/// How the search for a path went, once one connected.
pub(super) struct Dialed {
    pub probed: u32,
    pub answered: u32,
    pub handshakes: u32,
    /// From the start of the search to the first answer, and the first on a
    /// local network.
    pub first_pong: Option<Duration>,
    pub lan_first_pong: Option<Duration>,
    /// The winning handshake.
    pub handshake: Duration,
}

/// Where this machine's interface table comes from.
pub(super) enum InterfaceSource {
    System,
    #[cfg(test)]
    Fixed(Interfaces),
}

pub(super) struct Network {
    pub identity: Identity,
    transports: Vec<(Socket, quinn::Endpoint)>,
    incoming: Mutex<mpsc::Receiver<quinn::Connection>>,
    acceptors: Vec<tokio_util::task::AbortOnDropHandle<()>>,
    cached: Mutex<Option<Discovery>>,
    generation: AtomicU64,
    network: Arc<crate::network::NetworkHints>,
    pub(super) interfaces: InterfaceSource,
    #[cfg(feature = "port-mapping")]
    mapper: Option<Mapper>,
}

/// The router mapping this endpoint may ask for, and the one it asked for.
#[cfg(feature = "port-mapping")]
struct Mapper {
    port: std::num::NonZeroU16,
    network: Arc<crate::network::NetworkHints>,
    diagnostics: crate::diagnostics::Diagnostics,
    started: std::sync::OnceLock<super::mapping::Mapping>,
}

#[cfg(feature = "port-mapping")]
impl Mapper {
    fn start(&self) -> &super::mapping::Mapping {
        self.started.get_or_init(|| {
            super::mapping::Mapping::new(self.port, self.network.clone(), self.diagnostics.clone())
        })
    }
}

impl Network {
    /// Open the endpoint. As `port_mapping` says, ask the router to map its
    /// IPv4 port now, after a failure to punch through, or never, reporting
    /// into `diagnostics`.
    pub fn new(
        server: bool,
        port_mapping: crate::sdk::PortMapping,
        diagnostics: &crate::diagnostics::Diagnostics,
    ) -> io::Result<Arc<Self>> {
        let identity = Identity::new()?;
        let (tx, rx) = mpsc::channel(64);
        let mut transports = Vec::new();
        let mut acceptors = Vec::new();
        for addr in [
            SocketAddr::from(([0, 0, 0, 0], 0)),
            SocketAddr::from(([0u16; 8], 0)),
        ] {
            let (socket, quic) = match Socket::bind(addr) {
                Ok(s) => s,
                Err(error) if addr.is_ipv6() => {
                    tracing::debug!(%error, "IPv6 direct socket unavailable");
                    continue;
                }
                Err(error) => return Err(error),
            };
            let config = if server {
                Some(identity.server()?)
            } else {
                None
            };
            let endpoint = quinn::Endpoint::new_with_abstract_socket(
                socket::endpoint_config(),
                config,
                Box::new(quic),
                Arc::new(quinn::TokioRuntime),
            )?;
            if server {
                let ep = endpoint.clone();
                let tx = tx.clone();
                acceptors.push(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                    let mut handshakes = JoinSet::new();
                    loop {
                        tokio::select! {
                            incoming = ep.accept() => {
                                let Some(incoming) = incoming else { break };
                                if handshakes.len() >= 64 {
                                    incoming.refuse();
                                    continue;
                                }
                                let tx = tx.clone();
                                handshakes.spawn(async move {
                                    match tokio::time::timeout(Duration::from_secs(3), incoming).await {
                                        Ok(Ok(connection)) => {
                                            if let Err(error) = tx.try_send(connection) {
                                                error.into_inner().close(0u32.into(), b"accept capacity");
                                            }
                                        }
                                        result => tracing::debug!(?result, "direct TLS accept failed"),
                                    }
                                });
                            }
                            Some(_) = handshakes.join_next() => {}
                        }
                    }
                })));
            }
            transports.push((socket, endpoint));
        }
        let network = crate::network::NetworkHints::shared();
        #[cfg(feature = "port-mapping")]
        let mapper = (port_mapping != crate::sdk::PortMapping::Off)
            .then(|| {
                transports.iter().find_map(|(socket, _)| {
                    socket
                        .local_addr()
                        .ok()
                        .filter(SocketAddr::is_ipv4)
                        .and_then(|addr| std::num::NonZeroU16::new(addr.port()))
                })
            })
            .flatten()
            .map(|port| Mapper {
                port,
                network: network.clone(),
                diagnostics: diagnostics.clone(),
                started: std::sync::OnceLock::new(),
            });
        #[cfg(feature = "port-mapping")]
        if let Some(mapper) = &mapper
            && port_mapping == crate::sdk::PortMapping::Always
        {
            mapper.start();
        }
        #[cfg(not(feature = "port-mapping"))]
        let _ = (port_mapping, diagnostics);
        Ok(Arc::new(Self {
            identity,
            transports,
            incoming: Mutex::new(rx),
            acceptors,
            cached: Mutex::new(None),
            generation: AtomicU64::new(0),
            network,
            interfaces: InterfaceSource::System,
            #[cfg(feature = "port-mapping")]
            mapper,
        }))
    }
    pub async fn peer(&self, stun: SocketAddr) -> Discovered {
        let mut cached = self.cached.lock().await;
        let generation = (
            self.generation.load(Ordering::Acquire),
            self.network.generation(),
        );
        let mapped = self.mapped_addr();
        if let Some(discovery) = &*cached
            && discovery.server == stun
            && discovery.generation == generation
            && discovery.at.elapsed() < Duration::from_secs(10)
        {
            return self.discovered(discovery, mapped, StunOutcome::Cached, None);
        }
        let mut reflexive = None;
        let mut stun_outcome = StunOutcome::Unavailable;
        let started = Instant::now();
        for (socket, _) in &self.transports {
            if socket
                .local_addr()
                .is_ok_and(|a| a.is_ipv4() == stun.is_ipv4())
            {
                match socket.mapped_addr(stun).await {
                    Ok(addr) => {
                        reflexive = Some(addr);
                        stun_outcome = StunOutcome::Ok;
                    }
                    Err(error) => {
                        tracing::debug!(%error, "STUN discovery unavailable");
                        if error.kind() == io::ErrorKind::TimedOut {
                            stun_outcome = StunOutcome::Timeout;
                        }
                    }
                }
            }
        }
        let stun_ms = (stun_outcome != StunOutcome::Unavailable).then(|| ms(started.elapsed()));
        let interfaces = match &self.interfaces {
            InterfaceSource::System => Interfaces::system(self.network.generation()),
            #[cfg(test)]
            InterfaceSource::Fixed(table) => table.clone(),
        };
        let mut hosts = Vec::new();
        for (index, interface) in interfaces.iter().enumerate() {
            for &(ip, _) in &interface.addrs {
                if ip.is_loopback() && !stun.ip().is_loopback() {
                    continue;
                }
                for (socket, _) in &self.transports {
                    if let Ok(local) = socket.local_addr()
                        && local.is_ipv4() == ip.is_ipv4()
                    {
                        hosts.push(Host {
                            addr: SocketAddr::new(ip, local.port()),
                            tier: interface.tier,
                            interface: index,
                        });
                    }
                }
            }
        }
        let candidates = Candidates::new(reflexive, hosts);
        let local = Arc::new(Local {
            interfaces,
            public: reflexive.map(|addr| addr.ip()).into_iter().collect(),
        });
        let discovery = Discovery {
            at: Instant::now(),
            server: stun,
            generation,
            candidates,
            local,
        };
        let discovered = self.discovered(&discovery, mapped, stun_outcome, stun_ms);
        tracing::debug!(%stun, candidates = ?discovered.peer.candidates, "direct candidates discovered");
        *cached = Some(discovery);
        discovered
    }
    fn discovered(
        &self,
        discovery: &Discovery,
        mapped: Option<SocketAddr>,
        stun: StunOutcome,
        stun_ms: Option<u64>,
    ) -> Discovered {
        let selected = discovery.candidates.select(mapped);
        let mut local = discovery.local.clone();
        // The router's address is where this end is seen from too.
        if let Some(mapped) = mapped.map(|addr| addr.ip())
            && !local.public.contains(&mapped)
        {
            Arc::make_mut(&mut local).public.push(mapped);
        }
        Discovered {
            counts: (
                selected.addrs.len() as u32,
                selected.lan,
                selected.truncated,
            ),
            peer: PeerInfo {
                certificate: self.identity.certificate.clone(),
                candidates: selected.addrs,
            },
            local,
            stun,
            stun_ms,
        }
    }
    #[cfg(test)]
    pub fn block_quic(&self) {
        for (socket, _) in &self.transports {
            socket.block_quic();
        }
    }
    pub fn invalidate(&self) {
        // Discovery holds the cache lock across STUN. A generation also
        // invalidates an in-flight result instead of losing the notification.
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
    fn mapped_addr(&self) -> Option<SocketAddr> {
        #[cfg(feature = "port-mapping")]
        if let Some(mapping) = self.mapper.as_ref().and_then(|m| m.started.get()) {
            return mapping.address();
        }
        None
    }
    /// A direct attempt failed to punch through: a sign that a NAT drops what
    /// it has not seen. A tunnel that maps its port when needed asks the
    /// router now, so that later sessions can offer the mapped address.
    pub fn punching_failed(&self) {
        #[cfg(feature = "port-mapping")]
        if let Some(mapper) = &self.mapper
            && mapper.started.get().is_none()
        {
            tracing::debug!(
                event = "port_mapping_started",
                "direct punching failed; asking the router for a port mapping"
            );
            mapper.start();
        }
    }
    pub fn authorize(
        &self,
        grant: &SessionGrant,
        provider: bool,
        deadline: Instant,
    ) -> io::Result<()> {
        if provider {
            self.identity
                .permissions
                .allow(grant.consumer.certificate.clone(), deadline)?;
        }
        // One wake for the session, whichever socket hears from the peer.
        let wake = Arc::new(Notify::new());
        for (socket, _) in &self.transports {
            socket.authorize(grant.id, grant.secret, deadline, wake.clone());
        }
        Ok(())
    }
    pub fn forget(&self, id: &SessionId) {
        for (socket, _) in &self.transports {
            socket.forget(id);
        }
    }
    /// What wakes `id`'s session when a path answers or a source is owed a
    /// challenge.
    pub fn wake(&self, id: &SessionId) -> Arc<Notify> {
        self.transports
            .iter()
            .find_map(|(socket, _)| socket.wake(id))
            .unwrap_or_default()
    }
    /// Probe `candidates` in order, on the socket of each one's family, and
    /// any source owed a challenge. Returns those the system refused.
    pub fn probe(&self, id: SessionId, candidates: &[SocketAddr]) -> Vec<SocketAddr> {
        self.transports
            .iter()
            .flat_map(|(socket, _)| socket.probe(id, candidates))
            .collect()
    }
    /// The paths that have answered so far, on both families, and when.
    pub fn learned(&self, id: &SessionId) -> Vec<(SocketAddr, Instant)> {
        self.transports
            .iter()
            .flat_map(|(socket, _)| socket.learned(id))
            .collect()
    }
    /// How a peer's candidates are probed: every 120 ms all of them, those on
    /// a local network first, and on evidence of one those again shortly
    /// after the first round.
    fn probing(candidates: &[SocketAddr], lan: &Assessment) -> (Vec<SocketAddr>, Vec<SocketAddr>) {
        let ordered = lan.order(candidates);
        let reprobed = ordered
            .iter()
            .copied()
            .filter(|addr| lan.is_lan(*addr))
            .collect();
        (ordered, reprobed)
    }
    /// How many of `candidates` a socket of this end can probe.
    pub fn reachable(&self, candidates: &[SocketAddr]) -> u32 {
        candidates
            .iter()
            .filter(|addr| {
                self.transports.iter().any(|(socket, _)| {
                    socket
                        .local_addr()
                        .is_ok_and(|local| local.is_ipv4() == addr.is_ipv4())
                })
            })
            .count() as u32
    }
    /// Keep probing a peer for the provider, which is dialed rather than
    /// dialing: its probes open its own NAT and firewall for the consumer's,
    /// and answer the sources the consumer's probes come from.
    pub async fn keep_probing(&self, grant: &SessionGrant, lan: &Assessment) {
        let id = grant.id;
        let (ordered, reprobed) = Self::probing(&grant.consumer.candidates, lan);
        let wake = self.wake(&id);
        let started = Instant::now();
        let mut reprobes = reprobe_times(started, lan.evidence, &reprobed);
        let mut probes = tokio::time::interval(Duration::from_millis(120));
        loop {
            let reprobe = reprobes.first().copied();
            tokio::select! {
                _ = probes.tick() => {
                    self.probe(id, &ordered);
                }
                () = sleep_until_some(reprobe) => {
                    reprobes.remove(0);
                    self.probe(id, &reprobed);
                }
                () = wake.notified() => {
                    self.probe(id, &[]);
                }
            }
        }
    }
    pub async fn accept(&self) -> Option<quinn::Connection> {
        self.incoming.lock().await.recv().await
    }
    pub async fn shutdown(&self) {
        for (_, endpoint) in &self.transports {
            endpoint.close(0u32.into(), b"local tunnel stopped");
        }
        // Keep the endpoints, and so their drivers, alive while Quinn sends and
        // retransmits CLOSE; dropping them with the last session can leave the
        // peer waiting for its idle timeout. Bound draining so an unreachable
        // peer cannot prevent a local stop.
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            for (_, endpoint) in &self.transports {
                endpoint.wait_idle().await;
            }
        })
        .await;
        #[cfg(feature = "port-mapping")]
        if let Some(mapping) = self.mapper.as_ref().and_then(|m| m.started.get()) {
            mapping.shutdown().await;
        }
    }
    /// Dial the provider over each path as soon as it answers a probe, until
    /// one handshake completes. Only paths that answered are raced: blind
    /// QUIC flights to stale interface addresses can fill one shared UDP
    /// socket's send queue and stall healthy sessions.
    pub async fn connect(
        &self,
        grant: &SessionGrant,
        deadline: Instant,
        lan: &Assessment,
    ) -> io::Result<(quinn::Connection, Dialed)> {
        let id = grant.id;
        let started = Instant::now();
        let config = self.identity.client(&grant.provider.certificate)?;
        let (ordered, reprobed) = Self::probing(&grant.provider.candidates, lan);
        let probed = self.reachable(&ordered);
        let wake = self.wake(&id);
        let mut reprobes = reprobe_times(started, lan.evidence, &reprobed);
        let mut attempts = JoinSet::new();
        let mut tried = Vec::new();
        // LAN handshakes started and failed, and whether the system refused
        // to send to a LAN address: either way the head start is over.
        let (mut lan_dialed, mut lan_failed, mut lan_refused) = (0, 0, false);
        let mut probe = tokio::time::interval(Duration::from_millis(120));
        loop {
            let now = Instant::now();
            let mut held: Option<Instant> = None;
            for (addr, answered) in self.learned(&id) {
                // At most 16 handshakes per setup: an attempt is bounded
                // work, and the setup deadline ends the search anyway.
                if tried.contains(&addr) || tried.len() >= 16 {
                    continue;
                }
                let on_lan = lan.is_lan(addr);
                let settled = lan_refused || (lan_dialed > 0 && lan_failed == lan_dialed);
                let since = now.saturating_duration_since(answered);
                if !dial_now(lan.evidence, on_lan, since, settled) {
                    let at = answered + LAN_HEAD_START;
                    held = Some(held.map_or(at, |held| held.min(at)));
                    continue;
                }
                tried.push(addr);
                let endpoint = self.transports.iter().find(|(socket, _)| {
                    socket
                        .local_addr()
                        .is_ok_and(|a| a.is_ipv4() == addr.is_ipv4())
                });
                if let Some((_, endpoint)) = endpoint
                    && let Ok(connecting) =
                        endpoint.connect_with(config.clone(), addr, "pb-mapper.invalid")
                {
                    lan_dialed += u32::from(on_lan);
                    attempts.spawn(async move {
                        let dialed = Instant::now();
                        (addr, connecting.await, dialed.elapsed())
                    });
                }
            }
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "direct path timed out"));
                }
                _ = probe.tick() => {
                    let refused = self.probe(id, &ordered);
                    lan_refused |= refused.iter().any(|addr| lan.is_lan(*addr));
                }
                () = sleep_until_some(reprobes.first().copied()) => {
                    reprobes.remove(0);
                    let refused = self.probe(id, &reprobed);
                    lan_refused |= refused.iter().any(|addr| lan.is_lan(*addr));
                }
                // A source owed a challenge gets it now; a path that answered
                // is dialed above.
                () = wake.notified() => {
                    self.probe(id, &[]);
                }
                () = sleep_until_some(held) => {}
                Some(result) = attempts.join_next(), if !attempts.is_empty() => {
                    match result {
                        Ok((_, Ok(connection), handshake)) => {
                            let learned = self.learned(&id);
                            let first = |lan_only: bool| {
                                learned
                                    .iter()
                                    .filter(|(addr, _)| !lan_only || lan.is_lan(*addr))
                                    .map(|(_, at)| at.saturating_duration_since(started))
                                    .min()
                            };
                            return Ok((connection, Dialed {
                                probed,
                                answered: learned.len() as u32,
                                handshakes: tried.len() as u32,
                                first_pong: first(false),
                                lan_first_pong: first(true),
                                handshake,
                            }));
                        }
                        // A systematic identity or config mismatch must be
                        // distinguishable from an unreachable path.
                        Ok((addr, Err(error), _)) => {
                            lan_failed += u32::from(lan.is_lan(addr));
                            tracing::debug!(%error, %addr, "direct QUIC attempt failed");
                        }
                        Err(error) => tracing::debug!(%error, "direct QUIC attempt aborted"),
                    }
                }
            }
        }
    }
}
/// Whether a path that answered `since` ago may be dialed now. Only strong
/// evidence of a shared network holds a path off it back, and only for
/// [`LAN_HEAD_START`] or until the LAN has failed.
pub(super) fn dial_now(
    evidence: Option<LanEvidence>,
    on_lan: bool,
    since: Duration,
    lan_failed: bool,
) -> bool {
    on_lan || lan_failed || since >= LAN_HEAD_START || !evidence.is_some_and(LanEvidence::strong)
}

/// When LAN paths are probed again, on any evidence of a shared network.
fn reprobe_times(
    started: Instant,
    evidence: Option<LanEvidence>,
    lan: &[SocketAddr],
) -> Vec<Instant> {
    if evidence.is_none() || lan.is_empty() {
        return Vec::new();
    }
    LAN_REPROBES.iter().map(|after| started + *after).collect()
}

async fn sleep_until_some(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        for (_, ep) in &self.transports {
            ep.close(0u32.into(), b"local tunnel stopped");
        }
        self.acceptors.clear();
    }
}

pub(super) fn binding(
    connection: &quinn::Connection,
    grant: &SessionGrant,
) -> io::Result<[u8; 32]> {
    let exporter = binding_exporter(connection, grant)?;
    let tag = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &grant.secret.0),
        &exporter,
    );
    tag.as_ref().try_into().map_err(io::Error::other)
}
pub(super) fn verify_binding(
    connection: &quinn::Connection,
    grant: &SessionGrant,
    tag: &[u8],
) -> io::Result<()> {
    let exporter = binding_exporter(connection, grant)?;
    ring::hmac::verify(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &grant.secret.0),
        &exporter,
        tag,
    )
    .map_err(|_| io::Error::other("TLS session binding mismatch"))
}
fn binding_exporter(connection: &quinn::Connection, grant: &SessionGrant) -> io::Result<[u8; 32]> {
    use ring::digest;
    let context = serde_json::to_vec(&(
        grant.id,
        grant.server_instance,
        grant.generation,
        grant.namespace,
        &grant.service,
        grant.is_datagram,
        grant.consumer_key,
        grant.provider_key,
        digest::digest(&digest::SHA256, &grant.consumer.certificate).as_ref(),
        digest::digest(&digest::SHA256, &grant.provider.certificate).as_ref(),
    ))
    .map_err(io::Error::other)?;
    let mut exporter = [0; 32];
    connection
        .export_keying_material(&mut exporter, b"pb-mapper/direct/1", &context)
        .map_err(|_| io::Error::other("TLS exporter unavailable"))?;
    Ok(exporter)
}
pub(super) fn framing_key(
    connection: &quinn::Connection,
    grant: &SessionGrant,
) -> io::Result<[u8; 32]> {
    let mut key = [0; 32];
    connection
        .export_keying_material(&mut key, b"pb-mapper/framing/1", &grant.id)
        .map_err(|_| io::Error::other("TLS framing exporter unavailable"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{diagnostics::Diagnostics, sdk::PortMapping};
    use stun_proto::types::{
        attribute::XorMappedAddress,
        message::{Message, MessageWrite, MessageWriteExt, MessageWriteVec},
    };
    use tokio::net::UdpSocket;

    async fn respond(stun: &UdpSocket, mapped: SocketAddr, invalidate: Option<&Network>) {
        let mut buffer = [0; 2048];
        let (len, from) = stun.recv_from(&mut buffer).await.unwrap();
        // Receipt of the actual request proves discovery owns the cache lock.
        if let Some(network) = invalidate {
            network.invalidate();
        }
        let request = Message::from_bytes(&buffer[..len]).unwrap();
        let mut response = Message::builder_success(&request, MessageWriteVec::new());
        response
            .add_attribute(&XorMappedAddress::new(mapped, request.transaction_id()))
            .unwrap();
        response.add_fingerprint().unwrap();
        stun.send_to(&response.finish(), from).await.unwrap();
    }

    #[tokio::test]
    async fn discovery_invalidation_survives_in_flight_stun_and_coalesces_refresh() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut network =
                Network::new(false, PortMapping::Off, &Diagnostics::default()).unwrap();
            Arc::get_mut(&mut network).unwrap().network = crate::network::NetworkHints::isolated();
            let stun = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let server = stun.local_addr().unwrap();
            let old = "192.0.2.10:40001".parse().unwrap();
            let new = "192.0.2.10:40002".parse().unwrap();
            let (first, ()) =
                tokio::join!(network.peer(server), respond(&stun, old, Some(&network)));
            assert_eq!(first.peer.candidates[0], old);

            // Both waiters must share one fresh lookup; neither may reuse old.
            let (first, second, ()) = tokio::join!(
                network.peer(server),
                network.peer(server),
                respond(&stun, new, None)
            );
            let first = first.peer;
            assert_eq!(first.candidates[0], new);
            assert_eq!(second.peer.candidates, first.candidates);
            assert_eq!(network.peer(server).await.peer.candidates, first.candidates);
            assert_eq!(
                stun.try_recv_from(&mut [0; 2048]).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );

            // An idle cache must also observe the shared network generation,
            // including on providers that do not call Consumer::invalidate.
            network.network.notify();
            let refreshed = "192.0.2.10:40003".parse().unwrap();
            let (peer, ()) = tokio::join!(network.peer(server), respond(&stun, refreshed, None));
            assert_eq!(peer.peer.candidates[0], refreshed);
            network.shutdown().await;
        })
        .await
        .unwrap();
    }

    #[test]
    fn only_strong_evidence_holds_other_paths_back_and_never_for_long() {
        let strong = Some(LanEvidence::PublicIpSubnet);
        let soon = Duration::from_millis(10);
        // No evidence, or weak evidence, dials whatever answers at once.
        assert!(dial_now(None, false, Duration::ZERO, false));
        assert!(dial_now(Some(LanEvidence::Subnet), false, soon, false));
        // Strong evidence: the LAN at once, the rest after the head start,
        // or as soon as the LAN has failed.
        assert!(dial_now(strong, true, Duration::ZERO, false));
        assert!(!dial_now(strong, false, soon, false));
        assert!(dial_now(strong, false, LAN_HEAD_START, false));
        assert!(dial_now(strong, false, soon, true));
    }

    /// Where punching works, a tunnel that maps when needed never asks the
    /// router: the endpoint opens with its mapping on standby.
    #[cfg(feature = "port-mapping")]
    #[tokio::test]
    async fn an_endpoint_that_maps_when_needed_waits_for_a_punching_failure() {
        let network = Network::new(false, PortMapping::Auto, &Diagnostics::default()).unwrap();
        let mapper = network.mapper.as_ref().unwrap();
        assert!(mapper.started.get().is_none());
        assert_eq!(network.mapped_addr(), None);
        let off = Network::new(false, PortMapping::Off, &Diagnostics::default()).unwrap();
        assert!(off.mapper.is_none());
        // Without a failure there is nothing to start; with one, the router is
        // asked, which this test leaves to the end-to-end NAT checks.
    }
}
