//! The addresses an endpoint offers, best first, within the wire's limit of
//! eight: where its router and STUN see it, then each LAN interface, then the
//! rest with both address families kept.
use crate::diagnostics::InterfaceTier;
use pb_mapper_protocol::direct::{MAX_CANDIDATES, valid_candidate};
use std::net::SocketAddr;

/// An interface address paired with its family's socket port.
#[derive(Clone, Copy, Debug)]
pub(super) struct Host {
    pub addr: SocketAddr,
    pub tier: InterfaceTier,
    /// Which interface it is on, to offer one address per LAN.
    pub interface: usize,
}

pub(super) struct Candidates {
    /// Each with the tier of its interface; none for the reflexive address.
    ordered: Vec<(SocketAddr, Option<InterfaceTier>)>,
    /// Usable addresses left out past the limit.
    truncated: u32,
}

/// What an endpoint offers for one setup.
pub(super) struct Selected {
    pub addrs: Vec<SocketAddr>,
    /// Those on a LAN interface.
    pub lan: u32,
    pub truncated: u32,
}

impl Candidates {
    /// Order once per discovery, rather than sorting interfaces per session.
    ///
    /// A LAN's IPv4 address comes right after the reflexive one, and each of
    /// its IPv6 /64s gets one address: a peer on the same network should
    /// find it on the first probe, ahead of VPN and hypervisor adapters it
    /// can never reach. A /64's other addresses lead to the same place, so
    /// they take no slot.
    pub fn new(reflexive: Option<SocketAddr>, mut hosts: Vec<Host>) -> Self {
        hosts.retain(|host| valid_candidate(&host.addr));
        let mut ordered: Vec<(SocketAddr, Option<InterfaceTier>)> = Vec::new();
        let mut prefixes = Vec::new();
        let mut offer = |ordered: &mut Vec<(SocketAddr, Option<InterfaceTier>)>,
                         addr: SocketAddr,
                         tier: Option<InterfaceTier>| {
            if ordered.iter().any(|(known, _)| *known == addr) {
                return;
            }
            if let SocketAddr::V6(v6) = addr
                && !v6.ip().is_loopback()
            {
                let prefix = v6.ip().to_bits() >> 64;
                if prefixes.contains(&prefix) {
                    return;
                }
                prefixes.push(prefix);
            }
            ordered.push((addr, tier));
        };
        if let Some(reflexive) = reflexive.filter(valid_candidate) {
            offer(&mut ordered, reflexive, None);
        }
        let lan: Vec<_> = hosts
            .iter()
            .filter(|host| host.tier == InterfaceTier::Lan)
            .collect();
        let mut lans = Vec::new();
        for host in lan.iter().filter(|host| host.addr.is_ipv4()) {
            if !lans.contains(&host.interface) {
                lans.push(host.interface);
                offer(&mut ordered, host.addr, Some(host.tier));
            }
        }
        for host in lan.iter().filter(|host| host.addr.is_ipv6()) {
            offer(&mut ordered, host.addr, Some(host.tier));
        }
        // The rest by tier, alternating families so that a machine with many
        // virtual adapters cannot crowd out IPv6. Within a tier, globally
        // routed IPv6 before unique-local and loopback.
        let mut v6: Vec<_> = hosts
            .iter()
            .filter_map(|host| match host.addr {
                SocketAddr::V6(addr) => Some((host, addr)),
                SocketAddr::V4(_) => None,
            })
            .collect();
        let mut v4: Vec<_> = hosts.iter().filter(|host| host.addr.is_ipv4()).collect();
        v6.sort_by_key(|(host, addr)| {
            (
                host.tier,
                addr.ip().is_loopback(),
                addr.ip().is_unique_local(),
                host.addr,
            )
        });
        v4.sort_by_key(|host| (host.tier, host.addr.ip().is_loopback(), host.addr));
        let mut v6 = v6.into_iter().map(|(host, _)| host);
        let mut v4 = v4.into_iter();
        loop {
            match (v6.next(), v4.next()) {
                (None, None) => break,
                (a, b) => {
                    for host in a.into_iter().chain(b) {
                        offer(&mut ordered, host.addr, Some(host.tier));
                    }
                }
            }
        }
        let truncated = ordered.len().saturating_sub(MAX_CANDIDATES) as u32;
        ordered.truncate(MAX_CANDIDATES);
        Self { ordered, truncated }
    }

    pub fn select(&self, mapped: Option<SocketAddr>) -> Selected {
        // Preserve the gateway mapping and STUN result: under nested NAT they
        // describe different routes, and either may be useful to the peer.
        let mut selected = Selected {
            addrs: Vec::with_capacity(MAX_CANDIDATES),
            lan: 0,
            truncated: self.truncated,
        };
        for (addr, tier) in mapped
            .filter(valid_candidate)
            .map(|addr| (addr, None))
            .into_iter()
            .chain(self.ordered.iter().copied())
        {
            if selected.addrs.contains(&addr) {
                continue;
            }
            if selected.addrs.len() == MAX_CANDIDATES {
                selected.truncated += 1;
                continue;
            }
            selected.addrs.push(addr);
            selected.lan += u32::from(tier == Some(InterfaceTier::Lan));
        }
        selected
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// This PC as measured: a WLAN with a stable IPv6 address and a Windows
    /// temporary in one /64, a VPN's TUN and Hyper-V's switch, and then many
    /// more virtual adapters than the wire has room for.
    #[test]
    fn a_lan_comes_first_and_spends_one_slot_per_network() {
        let host = |addr: &str, tier, interface| Host {
            addr: addr.parse().unwrap(),
            tier,
            interface,
        };
        let temporary = "[2408:8256:3100:820:bcec:a6b4:ae1d:f125]:4600";
        let mut hosts = vec![
            host("10.0.0.33:4400", InterfaceTier::Virtual, 1),
            host("172.26.48.1:4400", InterfaceTier::Virtual, 2),
            host(temporary, InterfaceTier::Lan, 0),
            host(
                "[2408:8256:3100:820:2638:563e:2d59:169d]:4600",
                InterfaceTier::Lan,
                0,
            ),
            host("[fe80::1]:4600", InterfaceTier::Lan, 0),
            host("192.168.21.5:4400", InterfaceTier::Lan, 0),
        ];
        hosts.extend((1..20).map(|n| Host {
            addr: SocketAddr::from(([10, 9, 0, n], 4400)),
            tier: InterfaceTier::Virtual,
            interface: 2 + usize::from(n),
        }));
        let reflexive = "203.0.113.9:6000".parse().unwrap();
        let candidates = Candidates::new(Some(reflexive), hosts);
        let mapping = "203.0.113.9:5000".parse().unwrap();
        let selected = candidates.select(Some(mapping));
        let lan_v4: SocketAddr = "192.168.21.5:4400".parse().unwrap();
        assert_eq!(
            selected.addrs[..4],
            [mapping, reflexive, lan_v4, temporary.parse().unwrap()]
        );
        assert_eq!(selected.addrs.len(), MAX_CANDIDATES);
        assert!(selected.addrs.iter().all(valid_candidate));
        let in_the_lan_prefix = selected
            .addrs
            .iter()
            .filter(|addr| addr.to_string().starts_with("[2408:8256:3100:820:"))
            .count();
        assert_eq!(in_the_lan_prefix, 1);
        assert_eq!(selected.lan, 2);
        assert!(selected.truncated > 0);
        // A withdrawn lease must disappear even while STUN/hosts are cached.
        assert!(!candidates.select(None).addrs.contains(&mapping));
    }
}
