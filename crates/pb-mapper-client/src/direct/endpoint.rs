use super::{
    candidates::Candidates,
    identity::Identity,
    socket::{self, Socket},
};
use pb_mapper_protocol::direct::{PeerInfo, SessionGrant, valid_candidate};
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
    sync::{Mutex, mpsc},
    task::JoinSet,
    time::Instant,
};

struct Discovery {
    at: Instant,
    server: SocketAddr,
    generation: (u64, u64),
    candidates: Candidates,
}

pub(super) struct Network {
    pub identity: Identity,
    transports: Vec<(Socket, quinn::Endpoint)>,
    incoming: Mutex<mpsc::Receiver<quinn::Connection>>,
    acceptors: Vec<tokio_util::task::AbortOnDropHandle<()>>,
    cached: Mutex<Option<Discovery>>,
    generation: AtomicU64,
    network: Arc<crate::network::NetworkHints>,
    #[cfg(feature = "port-mapping")]
    mapping: Option<super::mapping::Mapping>,
}
impl Network {
    pub fn new(server: bool, port_mapping: bool) -> io::Result<Arc<Self>> {
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
        let mapping = if port_mapping {
            let port = transports.iter().find_map(|(socket, _)| {
                socket
                    .local_addr()
                    .ok()
                    .filter(SocketAddr::is_ipv4)
                    .and_then(|addr| std::num::NonZeroU16::new(addr.port()))
            });
            port.map(|port| super::mapping::Mapping::new(port, network.clone()))
        } else {
            None
        };
        #[cfg(not(feature = "port-mapping"))]
        let _ = port_mapping;
        Ok(Arc::new(Self {
            identity,
            transports,
            incoming: Mutex::new(rx),
            acceptors,
            cached: Mutex::new(None),
            generation: AtomicU64::new(0),
            network,
            #[cfg(feature = "port-mapping")]
            mapping,
        }))
    }
    pub async fn peer(&self, stun: SocketAddr) -> PeerInfo {
        let mut cached = self.cached.lock().await;
        let generation = (
            self.generation.load(Ordering::Acquire),
            self.network.generation(),
        );
        if let Some(discovery) = &*cached
            && discovery.server == stun
            && discovery.generation == generation
            && discovery.at.elapsed() < Duration::from_secs(10)
        {
            return PeerInfo {
                certificate: self.identity.certificate.clone(),
                candidates: discovery.candidates.select(self.mapped_addr()),
            };
        }
        let mut reflexive = None;
        let mut hosts = Vec::new();
        for (socket, _) in &self.transports {
            if socket
                .local_addr()
                .is_ok_and(|a| a.is_ipv4() == stun.is_ipv4())
            {
                match socket.mapped_addr(stun).await {
                    Ok(addr) => reflexive = Some(addr),
                    Err(error) => tracing::debug!(%error, "STUN discovery unavailable"),
                }
            }
        }
        if let Ok(interfaces) = if_addrs::get_if_addrs() {
            for interface in interfaces {
                let ip = interface.ip();
                if ip.is_loopback() && !stun.ip().is_loopback() {
                    continue;
                }
                for (socket, _) in &self.transports {
                    if let Ok(local) = socket.local_addr() {
                        if local.is_ipv4() != ip.is_ipv4() {
                            continue;
                        }
                        let addr = SocketAddr::new(ip, local.port());
                        if valid_candidate(&addr) {
                            hosts.push(addr);
                        }
                    }
                }
            }
        }
        let candidates = Candidates::new(reflexive, hosts);
        let selected = candidates.select(self.mapped_addr());
        tracing::debug!(%stun, candidates = ?selected, "direct candidates discovered");
        *cached = Some(Discovery {
            at: Instant::now(),
            server: stun,
            generation,
            candidates,
        });
        PeerInfo {
            certificate: self.identity.certificate.clone(),
            candidates: selected,
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
        if let Some(mapping) = &self.mapping {
            return mapping.address();
        }
        None
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
        for (socket, _) in &self.transports {
            socket.authorize(grant.id, grant.secret, deadline);
        }
        Ok(())
    }
    pub fn forget(&self, id: &pb_mapper_protocol::direct::SessionId) {
        for (socket, _) in &self.transports {
            socket.forget(id);
        }
    }
    pub fn probe(&self, grant: &SessionGrant, provider: bool) -> Vec<SocketAddr> {
        let peer = if provider {
            &grant.consumer
        } else {
            &grant.provider
        };
        self.transports
            .iter()
            .flat_map(|(socket, _)| socket.probe(grant.id, &peer.candidates))
            .collect()
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
        if let Some(mapping) = &self.mapping {
            mapping.shutdown().await;
        }
    }
    pub async fn connect(
        &self,
        grant: &SessionGrant,
        deadline: Instant,
    ) -> io::Result<quinn::Connection> {
        let config = self.identity.client(&grant.provider.certificate)?;
        let mut attempts = JoinSet::new();
        let mut tried = Vec::new();
        let mut probe = tokio::time::interval(Duration::from_millis(120));
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "direct path timed out"));
                }
                _ = probe.tick() => {
                    // Only race paths that answered our authenticated probe.
                    // Blind QUIC flights to stale interface addresses can fill
                    // one shared UDP socket's send queue and stall healthy sessions.
                    let candidates = self.probe(grant, false);
                    // At most 16 handshakes per setup: an attempt is bounded
                    // work, and the setup deadline ends the search anyway.
                    for addr in candidates {
                        if tried.contains(&addr) || tried.len() >= 16 {
                            continue;
                        }
                        tried.push(addr);
                        let endpoint = self.transports.iter().find(|(socket, _)| {
                            socket.local_addr().is_ok_and(|a| a.is_ipv4() == addr.is_ipv4())
                        });
                        if let Some((_, endpoint)) = endpoint
                            && let Ok(connecting) =
                                endpoint.connect_with(config.clone(), addr, "pb-mapper.invalid")
                        {
                            attempts.spawn(connecting);
                        }
                    }
                }
                Some(result) = attempts.join_next(), if !attempts.is_empty() => {
                    match result {
                        Ok(Ok(connection)) => return Ok(connection),
                        // A systematic identity or config mismatch must be
                        // distinguishable from an unreachable path.
                        Ok(Err(error)) => tracing::debug!(%error, "direct QUIC attempt failed"),
                        Err(error) => tracing::debug!(%error, "direct QUIC attempt aborted"),
                    }
                }
            }
        }
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
