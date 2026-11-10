//! Opt-in coordination messages. Legacy connections never emit these messages.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

pub type SessionId = [u8; 16];
pub fn random_session_id() -> SessionId {
    rand::random()
}

pub const DIRECT_VERSION: u16 = 1;
pub const MAX_CANDIDATES: usize = 8;
/// The most TCP addresses a provider offers: its LAN addresses only.
pub const MAX_LAN_TCP: usize = 4;
pub const MAX_CERT_LEN: usize = 2048;
pub const SETUP_TTL_MS: u64 = 8_000;

/// A direct-control connection that reads nothing for this long is dropped,
/// at either end.
pub const CONTROL_IDLE: Duration = Duration::from_secs(45);
/// How often a consumer pings the relay over its direct-control connection;
/// the relay answers each ping.
pub const CONTROL_PING: Duration = Duration::from_secs(15);
/// The longest writing one direct-control message may take, at either end.
pub const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const _: () = assert!(
    CONTROL_PING.as_secs() * 3 <= CONTROL_IDLE.as_secs(),
    "a live consumer pings three times within the idle window of either end"
);

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct SessionSecret(pub [u8; 32]);

impl PartialEq for SessionSecret {
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;
        bool::from(self.0.ct_eq(&other.0))
    }
}
impl Eq for SessionSecret {}
impl SessionSecret {
    pub fn random() -> Self {
        Self(rand::random())
    }
}

impl std::fmt::Debug for SessionSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectCapability {
    pub version: u16,
    /// An unspecified IP means the relay does not know its public address and
    /// the client should use the one it reached the relay at; see [`stun_target`].
    pub stun_addr: SocketAddr,
}

/// The STUN address a client should query, given what the relay advertised and
/// the relay IP this client's control connection actually reached.
///
/// A relay behind 1:1 NAT or port forwarding cannot see its own public IP, so
/// unless the operator names one it advertises only the port. The client
/// already dialed that public address, which is the one its UDP probes must use.
pub fn stun_target(advertised: SocketAddr, relay: IpAddr) -> SocketAddr {
    if advertised.ip().is_unspecified() {
        SocketAddr::new(relay.to_canonical(), advertised.port())
    } else {
        advertised
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerInfo {
    pub certificate: Vec<u8>,
    pub candidates: Vec<SocketAddr>,
    /// TCP within a local network. From a consumer, empty: it asks for it.
    /// From a provider: its TCP listener's addresses on its LAN interfaces,
    /// never a public one. Absent whenever neither, and from endpoints and
    /// relays that predate it, which then carry the session over QUIC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_tcp: Option<Vec<SocketAddr>>,
}

impl PeerInfo {
    pub fn valid(&self) -> bool {
        !self.certificate.is_empty()
            && self.certificate.len() <= MAX_CERT_LEN
            && self.candidates.len() <= MAX_CANDIDATES
            && self.candidates.iter().all(valid_candidate)
            && self
                .lan_tcp
                .as_ref()
                .is_none_or(|addrs| addrs.len() <= MAX_LAN_TCP && addrs.iter().all(valid_candidate))
    }
}

pub fn valid_candidate(addr: &SocketAddr) -> bool {
    addr.port() != 0
        && !addr.ip().is_unspecified()
        && !addr.ip().is_multicast()
        && match addr {
            SocketAddr::V4(addr) => !addr.ip().is_broadcast() && !addr.ip().is_link_local(),
            SocketAddr::V6(addr) => !addr.ip().is_unicast_link_local(),
        }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionGrant {
    pub id: SessionId,
    pub server_instance: SessionId,
    pub generation: u64,
    pub namespace: u64,
    pub service: String,
    pub is_datagram: bool,
    pub consumer_key: u64,
    pub provider_key: u64,
    pub consumer: PeerInfo,
    pub provider: PeerInfo,
    pub secret: SessionSecret,
    pub ttl_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DirectClientMessage {
    CancelOpen {
        request: u64,
    },
    Open {
        request: u64,
        key: String,
        is_datagram: bool,
        peer: PeerInfo,
    },
    RelaySelect {
        id: SessionId,
    },
    Cancel {
        id: SessionId,
    },
    Report(SessionUpdate),
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DirectProviderMessage {
    Accept { id: SessionId, peer: PeerInfo },
    Reject { id: SessionId, reason: String },
    RelaySelected { id: SessionId },
    Report(SessionUpdate),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DirectEvent {
    Ready(DirectCapability),
    Offer {
        grant: SessionGrant,
        stun_addr: SocketAddr,
    },
    Granted {
        request: u64,
        grant: SessionGrant,
    },
    LegacyRelay {
        request: u64,
    },
    Rejected {
        request: u64,
        reason: String,
    },
    RelaySelect {
        id: SessionId,
    },
    RelaySelected {
        id: SessionId,
    },
    Cancel {
        id: SessionId,
    },
    Pong,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionPath {
    Direct,
    Relay,
}

impl SessionPath {
    /// Stable label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Active,
    Closed,
}

impl SessionState {
    /// Stable label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Closed => "closed",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionReport {
    pub id: SessionId,
    pub seq: u64,
    pub path: SessionPath,
    pub state: SessionState,
    pub reason: Option<String>,
    /// What a direct session's path is, as the reporting end saw it. Absent
    /// from relayed sessions and from endpoints that predate it; a relay
    /// that predates it drops it, as it re-serializes only what it parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkSummary>,
}

/// A direct path in a few words, with no address: reports must not carry
/// candidates.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkSummary {
    pub kind: LinkKind,
    pub family: AddressFamily,
    /// Absent for QUIC, as from every endpoint that predates TCP.
    #[serde(default, skip_serializing_if = "DirectTransport::is_quic")]
    pub transport: DirectTransport,
}

/// What a direct path runs over: QUIC, or TCP within a local network.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DirectTransport {
    #[default]
    Quic,
    Tcp,
    /// A carrier added by a later version.
    #[serde(other)]
    Unknown,
}

impl DirectTransport {
    /// Stable label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quic => "quic",
            Self::Tcp => "tcp",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_quic(&self) -> bool {
        *self == Self::Quic
    }
}

/// Where a direct path runs.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// Within one local network: the peer is on the subnet, or the IPv6 /64,
    /// of a physical interface of this end.
    Lan,
    /// Across the internet, between public addresses.
    Internet,
    /// Between private addresses that are not on one local network: a VPN,
    /// a routed private network, a carrier's NAT, or this machine.
    Private,
    /// A kind added by a later version.
    #[serde(other)]
    Unknown,
}

impl LinkKind {
    /// Stable label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lan => "lan",
            Self::Internet => "internet",
            Self::Private => "private",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AddressFamily {
    Ipv4,
    Ipv6,
    /// A family added by a later version.
    #[serde(other)]
    Unknown,
}

impl AddressFamily {
    pub fn of(addr: IpAddr) -> Self {
        if addr.to_canonical().is_ipv4() {
            Self::Ipv4
        } else {
            Self::Ipv6
        }
    }

    /// Stable label, matching the serialized representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ipv4 => "ipv4",
            Self::Ipv6 => "ipv6",
            Self::Unknown => "unknown",
        }
    }
}

/// Metadata claimed by an authenticated endpoint. It never authorizes setup.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub server_instance: SessionId,
    pub id: SessionId,
    pub namespace: u64,
    pub service: String,
    pub is_datagram: bool,
    pub consumer_key: u64,
    pub provider_key: u64,
}
impl From<&SessionGrant> for SessionSummary {
    fn from(grant: &SessionGrant) -> Self {
        Self {
            server_instance: grant.server_instance,
            id: grant.id,
            namespace: grant.namespace,
            service: grant.service.clone(),
            is_datagram: grant.is_datagram,
            consumer_key: grant.consumer_key,
            provider_key: grant.provider_key,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionUpdate {
    pub session: SessionSummary,
    pub secret: SessionSecret,
    pub report: SessionReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservedReport {
    pub report: SessionReport,
    pub age_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionObservation {
    pub server_instance: SessionId,
    /// Restored endpoint claim after restart or retention expiry, without setup authority.
    pub restored: bool,
    pub id: SessionId,
    pub service: String,
    pub is_datagram: bool,
    pub consumer: Option<ObservedReport>,
    pub provider: Option<ObservedReport>,
    pub stale: bool,
    pub conflicting: bool,
}

macro_rules! wire_message {
    ($($name:ty),+ $(,)?) => {$ (
        impl crate::command::MessageSerializer for $name {
            fn encode(&self) -> pb_mapper_core::error::Result<Vec<u8>> {
                serde_json::to_vec(self).map_err(|e| crate::secure::protocol_error(e.to_string()))
            }
            fn decode(bytes: &[u8]) -> pb_mapper_core::error::Result<Self> {
                serde_json::from_slice(bytes).map_err(|e| crate::secure::protocol_error(e.to_string()))
            }
        }
    )+};
}
wire_message!(
    DirectClientMessage,
    DirectProviderMessage,
    DirectEvent,
    SessionGrant
);

#[cfg(test)]
mod tests {
    /// Each label is its value's serialized form, so a front end printing
    /// `as_str` agrees with the JSON that carries the value; a relay carrier
    /// parses back from exactly its label, and from nothing else.
    #[test]
    fn labels_are_the_wire_names_and_parse_back_exactly() {
        use super::{SessionPath, SessionState};
        use crate::transport::RelayTransport;
        fn wire(value: impl serde::Serialize) -> String {
            serde_json::to_value(value)
                .unwrap()
                .as_str()
                .unwrap()
                .into()
        }
        for path in [SessionPath::Direct, SessionPath::Relay] {
            assert_eq!(wire(path), path.as_str());
        }
        for state in [SessionState::Active, SessionState::Closed] {
            assert_eq!(wire(state), state.as_str());
        }
        for transport in [RelayTransport::Tcp, RelayTransport::Quic] {
            assert_eq!(wire(transport), transport.as_str());
            assert_eq!(transport.as_str().parse(), Ok(transport));
        }
        for label in ["TCP", " tcp", ""] {
            let error = label.parse::<RelayTransport>().unwrap_err();
            assert_eq!(error.label(), label);
        }
    }

    /// LAN TCP: a peer that neither asks nor serves writes what it always
    /// has, and what it always wrote reads as neither. The command messages'
    /// unset direct fields are pinned with the rest of their wire form in
    /// `command.rs`.
    #[test]
    fn a_peer_without_lan_tcp_keeps_its_wire_form() {
        let quic = br#"{"certificate":[1],"candidates":["192.168.1.5:41234"]}"#;
        let mut peer = super::PeerInfo {
            certificate: vec![1],
            candidates: vec!["192.168.1.5:41234".parse().unwrap()],
            lan_tcp: None,
        };
        assert_eq!(serde_json::to_vec(&peer).unwrap(), quic);
        assert_eq!(
            serde_json::from_slice::<super::PeerInfo>(quic).unwrap(),
            peer
        );
        peer.lan_tcp = Some(vec![]);
        assert!(
            serde_json::to_string(&peer)
                .unwrap()
                .ends_with(r#","lan_tcp":[]}"#)
        );
        peer.lan_tcp = Some(vec!["192.168.1.5:41234".parse().unwrap(); 5]);
        assert!(!peer.valid(), "at most four TCP addresses");
    }

    /// A report without a link reads exactly as before, so an old relay and
    /// old peers see nothing new; one with a kind this build does not know
    /// still decodes, so a later endpoint cannot break the session list.
    #[test]
    fn a_session_report_link_is_optional_and_tolerates_later_kinds() {
        use super::{
            AddressFamily, DirectTransport, LinkKind, LinkSummary, SessionPath, SessionReport,
        };
        let mut report = SessionReport {
            id: [7; 16],
            seq: 3,
            path: SessionPath::Direct,
            state: super::SessionState::Active,
            reason: None,
            link: None,
        };
        let id = serde_json::to_string(&[7u8; 16]).unwrap();
        assert_eq!(
            serde_json::to_string(&report).unwrap(),
            format!(r#"{{"id":{id},"seq":3,"path":"direct","state":"active","reason":null}}"#)
        );
        report.link = Some(LinkSummary {
            kind: LinkKind::Lan,
            family: AddressFamily::Ipv4,
            transport: DirectTransport::Quic,
        });
        // A QUIC path reads as it did before there was a choice.
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.ends_with(r#","link":{"kind":"lan","family":"ipv4"}}"#));
        let later = json.replace(
            r#""lan","family":"ipv4""#,
            r#""mesh","family":"ipv9","transport":"sctp""#,
        );
        let parsed: SessionReport = serde_json::from_str(&later).unwrap();
        assert_eq!(
            parsed.link,
            Some(LinkSummary {
                kind: LinkKind::Unknown,
                family: AddressFamily::Unknown,
                transport: DirectTransport::Unknown,
            })
        );
        report.link = Some(LinkSummary {
            transport: DirectTransport::Tcp,
            ..parsed.link.unwrap()
        });
        assert!(
            serde_json::to_string(&report)
                .unwrap()
                .ends_with(r#""transport":"tcp"}}"#)
        );
    }

    #[test]
    fn unspecified_stun_address_takes_the_dialed_relay_ip() {
        use super::stun_target;
        let relay: std::net::IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(
            stun_target("0.0.0.0:7666".parse().unwrap(), relay),
            "203.0.113.7:7666".parse().unwrap()
        );
        assert_eq!(
            stun_target("[::]:7666".parse().unwrap(), relay),
            "203.0.113.7:7666".parse().unwrap()
        );
        // An IPv4 relay reached through a dual-stack socket keeps its v4 form.
        assert_eq!(
            stun_target(
                "0.0.0.0:7666".parse().unwrap(),
                "::ffff:203.0.113.7".parse().unwrap()
            ),
            "203.0.113.7:7666".parse().unwrap()
        );
        // An operator-supplied address always wins.
        assert_eq!(
            stun_target("198.51.100.1:10811".parse().unwrap(), relay),
            "198.51.100.1:10811".parse().unwrap()
        );
    }
}
