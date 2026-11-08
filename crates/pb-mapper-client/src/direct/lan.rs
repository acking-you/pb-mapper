//! Which local interfaces lead to a LAN, and whether a peer is on one.
//!
//! Candidates and probes go LAN first, and that is decided by what an
//! interface is, not by how its addresses look: Ethernet or Wi-Fi with a
//! gateway leads to a LAN; a VPN's TUN, a hypervisor's switch or a container
//! bridge does not, however private its addresses. Addresses come from
//! if-addrs, as they always have; what each interface is comes from netdev,
//! which knows types and gateways but is slow to ask, so it is read in the
//! background. Until it answers, and where it cannot tell, as on Android,
//! names and address ranges decide.
//!
//! Everything else is a pure function of an [`Interfaces`] table, which tests
//! inject. Getting it wrong costs an order or a 150 ms head start; what is
//! authenticated, and how, never depends on it.

use crate::diagnostics::{InterfaceTier, LanEvidence, LinkKind};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};

/// One up interface and its usable addresses.
#[derive(Clone, Debug)]
pub(crate) struct Interface {
    pub name: String,
    pub tier: InterfaceTier,
    /// Each address with the prefix it shares with its link, preferred
    /// first.
    pub addrs: Vec<(IpAddr, u8)>,
}

impl Interface {
    pub fn new(
        name: impl Into<String>,
        tier: InterfaceTier,
        addrs: impl IntoIterator<Item = (IpAddr, u8)>,
    ) -> Self {
        Self {
            name: name.into(),
            tier,
            addrs: addrs
                .into_iter()
                .map(|(ip, len)| (ip, on_link_prefix(ip, len)))
                .collect(),
        }
    }

    /// Whether `ip` is on this interface's link.
    fn holds(&self, ip: IpAddr) -> bool {
        self.addrs
            .iter()
            .any(|&(own, len)| own == ip || same_prefix(own, ip, len))
    }

    /// Whether the peers on its link share a network with this machine.
    fn local_network(&self) -> bool {
        matches!(self.tier, InterfaceTier::Lan | InterfaceTier::Physical)
    }
}

/// This machine's interfaces, as one discovery saw them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Interfaces(Vec<Interface>);

impl Interfaces {
    pub fn new(interfaces: Vec<Interface>) -> Self {
        Self(interfaces)
    }

    /// The system's interfaces now, on network `generation`: addresses from
    /// if-addrs, which answers in milliseconds, each interface ranked as
    /// netdev last said, or by name and range until it has.
    pub fn system(generation: u64) -> Self {
        struct Found {
            index: Option<u32>,
            point_to_point: bool,
            name: String,
            addrs: Vec<(IpAddr, u8)>,
        }
        let kinds = Kinds::current(generation);
        let mut found: Vec<Found> = Vec::new();
        for interface in if_addrs::get_if_addrs().unwrap_or_default() {
            let (ip, len) = match &interface.addr {
                if_addrs::IfAddr::V4(v4) => (IpAddr::V4(v4.ip), v4.netmask.to_bits().count_ones()),
                if_addrs::IfAddr::V6(v6) => (IpAddr::V6(v6.ip), v6.netmask.to_bits().count_ones()),
            };
            // By index, or by name where there is none.
            let same = |known: &&mut Found| match known.index {
                Some(index) => Some(index) == interface.index,
                None => known.name == interface.name,
            };
            match found.iter_mut().find(same) {
                Some(known) => known.addrs.push((ip, len as u8)),
                None => found.push(Found {
                    index: interface.index,
                    point_to_point: interface.is_p2p,
                    name: interface.name.clone(),
                    addrs: vec![(ip, len as u8)],
                }),
            }
        }
        let mut table: Vec<_> = found
            .into_iter()
            .map(|mut found| {
                let known = kinds
                    .as_ref()
                    .and_then(|kinds| kinds.of(found.index, &found.name));
                let tier = known.unwrap_or_else(|| {
                    let kind = if found.point_to_point {
                        Kind::Virtual
                    } else {
                        Kind::Unknown
                    };
                    tier(kind, &[&found.name], None, true, &found.addrs)
                });
                // Windows sends from its temporary IPv6 address, so that one
                // is offered for its /64.
                if let Some(kinds) = &kinds {
                    found
                        .addrs
                        .sort_by_key(|(ip, _)| !kinds.temporary.contains(ip));
                }
                Interface::new(found.name, tier, found.addrs)
            })
            .collect();
        // The best first, so a local address held by two links is put on the
        // better one.
        table.sort_by_key(|interface| interface.tier);
        Self::new(table)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Interface> {
        self.0.iter()
    }

    /// Whether `ip` is one of this machine's own addresses.
    pub fn owns(&self, ip: IpAddr) -> bool {
        self.0
            .iter()
            .any(|interface| interface.addrs.iter().any(|&(own, _)| own == ip))
    }

    /// The interface a local address belongs to: its owner, or else the one
    /// whose link holds it.
    pub fn local(&self, ip: IpAddr) -> Option<&Interface> {
        let ip = ip.to_canonical();
        self.0
            .iter()
            .find(|interface| interface.addrs.iter().any(|&(own, _)| own == ip))
            .or_else(|| self.0.iter().find(|interface| interface.holds(ip)))
    }

    /// The local network a peer address is on: a LAN or physical interface
    /// whose link holds it. Never for an address of this machine's own, which
    /// on another machine says only that both use one private range.
    pub fn network_of(&self, peer: IpAddr) -> Option<&Interface> {
        let peer = peer.to_canonical();
        if self.owns(peer) {
            return None;
        }
        self.0
            .iter()
            .filter(|interface| interface.local_network())
            .find(|interface| interface.holds(peer))
    }
}

/// What this end knows of its own networks when a setup starts.
#[derive(Clone, Debug, Default)]
pub(crate) struct Local {
    pub interfaces: Interfaces,
    /// Where STUN and the router mapping say this end is seen from.
    pub public: Vec<IpAddr>,
}

impl Local {
    /// Whether the peer has shown, by what it offers, that it shares one of
    /// this end's local networks.
    pub fn assess(&self, peer: &[SocketAddr]) -> Option<LanEvidence> {
        let mut subnet = false;
        let mut public = false;
        for addr in peer {
            let ip = addr.ip().to_canonical();
            public |= self.public.contains(&ip) && !self.interfaces.owns(ip);
            if self.interfaces.network_of(ip).is_some() {
                if ip.is_ipv6() && is_global(ip) {
                    return Some(LanEvidence::Ipv6Prefix);
                }
                subnet = true;
            }
        }
        match (subnet, public) {
            (true, true) => Some(LanEvidence::PublicIpSubnet),
            (true, false) => Some(LanEvidence::Subnet),
            (false, _) => None,
        }
    }
}

/// A setup's view of the peer, given this end's networks.
#[derive(Clone, Debug, Default)]
pub(crate) struct Assessment {
    pub local: Arc<Local>,
    pub evidence: Option<LanEvidence>,
}

impl Assessment {
    pub fn new(local: Arc<Local>, peer: &[SocketAddr]) -> Self {
        let evidence = local.assess(peer);
        Self { local, evidence }
    }

    /// Whether a peer address is on one of this end's local networks.
    pub fn is_lan(&self, peer: SocketAddr) -> bool {
        self.local.interfaces.network_of(peer.ip()).is_some()
    }

    /// The peer's candidates, those on a local network first, each order
    /// kept.
    pub fn order(&self, peer: &[SocketAddr]) -> Vec<SocketAddr> {
        let (mut lan, rest): (Vec<_>, Vec<_>) =
            peer.iter().copied().partition(|addr| self.is_lan(*addr));
        lan.extend(rest);
        lan
    }

    /// What a path between `local` and `peer` crosses: a local network when
    /// the peer is on the link of the LAN or physical interface the path
    /// leaves by, else the internet or a private network by the peer's
    /// address.
    pub fn kind(&self, local: Option<IpAddr>, peer: IpAddr) -> LinkKind {
        let peer = peer.to_canonical();
        let interfaces = &self.local.interfaces;
        let on_lan = match local.and_then(|ip| interfaces.local(ip)) {
            Some(interface) => interface.local_network() && interface.holds(peer),
            None => interfaces.network_of(peer).is_some(),
        };
        if on_lan {
            LinkKind::Lan
        } else if is_global(peer) {
            LinkKind::Internet
        } else {
            LinkKind::Private
        }
    }
}

/// Whether `ip` is routed on the internet.
pub(crate) fn is_global(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && b & 0xc0 == 64)
                || (a == 198 && b & 0xfe == 18))
        }
        IpAddr::V6(v6) => v6.segments()[0] & 0xe000 == 0x2000,
    }
}

/// The prefix an address shares with its link. Windows lists temporary IPv6
/// addresses as /128, but SLAAC puts global and unique-local addresses on a
/// /64.
fn on_link_prefix(ip: IpAddr, len: u8) -> u8 {
    match ip {
        IpAddr::V4(_) => len.min(32),
        IpAddr::V6(v6) if len > 64 && (is_global(ip) || v6.is_unique_local()) => 64,
        IpAddr::V6(_) => len.min(128),
    }
}

fn same_prefix(a: IpAddr, b: IpAddr, len: u8) -> bool {
    // A zero-length prefix is a missing mask, not a link to everything.
    if len == 0 {
        return false;
    }
    match (a, b) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let mask = u32::MAX << (32 - u32::from(len.min(32)));
            a.to_bits() & mask == b.to_bits() & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let mask = u128::MAX << (128 - u32::from(len.min(128)));
            a.to_bits() & mask == b.to_bits() & mask
        }
        _ => false,
    }
}

/// What the system says an interface is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Loopback,
    Ethernet,
    Wireless,
    Cellular,
    Virtual,
    Unknown,
}

/// Rank an interface. `labels` are its names and description, `gateway` is
/// `None` where the system does not say, and `physical` is the system's
/// judgment of the hardware behind it.
fn tier(
    kind: Kind,
    labels: &[&str],
    gateway: Option<bool>,
    physical: bool,
    addrs: &[(IpAddr, u8)],
) -> InterfaceTier {
    let labels: Vec<String> = labels.iter().map(|label| label.to_lowercase()).collect();
    let named = |prefixes: &[&str], words: &[&str]| {
        labels.iter().any(|label| {
            prefixes.iter().any(|prefix| label.starts_with(prefix))
                || words.iter().any(|word| label.contains(word))
        })
    };
    if kind == Kind::Loopback || addrs.iter().all(|(ip, _)| ip.is_loopback()) {
        return InterfaceTier::Loopback;
    }
    if kind == Kind::Cellular
        || named(
            &["rmnet", "ccmni", "pdp", "wwan", "clat", "v4-", "rev_rmnet"],
            &["mobile broadband", "cellular"],
        )
    {
        return InterfaceTier::Cellular;
    }
    // Ranges VPNs number their tunnels from: Tailscale's CGNAT, and the
    // benchmarking block fake-IP proxies use.
    let vpn_range = || {
        addrs.iter().any(|(ip, _)| match ip {
            IpAddr::V4(v4) => {
                let [a, b, ..] = v4.octets();
                (a == 100 && b & 0xc0 == 64) || (a == 198 && b & 0xfe == 18)
            }
            IpAddr::V6(_) => false,
        })
    };
    let typed_physical = matches!(kind, Kind::Ethernet | Kind::Wireless);
    if kind == Kind::Virtual
        || named(
            &[
                "tun", "tap", "wg", "zt", "br-", "veth", "vmnet", "virbr", "vboxnet", "docker",
                "awdl", "llw", "anpi", "ipsec", "ppp", "gif", "stf", "bridge",
            ],
            &[
                "wintun",
                "utun",
                "wireguard",
                "tailscale",
                "zerotier",
                "vethernet",
                "hyper-v",
                "vmware",
                "virtualbox",
                "tap-windows",
                "openvpn",
            ],
        )
        || (typed_physical && !physical && gateway != Some(true))
        || (kind == Kind::Unknown && vpn_range())
    {
        return InterfaceTier::Virtual;
    }
    let looks_physical =
        typed_physical || named(&["wlan", "swlan", "eth", "en", "wl", "ap"], &["wi-fi"]);
    let site_local = addrs.iter().any(|&(ip, _)| match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => is_global(ip) || v6.is_unique_local(),
    });
    match gateway {
        Some(true) => InterfaceTier::Lan,
        // Android hides gateways from apps: a Wi-Fi or Ethernet interface
        // numbered from a private range is taken to lead to one.
        None if looks_physical && site_local => InterfaceTier::Lan,
        _ => InterfaceTier::Physical,
    }
}

/// What netdev last said each interface is. It is read on a thread of its
/// own, never on a setup's path: for each gateway netdev asks Windows for
/// its MAC address and may wait on ARP, which took 1.2 to 3.2 s on a PC whose
/// VPN gateway does not answer. A reading ranks the interfaces it knows
/// until one taken on the current network replaces it.
struct Kinds {
    generation: u64,
    known: Vec<Known>,
    /// Temporary IPv6 addresses, which Windows sends from.
    temporary: Vec<IpAddr>,
}

struct Known {
    index: u32,
    names: Vec<String>,
    tier: InterfaceTier,
}

/// The latest reading, and whether a new one is being taken.
static KINDS: Mutex<(Option<Arc<Kinds>>, bool)> = Mutex::new((None, false));

impl Kinds {
    /// The latest reading. Starts a new one, unless one is under way, when
    /// there is none or it was taken on another network.
    fn current(generation: u64) -> Option<Arc<Self>> {
        let mut state = KINDS.lock().unwrap_or_else(|e| e.into_inner());
        let stale = state
            .0
            .as_ref()
            .is_none_or(|kinds| kinds.generation != generation);
        if stale && !state.1 {
            state.1 = true;
            let reading = std::thread::Builder::new()
                .name("pb-mapper-interfaces".into())
                .spawn(move || {
                    // Cleared however the reading ends, so a later discovery
                    // can start another.
                    struct Reading(Option<Kinds>);
                    impl Drop for Reading {
                        fn drop(&mut self) {
                            let mut state = KINDS.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(kinds) = self.0.take() {
                                state.0 = Some(Arc::new(kinds));
                            }
                            state.1 = false;
                        }
                    }
                    let mut reading = Reading(None);
                    reading.0 = Some(Self::read(generation));
                });
            if reading.is_err() {
                state.1 = false;
            }
        }
        state.0.clone()
    }

    /// The tier netdev gave the interface with this index, or this name.
    fn of(&self, index: Option<u32>, name: &String) -> Option<InterfaceTier> {
        self.known
            .iter()
            .find(|known| match index {
                Some(index) => known.index == index,
                None => known.names.contains(name),
            })
            .map(|known| known.tier)
    }

    fn read(generation: u64) -> Self {
        use netdev::interface::types::InterfaceType as T;
        let mut kinds = Self {
            generation,
            known: Vec::new(),
            temporary: Vec::new(),
        };
        for interface in netdev::get_interfaces() {
            if !interface.is_up() {
                continue;
            }
            let addrs: Vec<(IpAddr, u8)> = interface
                .ipv4
                .iter()
                .map(|net| (IpAddr::V4(net.addr()), net.prefix_len()))
                .chain(
                    interface
                        .ipv6
                        .iter()
                        .map(|net| (IpAddr::V6(net.addr()), net.prefix_len())),
                )
                .collect();
            if addrs.is_empty() {
                continue;
            }
            kinds.temporary.extend(
                interface
                    .ipv6
                    .iter()
                    .zip(&interface.ipv6_addr_flags)
                    .filter(|(_, flags)| flags.temporary)
                    .map(|(net, _)| IpAddr::V6(net.addr())),
            );
            let kind = match interface.if_type {
                T::Loopback => Kind::Loopback,
                T::Ethernet
                | T::Ethernet3Megabit
                | T::FastEthernetT
                | T::FastEthernetFx
                | T::GigabitEthernet => Kind::Ethernet,
                T::Wireless80211 => Kind::Wireless,
                T::Wwan | T::Wwanpp | T::Wwanpp2 | T::Wman | T::GenericModem => Kind::Cellular,
                T::Tunnel | T::ProprietaryVirtual | T::Ppp | T::Slip | T::PeerToPeerWireless => {
                    Kind::Virtual
                }
                _ => Kind::Unknown,
            };
            let gateway = match &interface.gateway {
                Some(gateway) if !gateway.ipv4.is_empty() || !gateway.ipv6.is_empty() => Some(true),
                // Android hides routes from apps: no gateway says nothing.
                _ if cfg!(target_os = "android") => None,
                _ => Some(false),
            };
            let labels: Vec<&str> = [
                Some(interface.name.as_str()),
                interface.friendly_name.as_deref(),
                interface.description.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect();
            let tier = tier(kind, &labels, gateway, interface.is_physical(), &addrs);
            kinds.known.push(Known {
                index: interface.index,
                names: [Some(interface.name), interface.friendly_name]
                    .into_iter()
                    .flatten()
                    .collect(),
                tier,
            });
        }
        kinds
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn at(text: &str) -> SocketAddr {
        SocketAddr::new(ip(text), 40000)
    }

    /// This PC as measured: WLAN with a gateway, a stable IPv6 /64 and a
    /// Windows temporary listed as /128; a VPN's TUN and Hyper-V's switch.
    fn this_pc() -> Local {
        Local {
            interfaces: Interfaces::new(vec![
                Interface::new(
                    "WLAN",
                    InterfaceTier::Lan,
                    [
                        (ip("192.168.21.5"), 24),
                        (ip("2408:8256:3100:820:bcec:a6b4:ae1d:f125"), 128),
                        (ip("2408:8256:3100:820:2638:563e:2d59:169d"), 64),
                    ],
                ),
                Interface::new("Clash", InterfaceTier::Virtual, [(ip("10.0.0.33"), 24)]),
                Interface::new(
                    "vEthernet (Default Switch)",
                    InterfaceTier::Virtual,
                    [(ip("172.26.48.1"), 20)],
                ),
            ]),
            public: vec![ip("203.0.113.9")],
        }
    }

    #[test]
    fn evidence_is_strong_only_where_a_network_cannot_be_mistaken() {
        let pc = this_pc();
        // A phone on the same Wi-Fi: its address in the /64 that only the
        // temporary, listed as /128, would otherwise have hidden.
        assert_eq!(
            pc.assess(&[at("2408:8256:3100:820::1234")]),
            Some(LanEvidence::Ipv6Prefix)
        );
        // Behind the same NAT, and on the WLAN's subnet.
        assert_eq!(
            pc.assess(&[at("203.0.113.9"), at("192.168.21.100")]),
            Some(LanEvidence::PublicIpSubnet)
        );
        // Another home numbered from the same private range.
        assert_eq!(
            pc.assess(&[at("198.51.100.7"), at("192.168.21.100")]),
            Some(LanEvidence::Subnet)
        );
        // Not evidence: this machine's own address seen on another, a peer
        // on the VPN's or the hypervisor's subnet, or the public IP alone.
        assert_eq!(
            pc.assess(&[at("192.168.21.5"), at("10.0.0.40"), at("172.26.48.9")]),
            None
        );
        assert_eq!(pc.assess(&[at("203.0.113.9")]), None);
    }
}
