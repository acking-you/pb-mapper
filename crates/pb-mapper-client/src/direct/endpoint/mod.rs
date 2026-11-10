//! A tunnel's direct endpoint: its UDP sockets and QUIC endpoints, the QUIC
//! and TCP connections it accepts, and its router mapping. Discovery and
//! dialing are its child modules.
use super::{
    identity::Identity,
    socket::{self, Socket},
    tcp::{self, TlsTcp},
};
use pb_mapper_protocol::direct::{SessionGrant, SessionId};
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};
use tokio::{
    sync::{Mutex, Notify, mpsc},
    task::JoinSet,
    time::Instant,
};

mod dial;
mod discovery;

pub(super) use dial::Dialed;
pub(super) use discovery::Discovered;
use discovery::Discovery;

/// Handshakes an acceptor runs at once, on either carrier; past it, a new
/// connection is turned away.
pub(super) const MAX_HANDSHAKES: usize = 64;
/// How long one accepted handshake may take, on either carrier.
pub(super) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// Accepted connections waiting for the provider; past it, one is closed.
const ARRIVAL_QUEUE: usize = 64;
/// How long a stopping endpoint keeps its drivers alive to close with its
/// peers.
const CLOSE_DRAIN: Duration = Duration::from_secs(3);

/// Where this machine's interface table comes from.
pub(super) enum InterfaceSource {
    System,
    #[cfg(test)]
    Fixed(super::lan::Interfaces),
}

/// A connection a provider accepted, with its TLS handshake done, before the
/// session it claims is known.
pub(super) enum Arrival {
    Quic(quinn::Connection),
    Tcp {
        stream: Box<TlsTcp>,
        /// The handshake's round trip, which stands in for TCP's figures.
        rtt: Duration,
    },
}

pub(super) struct Network {
    pub identity: Identity,
    transports: Vec<(Socket, quinn::Endpoint)>,
    /// Accepted connections on either carrier, in one queue.
    incoming: Mutex<mpsc::Receiver<Arrival>>,
    arrivals: mpsc::Sender<Arrival>,
    acceptors: Vec<tokio_util::task::AbortOnDropHandle<()>>,
    /// A provider's TCP listeners, each set opened the first time a consumer
    /// asks for TCP: on a shared network, wherever the relay is, or on this
    /// computer under a relay on loopback. `None` if they could not be.
    lan_tcp: std::sync::OnceLock<Option<tcp::Acceptor>>,
    local_tcp: std::sync::OnceLock<Option<tcp::Acceptor>>,
    cached: Mutex<Option<Discovery>>,
    generation: AtomicU64,
    network: Arc<crate::network::NetworkHints>,
    pub(super) interfaces: InterfaceSource,
    #[cfg(feature = "port-mapping")]
    mapper: Option<Mapper>,
    /// Where a consumer's TCP attempts go instead of the provider's
    /// addresses: a port nothing listens on refuses them.
    #[cfg(test)]
    pub(super) tcp_redirect: std::sync::OnceLock<SocketAddr>,
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
        let (tx, rx) = mpsc::channel(ARRIVAL_QUEUE);
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
                let accept = accept_quic(endpoint.clone(), tx.clone());
                acceptors.push(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
                    accept,
                )));
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
            arrivals: tx,
            acceptors,
            lan_tcp: std::sync::OnceLock::new(),
            local_tcp: std::sync::OnceLock::new(),
            cached: Mutex::new(None),
            generation: AtomicU64::new(0),
            network,
            interfaces: InterfaceSource::System,
            #[cfg(feature = "port-mapping")]
            mapper,
            #[cfg(test)]
            tcp_redirect: std::sync::OnceLock::new(),
        }))
    }
    /// This end's TCP acceptor for a consumer on its LAN, listening on every
    /// interface, or with `loopback` for one on this computer, listening on
    /// loopback alone; opened on first use, and admitting only what
    /// [`tcp::Acceptor::offer`] lets in.
    pub fn lan_tcp(&self, loopback: bool) -> Option<&tcp::Acceptor> {
        let acceptor = if loopback {
            &self.local_tcp
        } else {
            &self.lan_tcp
        };
        acceptor
            .get_or_init(|| {
                let udp: Vec<_> = self
                    .transports
                    .iter()
                    .filter_map(|(socket, _)| socket.local_addr().ok())
                    .collect();
                tcp::Acceptor::open(&udp, loopback, &self.identity, self.arrivals.clone())
                    .inspect_err(|error| tracing::debug!(%error, "direct TCP unavailable"))
                    .ok()
            })
            .as_ref()
    }
    /// Whether TCP listeners were ever opened.
    #[cfg(test)]
    pub fn lan_tcp_opened(&self) -> bool {
        self.lan_tcp.get().is_some() || self.local_tcp.get().is_some()
    }
    #[cfg(test)]
    pub fn block_quic(&self) {
        for (socket, _) in &self.transports {
            socket.block_quic();
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
    pub async fn accept(&self) -> Option<Arrival> {
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
        let _ = tokio::time::timeout(CLOSE_DRAIN, async {
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
}

/// Accept QUIC connections for as long as `endpoint` lives, refusing those
/// past [`MAX_HANDSHAKES`] at once.
async fn accept_quic(endpoint: quinn::Endpoint, arrivals: mpsc::Sender<Arrival>) {
    let mut handshakes = JoinSet::new();
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                if handshakes.len() >= MAX_HANDSHAKES {
                    incoming.refuse();
                    continue;
                }
                handshakes.spawn(handshake(incoming, arrivals.clone()));
            }
            Some(_) = handshakes.join_next() => {}
        }
    }
}

/// Finish one QUIC handshake and hand the connection to `arrivals`, or close
/// it when they are full.
async fn handshake(incoming: quinn::Incoming, arrivals: mpsc::Sender<Arrival>) {
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming).await {
        Ok(Ok(connection)) => {
            if let Err(error) = arrivals.try_send(Arrival::Quic(connection))
                && let Arrival::Quic(connection) = error.into_inner()
            {
                connection.close(0u32.into(), b"accept capacity");
            }
        }
        result => tracing::debug!(?result, "direct TLS accept failed"),
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

#[cfg(all(test, feature = "port-mapping"))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::{diagnostics::Diagnostics, sdk::PortMapping};

    /// Where punching works, a tunnel that maps when needed never asks the
    /// router: the endpoint opens with its mapping on standby.
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
