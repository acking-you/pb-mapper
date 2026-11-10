//! STUN discovery and its cache: what this end offers for a setup.
use super::{InterfaceSource, Network};
use crate::{
    diagnostics::{StunOutcome, ms},
    direct::{
        candidates::{Candidates, Host},
        lan::{Interfaces, Local},
    },
};
use pb_mapper_protocol::direct::PeerInfo;
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::time::Instant;

pub(super) struct Discovery {
    at: Instant,
    server: SocketAddr,
    generation: (u64, u64),
    candidates: Candidates,
    local: Arc<Local>,
}

/// What this end offers for one setup, and what that took.
pub(in crate::direct) struct Discovered {
    pub peer: PeerInfo,
    pub local: Arc<Local>,
    /// Candidates offered, on a LAN interface, and left out past the limit.
    pub counts: (u32, u32, u32),
    pub stun: StunOutcome,
    pub stun_ms: Option<u64>,
}

impl Network {
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
                lan_tcp: None,
            },
            local,
            stun,
            stun_ms,
        }
    }
    pub fn invalidate(&self) {
        // Discovery holds the cache lock across STUN. A generation also
        // invalidates an in-flight result instead of losing the notification.
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
    pub(super) fn mapped_addr(&self) -> Option<SocketAddr> {
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
}
