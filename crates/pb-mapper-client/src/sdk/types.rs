//! Small value types shared across the SDK surface, and their conversions from
//! the wire types in `pb-mapper-protocol`.
//!
//! Keeping owned mirrors here is what lets a caller depend on the SDK alone: the
//! protocol crate stays an implementation detail.

use crate::worker_status::WorkerStatus;
use pb_mapper_auth::LegacyProtocolPolicy;
use pb_mapper_protocol::command::{PbConnStatusResp, PbServiceConnStatus};
use snafu::Snafu;

/// A label that names no value of the option it was parsed as.
#[derive(Clone, Debug, Eq, PartialEq, Snafu)]
#[snafu(display("unknown value `{label}`"))]
pub struct UnknownLabel {
    label: String,
}

impl UnknownLabel {
    /// The label as it was given.
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Give an option enum its one table of labels: `as_str`, and a `FromStr`
/// that takes exactly those labels back, case and all.
macro_rules! labels {
    ($(#[$doc:meta])* $ty:ident { $($variant:ident => $label:literal),+ $(,)? }) => {
        impl $ty {
            $(#[$doc])*
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)+
                }
            }
        }

        impl std::str::FromStr for $ty {
            type Err = UnknownLabel;

            fn from_str(label: &str) -> Result<Self, UnknownLabel> {
                match label {
                    $($label => Ok(Self::$variant),)+
                    _ => UnknownLabelSnafu { label }.fail(),
                }
            }
        }
    };
}

/// Transport used by a registered service or a local subscriber.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    Tcp,
    Udp,
}

labels! {
    /// Stable label, as the CLI and the Node addon spell it.
    Transport { Tcp => "tcp", Udp => "udp" }
}

impl Transport {
    pub(crate) fn is_datagram(self) -> bool {
        match self {
            Self::Tcp => false,
            Self::Udp => true,
        }
    }
}

/// Configuration for a [`Client`](super::Client) session.
#[derive(Clone)]
pub struct ClientConfig {
    /// Relay address (`host:port`).
    pub server: String,
    /// Administrator key (32 printable bytes) or a `pbmt1_` temporary credential.
    pub credential: String,
    pub keep_alive: bool,
    /// Administrator-only target namespace. Temporary credentials always use
    /// their own namespace when this is `None`.
    pub namespace: Option<u64>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("server", &self.server)
            .field("credential", &"[redacted]")
            .field("keep_alive", &self.keep_alive)
            .field("namespace", &self.namespace)
            .finish()
    }
}

/// Register a local TCP/UDP service with the relay.
#[derive(Clone, Debug)]
pub struct RegisterRequest {
    pub key: String,
    pub local_addr: String,
    pub transport: Transport,
    pub codec: bool,
    pub force_namespace: bool,
}

/// Subscribe to a registered service and listen locally.
#[derive(Clone, Debug)]
pub struct ConnectRequest {
    pub key: String,
    pub local_addr: String,
    pub transport: Transport,
}

/// Observed lifecycle of a [`super::Registration`] or [`super::Connection`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TunnelStatus {
    Starting,
    Connected,
    Retrying,
    Stopped,
    Failed(String),
}

impl TunnelStatus {
    /// What a worker's report means to a caller of the SDK.
    pub(crate) fn from_worker(status: &WorkerStatus) -> Self {
        match status {
            WorkerStatus::Connected => Self::Connected,
            WorkerStatus::Retrying => Self::Retrying,
            WorkerStatus::Failed(None) => Self::Failed("failed".into()),
            // The reason is the only description of a permanent rejection
            // the caller ever gets. Trimmed: one a `snafu::Report` rendered
            // ends in a line break.
            WorkerStatus::Failed(Some(reason)) => Self::Failed(reason.trim().to_owned()),
        }
    }
}

/// Relay mapping dump from `status remote-id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteId {
    pub server_map: String,
    pub active: String,
    pub idle: String,
}

impl RemoteId {
    pub(crate) fn from_status(status: PbConnStatusResp) -> super::Result<Self> {
        match status {
            PbConnStatusResp::RemoteId {
                server_map,
                active,
                idle,
            } => Ok(Self {
                server_map,
                active,
                idle,
            }),
            other => Err(super::Error::unexpected("remote-id status", &other)),
        }
    }
}

/// One control connection the relay is holding for a service key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceConnection {
    pub conn_id: u32,
    pub generation: u64,
    pub protocol_version: u16,
    pub healthy: bool,
    pub last_rx_age_ms: u64,
}

impl From<PbServiceConnStatus> for ServiceConnection {
    fn from(value: PbServiceConnStatus) -> Self {
        let PbServiceConnStatus {
            conn_id,
            generation,
            protocol_version,
            healthy,
            last_rx_age_ms,
        } = value;
        Self {
            conn_id,
            generation,
            protocol_version,
            healthy,
            last_rx_age_ms,
        }
    }
}

/// Legacy framing policy, matching `pb-mapper admin legacy-protocol`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacyProtocol {
    Allow,
    Deny,
}

labels! {
    /// Stable label, as the CLI and the Node addon spell it.
    LegacyProtocol { Allow => "allow", Deny => "deny" }
}

impl From<LegacyProtocol> for LegacyProtocolPolicy {
    fn from(value: LegacyProtocol) -> Self {
        match value {
            LegacyProtocol::Allow => Self::Allow,
            LegacyProtocol::Deny => Self::Deny,
        }
    }
}

impl From<LegacyProtocolPolicy> for LegacyProtocol {
    fn from(value: LegacyProtocolPolicy) -> Self {
        match value {
            LegacyProtocolPolicy::Allow => Self::Allow,
            LegacyProtocolPolicy::Deny => Self::Deny,
        }
    }
}

/// Per-connect path policy.
///
/// The default is [`Self::Auto`] when the SDK is built with the `p2p` feature,
/// and [`Self::RelayOnly`] when it is not, since only `Auto` needs it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathPolicy {
    /// Use the relay for every connection, with the pre-direct protocol.
    RelayOnly,
    /// Try a direct QUIC path first; fall back to the relay if it cannot be
    /// set up, before any application byte is sent.
    Auto,
}

labels! {
    /// Stable label, as the CLI and the Node addon spell it.
    PathPolicy { RelayOnly => "relay-only", Auto => "auto" }
}

impl Default for PathPolicy {
    fn default() -> Self {
        if cfg!(feature = "p2p") {
            Self::Auto
        } else {
            Self::RelayOnly
        }
    }
}

/// Whether a tunnel asks this network's router (PCP, NAT-PMP or UPnP) to map
/// its direct endpoint's UDP port, so that a peer's first packet gets in
/// where the NAT drops what it has not seen. A mapping never holds up a
/// session, is renewed while the tunnel runs and released when it stops,
/// and is ignored while the tunnel keeps to the relay;
/// [`TunnelDiagnostics::port_mapping`] says where it stands.
///
/// [`TunnelDiagnostics::port_mapping`]: crate::sdk::TunnelDiagnostics::port_mapping
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMapping {
    /// Never ask the router.
    Off,
    /// The default: ask only once a direct attempt of this tunnel has failed
    /// to punch through, a sign that a NAT drops unsolicited packets; later
    /// sessions then offer the mapped address. Where punching works, the
    /// router is never asked. In a build without the `port-mapping` feature
    /// there is nothing to ask with, and this is `Off`.
    #[default]
    Auto,
    /// Ask as soon as the tunnel's direct endpoint opens. Needs the
    /// `port-mapping` feature: a build without it refuses the tunnel.
    Always,
}

labels! {
    /// Stable label, matching the serialized representation.
    PortMapping { Off => "off", Auto => "auto", Always => "always" }
}

impl PortMapping {
    /// Refuse a mapping from the start that this build cannot make, rather
    /// than ignore it. One when needed is best effort, and the default:
    /// without the feature it never starts.
    pub(crate) fn check_supported(self) -> super::Result<()> {
        if self == Self::Always && !cfg!(feature = "port-mapping") {
            return Err(super::Error::invalid_config(
                "built without port-mapping support",
            ));
        }
        Ok(())
    }
}

/// What a direct path within a local network runs over. A path across the
/// internet is always QUIC: TCP is offered only between ends that share a
/// LAN, so a hole-punched path never takes it.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LanTransport {
    /// The default: QUIC over the same UDP socket as every other direct path,
    /// which the provider's own probes open through its firewall.
    #[default]
    Quic,
    /// TLS 1.3 over TCP, tried first, 300 ms ahead of every QUIC path while
    /// LAN-first is on, and raced with them while it is off. It needs the
    /// provider's firewall to let inbound TCP in; where it does not, or the
    /// provider does not serve it, the session takes QUIC as it would have.
    Tcp,
}

labels! {
    /// Stable label, matching the serialized representation.
    LanTransport { Quic => "quic", Tcp => "tcp" }
}

#[derive(Clone, Copy, Debug)]
pub struct ConnectOptions {
    pub path: PathPolicy,
    /// `Auto` by default, and ignored on the relay-only path.
    pub port_mapping: PortMapping,
    /// Try a local network this end shares with the publisher first. On, the
    /// publisher's addresses on that network are probed ahead of the others
    /// and again 20 and 50 ms later; and once the network is known to be
    /// there, by strong evidence or by an answer from a path on it, a path
    /// off it waits up to 150 ms after its own answer before it is dialed,
    /// unless a LAN handshake completes or every LAN path fails first. Off,
    /// the publisher's addresses are probed in the order it offered them and
    /// every path that answers is dialed at once. What either end offers,
    /// and how a path is reported, are the same either way.
    ///
    /// On by default. It is this end's policy alone: nothing about it crosses
    /// the wire, and the publisher follows its own. Ignored on the relay-only
    /// path.
    pub lan_first: bool,
    /// What a path within a local network this end shares with the publisher
    /// runs over: QUIC by default. Unlike `lan_first` it crosses the wire, as
    /// a request: `Tcp` asks the publisher for its TCP addresses on the LAN,
    /// which it gives under its own [`RegisterOptions::lan_tcp`]. A publisher
    /// or relay that does not, and every path across the internet, leaves
    /// the session on QUIC. Ignored on the relay-only path.
    pub lan_transport: LanTransport,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            path: PathPolicy::default(),
            port_mapping: PortMapping::Auto,
            lan_first: true,
            lan_transport: LanTransport::Quic,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RegisterOptions {
    /// Accept direct sessions from authorized consumers. Defaults to `true`
    /// with the `p2p` feature. Allowing it costs nothing until a consumer asks:
    /// the UDP socket is opened on the first offer.
    pub allow_direct: bool,
    pub idle_timeout: Option<std::time::Duration>,
    /// `Auto` by default, and ignored unless `allow_direct`.
    pub port_mapping: PortMapping,
    /// Try a local network this end shares with a consumer first: probe the
    /// consumer's addresses on it ahead of the others, and again 20 and 50 ms
    /// later. Off, they are probed in the order the consumer offered them.
    /// Which path a session takes is the dialing consumer's choice, under its
    /// own [`ConnectOptions::lan_first`].
    ///
    /// On by default. It is this end's policy alone, and nothing about it
    /// crosses the wire. Ignored unless `allow_direct`.
    pub lan_first: bool,
    /// Serve a consumer that asks for [`LanTransport::Tcp`] and shares a
    /// local network with this end: open a TCP listener for direct sessions
    /// the first time one asks, and offer its addresses on that network only.
    /// Off, such a consumer gets QUIC, as from a publisher that predates it.
    ///
    /// On by default; nothing listens until a consumer asks. Ignored unless
    /// `allow_direct`.
    pub lan_tcp: bool,
}

impl Default for RegisterOptions {
    fn default() -> Self {
        Self {
            allow_direct: cfg!(feature = "p2p"),
            idle_timeout: None,
            port_mapping: PortMapping::Auto,
            lan_first: true,
            lan_tcp: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_debug_redacts_credentials() {
        let config = ClientConfig {
            server: "localhost:7666".into(),
            credential: "0123456789abcdefghijklmnopqrstuv".into(),
            keep_alive: false,
            namespace: None,
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains(&config.credential));
        assert!(debug.contains("[redacted]"));
    }

    /// The labels the CLI, the Node addon and the app read and write: each
    /// value's label parses back to it, exactly as spelled, and where a
    /// value is serialized, it is serialized as its label.
    #[test]
    fn option_labels_are_the_ones_every_front_end_spells() {
        macro_rules! assert_labels {
            ($($value:expr => $label:literal),+ $(,)?) => {$(
                assert_eq!($value.as_str(), $label);
                assert_eq!($label.parse(), Ok($value));
            )+};
        }
        assert_labels!(
            Transport::Tcp => "tcp",
            Transport::Udp => "udp",
            LegacyProtocol::Allow => "allow",
            LegacyProtocol::Deny => "deny",
            PathPolicy::RelayOnly => "relay-only",
            PathPolicy::Auto => "auto",
            PortMapping::Off => "off",
            PortMapping::Auto => "auto",
            PortMapping::Always => "always",
            LanTransport::Quic => "quic",
            LanTransport::Tcp => "tcp",
        );
        assert_eq!(
            "TCP".parse::<Transport>(),
            Err(UnknownLabel {
                label: "TCP".into()
            })
        );
        for mapping in [PortMapping::Off, PortMapping::Auto, PortMapping::Always] {
            assert_eq!(serde_json::to_value(mapping).unwrap(), mapping.as_str());
        }
        for transport in [LanTransport::Quic, LanTransport::Tcp] {
            assert_eq!(serde_json::to_value(transport).unwrap(), transport.as_str());
        }
    }
}
