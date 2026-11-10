//! How a direct session travels, as [`PathDiagnostics::link`] reports it:
//! which network it crosses, by which interface, how it was found, how long
//! each stage of its setup took, and its carrier's live view of the path.
//!
//! [`PathDiagnostics::link`]: super::PathDiagnostics::link

#[cfg(feature = "p2p")]
use super::LinkSample;
pub use pb_mapper_protocol::direct::{AddressFamily, DirectTransport, LinkKind};
use serde::Serialize;
use std::net::{IpAddr, SocketAddr};
#[cfg(feature = "p2p")]
use std::time::Duration;

/// What a local interface is. Candidates are offered, and probed, in this
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceTier {
    /// Ethernet or Wi-Fi with a gateway: a network other devices share.
    Lan,
    /// Physical, without a gateway: a cable to one other machine, or a
    /// network with no way out.
    Physical,
    /// Mobile data.
    Cellular,
    /// A VPN's tunnel, a hypervisor's switch or a container's bridge,
    /// however private its addresses look.
    Virtual,
    Loopback,
}

impl InterfaceTier {
    /// Stable label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lan => "lan",
            Self::Physical => "physical",
            Self::Cellular => "cellular",
            Self::Virtual => "virtual",
            Self::Loopback => "loopback",
        }
    }
}

/// Why this end believed, before any packet said so, that the peer shares
/// one of its local networks. Strong evidence gives the LAN a head start;
/// weak evidence only probes it first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LanEvidence {
    /// A global IPv6 address of the peer is in the /64 of one of ours.
    /// Strong: global prefixes are unique.
    Ipv6Prefix,
    /// The peer offers this end's public IP, so sits behind the same NAT,
    /// and an address on one of its subnets. Strong.
    PublicIpSubnet,
    /// Only a subnet is shared, as countless homes share 192.168.1.0/24.
    /// Weak.
    Subnet,
}

impl LanEvidence {
    /// Whether this is enough to hold other paths back for the LAN.
    pub fn strong(self) -> bool {
        match self {
            Self::Ipv6Prefix | Self::PublicIpSubnet => true,
            Self::Subnet => false,
        }
    }

    /// Stable label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ipv6Prefix => "ipv6_prefix",
            Self::PublicIpSubnet => "public_ip_subnet",
            Self::Subnet => "subnet",
        }
    }
}

/// How this end came to know the peer address its path runs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkVia {
    /// An interface address the peer offered.
    Host,
    /// A public IPv4 address the peer offered: what a STUN server saw of
    /// it, or one of its interfaces.
    Public,
    /// The peer's router mapping: offered first, ahead of another public
    /// IPv4 address.
    Mapped,
    /// Not offered: the peer's packets came from it, and it answered a
    /// challenge sent back to it.
    PeerReflexive,
}

impl LinkVia {
    /// Stable label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Public => "public",
            Self::Mapped => "mapped",
            Self::PeerReflexive => "peer_reflexive",
        }
    }
}

/// How this end's STUN lookup went for the setup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StunOutcome {
    /// It answered.
    Ok,
    /// It did not answer in time (700 ms).
    Timeout,
    /// It failed, or this end has no socket of the relay's address family.
    #[default]
    Unavailable,
    /// A lookup made in the last 10 s was used.
    Cached,
}

impl StunOutcome {
    /// Stable label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Cached => "cached",
        }
    }
}

/// How long each stage of a direct setup took on this end, in milliseconds.
/// A stage this end does not run, such as dialing on the publishing end, is
/// `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct SetupTimings {
    /// Asking the relay whether direct is offered and opening its control
    /// link.
    pub negotiate_ms: Option<u64>,
    /// Gathering this end's candidates, STUN included.
    pub discover_ms: Option<u64>,
    pub stun_ms: Option<u64>,
    pub stun: StunOutcome,
    /// From asking for a session to the grant.
    pub grant_ms: Option<u64>,
    /// From the grant to the first peer address that answered a probe, and
    /// to the first on a local network.
    pub first_pong_ms: Option<u64>,
    pub lan_first_pong_ms: Option<u64>,
    /// The winning handshake: QUIC's, or TCP's and TLS's.
    pub handshake_ms: Option<u64>,
    /// From the handshake to the publisher's local service being open.
    pub commit_ms: Option<u64>,
    /// The whole setup.
    pub setup_ms: u64,
}

/// The addresses a direct setup worked with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CandidateCounts {
    /// This end's candidates as offered, those on a LAN interface, and
    /// those left out past the limit of 8.
    pub local: u32,
    pub local_lan: u32,
    pub truncated: u32,
    /// The peer's candidates, and those on one of this end's networks.
    pub peer: u32,
    pub peer_lan: u32,
    /// Peer addresses probed and those that answered; handshakes this end
    /// started, TCP's among them (none on the publishing end, which is
    /// dialed).
    pub probed: u32,
    pub answered: u32,
    pub handshakes: u32,
}

/// QUIC's view of the path, read when diagnostics or metrics are. A TCP
/// path has only its handshake's round trip, as both round trips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct LinkStats {
    pub rtt_us: u64,
    /// The lowest round trip seen: the path without queues.
    pub min_rtt_us: u64,
    pub congestion_window: u64,
    pub mtu: u16,
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub congestion_events: u64,
    /// UDP bytes on the wire each way and those lost: QUIC's headers,
    /// acknowledgements and probes included, so more than the application
    /// bytes in [`PathTraffic`](super::PathTraffic).
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub lost_bytes: u64,
    /// The congestion controller's estimate of what the path sustains, in
    /// bits per second, when it makes one.
    pub bandwidth_bps: Option<u64>,
}

/// A direct session's path, kept while the session is open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct DirectLink {
    pub kind: LinkKind,
    pub family: AddressFamily,
    pub via: LinkVia,
    /// QUIC, or TCP within a local network. Over TCP, [`Self::stats`] has
    /// the handshake's round trip and nothing else.
    pub transport: DirectTransport,
    pub peer: SocketAddr,
    /// The local address the path leaves from, when the socket reports it.
    pub local_ip: Option<IpAddr>,
    /// The interface that address belongs to, and what it is.
    pub interface: Option<String>,
    pub tier: Option<InterfaceTier>,
    /// What suggested a shared network before the setup, if anything.
    pub same_lan: Option<LanEvidence>,
    pub timings: SetupTimings,
    pub candidates: CandidateCounts,
    pub stats: LinkStats,
}

/// Where a direct session's live figures come from.
#[cfg(feature = "p2p")]
pub(crate) enum LinkProbe {
    /// QUIC's own, read from the connection as they are now.
    Quic(quinn::Connection),
    /// TCP keeps its figures in the kernel; the round trip of the
    /// connection's handshake stands in for them.
    Tcp { handshake_rtt: Duration },
}

/// A direct session's path and what its figures are read from.
#[cfg(feature = "p2p")]
pub(super) struct LiveLink {
    pub(super) id: u64,
    pub(super) link: DirectLink,
    pub(super) probe: LinkProbe,
}

#[cfg(feature = "p2p")]
impl LinkStats {
    /// QUIC's view of `connection`'s path, as it is now.
    pub(crate) fn of_quic(connection: &quinn::Connection) -> Self {
        let stats = connection.stats();
        Self {
            rtt_us: micros(connection.rtt()),
            min_rtt_us: micros(stats.path.min_rtt),
            congestion_window: stats.path.cwnd,
            mtu: stats.path.current_mtu,
            sent_packets: stats.path.sent_packets,
            lost_packets: stats.path.lost_packets,
            congestion_events: stats.path.congestion_events,
            sent_bytes: stats.udp_tx.bytes,
            received_bytes: stats.udp_rx.bytes,
            lost_bytes: stats.path.lost_bytes,
            bandwidth_bps: stats.path.bandwidth_estimate,
        }
    }
}

#[cfg(feature = "p2p")]
impl LiveLink {
    /// The carrier's figures as they are now.
    fn stats(&self) -> LinkStats {
        match &self.probe {
            LinkProbe::Quic(connection) => LinkStats::of_quic(connection),
            LinkProbe::Tcp { handshake_rtt } => LinkStats {
                rtt_us: micros(*handshake_rtt),
                min_rtt_us: micros(*handshake_rtt),
                ..LinkStats::default()
            },
        }
    }

    /// The link with its carrier's figures as they are now.
    pub(super) fn sample(&self) -> DirectLink {
        DirectLink {
            stats: self.stats(),
            ..self.link.clone()
        }
    }

    /// What [`TunnelMetrics::link`] reports of it, figures as they are now.
    ///
    /// [`TunnelMetrics::link`]: super::TunnelMetrics::link
    pub(super) fn metrics(&self) -> LinkSample {
        let link = &self.link;
        LinkSample {
            kind: link.kind,
            family: link.family,
            via: link.via,
            transport: link.transport,
            peer: link.peer,
            local_ip: link.local_ip,
            same_lan: link.same_lan,
            setup_ms: link.timings.setup_ms,
            stats: self.stats(),
        }
    }
}

#[cfg(feature = "p2p")]
fn micros(time: Duration) -> u64 {
    time.as_micros().min(u128::from(u64::MAX)) as u64
}
