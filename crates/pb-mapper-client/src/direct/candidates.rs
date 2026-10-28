//! Keep both address families available within the wire's candidate limit.
use pb_mapper_protocol::direct::{MAX_CANDIDATES, valid_candidate};
use std::net::SocketAddr;

pub(super) struct Candidates(Vec<SocketAddr>);

impl Candidates {
    /// Order once per discovery, rather than sorting interfaces per session.
    pub fn new(reflexive: Option<SocketAddr>, hosts: Vec<SocketAddr>) -> Self {
        let mut v6: Vec<_> = hosts
            .iter()
            .copied()
            .filter(SocketAddr::is_ipv6)
            .filter(valid_candidate)
            .collect();
        let mut v4: Vec<_> = hosts
            .iter()
            .copied()
            .filter(SocketAddr::is_ipv4)
            .filter(valid_candidate)
            .collect();
        // Prefer globally routed IPv6 to ULA and loopback. Alternate families
        // so a machine with many virtual adapters cannot crowd out IPv6.
        v6.sort_by_key(|addr| match addr {
            SocketAddr::V6(addr) => (addr.ip().is_loopback(), addr.ip().is_unique_local(), *addr),
            SocketAddr::V4(_) => unreachable!(),
        });
        v6.dedup();
        v4.sort_by_key(|addr| (addr.ip().is_loopback(), *addr));
        v4.dedup();
        let mut v6 = v6.into_iter();
        let mut v4 = v4.into_iter();
        let mut ordered: Vec<_> = reflexive.into_iter().filter(valid_candidate).collect();
        loop {
            match (v6.next(), v4.next()) {
                (None, None) => break,
                (a, b) => {
                    for addr in a.into_iter().chain(b) {
                        if !ordered.contains(&addr) && ordered.len() < MAX_CANDIDATES {
                            ordered.push(addr);
                        }
                    }
                }
            }
        }
        Self(ordered)
    }

    pub fn select(&self, mapped: Option<SocketAddr>) -> Vec<SocketAddr> {
        // Preserve the gateway mapping and STUN result: under nested NAT they
        // describe different routes, and either may be useful to the peer.
        let mut selected = Vec::with_capacity(MAX_CANDIDATES);
        for addr in mapped
            .into_iter()
            .filter(valid_candidate)
            .chain(self.0.iter().copied())
        {
            if selected.len() < MAX_CANDIDATES && !selected.contains(&addr) {
                selected.push(addr);
            }
        }
        selected
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn mappings_and_ipv6_survive_many_interfaces() {
        let reflexive = "198.51.100.1:6000".parse().unwrap();
        let mut hosts: Vec<_> = (1..20)
            .map(|n| SocketAddr::from(([10, 0, 0, n], 4000)))
            .collect();
        let ipv6 = "[2001:db8::1]:4000".parse().unwrap();
        hosts.extend([
            "[fd00::1]:4000".parse().unwrap(),
            "[fe80::1]:4000".parse().unwrap(),
            "0.0.0.0:4000".parse().unwrap(),
            ipv6,
            ipv6,
        ]);
        let candidates = Candidates::new(Some(reflexive), hosts);
        let mapping = "100.64.1.1:5000".parse().unwrap();
        let selected = candidates.select(Some(mapping));
        assert_eq!(selected.len(), MAX_CANDIDATES);
        assert_eq!(selected[0], mapping);
        assert_eq!(selected[1], reflexive);
        assert_eq!(selected[2], ipv6);
        assert!(selected.iter().all(valid_candidate));
        assert_eq!(selected.iter().filter(|addr| **addr == ipv6).count(), 1);
        // A withdrawn lease must disappear even while STUN/hosts are cached.
        assert!(!candidates.select(None).contains(&mapping));
    }
}
