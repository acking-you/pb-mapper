//! What the window reads: tunnels, their paths and the relays, as JSON.

use pb_mapper_client::sdk::{
    AddressFamily, DirectLink, DirectStage, DirectTransport, LanEvidence, LinkKind, LinkVia,
    PortMapping, RelayTransport, StunOutcome, TunnelDiagnostics, TunnelEventKind,
};
use serde::Serialize;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServiceConfigInfo {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_encryption: bool,
    pub enable_keep_alive: bool,
    pub allow_direct: bool,
    pub port_mapping: PortMapping,
    pub status: TunnelState,
    pub status_message: String,
    /// How its sessions travel, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathInfo>,
    /// How it reaches the relay, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_transport: Option<RelayTransport>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientConfigInfo {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_keep_alive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<u64>,
    /// While it runs, the namespace the relay routes it in: [`Self::namespace`],
    /// or else that of the credential it was started with, which a credential
    /// changed since does not move.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routed_namespace: Option<u64>,
    pub allow_direct: bool,
    pub port_mapping: PortMapping,
    pub status: TunnelState,
    pub status_message: String,
    /// How its sessions travel, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathInfo>,
    /// How it reaches the relay, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_transport: Option<RelayTransport>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// How a running tunnel's sessions travel: each TCP connection or UDP flow
/// through it takes a direct path or the relay, decided when it opens.
///
/// A connection and a share both count every session on each path; a
/// connection also counts its direct attempts, which a share does not make.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PathInfo {
    /// Sessions open now, on each path, and of the direct ones those within
    /// one local network.
    pub active_direct: u64,
    pub active_relay: u64,
    pub active_lan: u64,
    /// Sessions opened since the tunnel started, on each path, and of the
    /// direct ones those within one local network.
    pub direct_sessions: u64,
    pub relay_sessions: u64,
    pub lan_sessions: u64,
    /// The stage reached by a direct attempt in flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<DirectStage>,
    /// How the latest attempt ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<PathOutcome>,
    /// The newest direct session open now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<PathLink>,
    /// The application bytes its sessions carried on each path, `sent`
    /// from this computer's side; `direct` includes `lan`.
    pub traffic: pb_mapper_client::sdk::PathTraffic,
    /// The smoothed round trip of its control exchanges with the relay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_rtt_ms: Option<u64>,
}

/// How a direct attempt ended.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "path", rename_all = "camelCase")]
pub enum PathOutcome {
    /// It went direct, set up in this long, over a path of this kind.
    Direct {
        #[serde(rename = "setupMs")]
        setup_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<LinkKind>,
    },
    /// It gave way to the relay at this stage. Later sessions go straight to
    /// the relay for a while (10 s, doubling to 5 min) before trying again.
    Relay { stage: DirectStage },
}

/// A direct session's path: the SDK's [`DirectLink`], as the window reads
/// it.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PathLink {
    /// `lan`, `internet` or `private`.
    pub kind: LinkKind,
    pub family: AddressFamily,
    /// How the peer address was learned.
    pub via: LinkVia,
    /// `quic`, or `tcp` within a local network.
    pub transport: DirectTransport,
    pub peer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    /// The round trip now and its lowest, rounded up to whole
    /// milliseconds, so that a LAN never reads 0: QUIC's, or over TCP the
    /// handshake's for both.
    pub rtt_ms: u64,
    pub min_rtt_ms: u64,
    pub setup_ms: u64,
    /// What suggested a shared network before the setup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_lan: Option<LanEvidence>,
    /// The stages of the setup on this end; one it did not run is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negotiate_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discover_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stun_ms: Option<u64>,
    pub stun: StunOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_pong_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lan_first_pong_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handshake_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_ms: Option<u64>,
}

impl From<&DirectLink> for PathLink {
    fn from(link: &DirectLink) -> Self {
        let timings = link.timings;
        Self {
            kind: link.kind,
            family: link.family,
            via: link.via,
            transport: link.transport,
            peer: link.peer.to_string(),
            local_ip: link.local_ip.map(|ip| ip.to_string()),
            interface: link.interface.clone(),
            rtt_ms: link.stats.rtt_us.div_ceil(1000),
            min_rtt_ms: link.stats.min_rtt_us.div_ceil(1000),
            setup_ms: timings.setup_ms,
            same_lan: link.same_lan,
            negotiate_ms: timings.negotiate_ms,
            discover_ms: timings.discover_ms,
            stun_ms: timings.stun_ms,
            stun: timings.stun,
            grant_ms: timings.grant_ms,
            first_pong_ms: timings.first_pong_ms,
            lan_first_pong_ms: timings.lan_first_pong_ms,
            handshake_ms: timings.handshake_ms,
            commit_ms: timings.commit_ms,
        }
    }
}

/// What a change of a tunnel's path is worth telling the window about:
/// which paths are in use, the LAN among them, an attempt, and how the last
/// one ended; not the counts, nor the link's live figures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PathShape {
    direct: bool,
    relay: bool,
    lan: bool,
    attempt: Option<DirectStage>,
    last_direct: Option<bool>,
    last_kind: Option<LinkKind>,
}

impl PathInfo {
    /// What the diagnostics of a running tunnel say about its paths; None
    /// before any session or attempt.
    pub(super) fn of(diagnostics: &TunnelDiagnostics) -> Option<Self> {
        let direct = diagnostics.direct.as_ref()?;
        let last = diagnostics
            .events
            .iter()
            .rev()
            .find_map(|event| match event.kind {
                TunnelEventKind::DirectEstablished { setup_ms } => Some(PathOutcome::Direct {
                    setup_ms,
                    kind: direct.last_kind,
                }),
                TunnelEventKind::DirectFallback { stage, .. } => Some(PathOutcome::Relay { stage }),
                _ => None,
            });
        Some(Self {
            active_direct: direct.active_direct,
            active_relay: direct.active_relay,
            active_lan: direct.active_lan,
            direct_sessions: direct.direct_sessions,
            relay_sessions: direct.relay_sessions,
            lan_sessions: direct.lan_sessions,
            attempt: direct.attempt.map(|attempt| attempt.stage),
            last,
            link: direct.link.as_ref().map(PathLink::from),
            traffic: diagnostics.traffic,
            relay_rtt_ms: diagnostics.relay_rtt_ms,
        })
    }

    pub(super) fn shape(&self) -> PathShape {
        let last_kind = match self.last {
            Some(PathOutcome::Direct { kind, .. }) => kind,
            Some(PathOutcome::Relay { .. }) | None => None,
        };
        PathShape {
            direct: self.active_direct > 0,
            relay: self.active_relay > 0,
            lan: self.active_lan > 0,
            attempt: self.attempt,
            last_direct: self
                .last
                .map(|last| matches!(last, PathOutcome::Direct { .. })),
            last_kind,
        }
    }
}

/// The relay this computer runs itself. The default is a stopped one.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocalServerStatus {
    pub is_running: bool,
    /// The port it listens on, while it runs: the page shows and dials this,
    /// not whatever its port field last held.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The UDP port it takes QUIC on, while it runs with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quic_port: Option<u16>,
    pub active_connections: u32,
    pub registered_services: u32,
    pub uptime_seconds: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatusDetail {
    pub server_available: bool,
    pub registered_services: Vec<String>,
    pub server_map: String,
    pub active_connections: String,
    pub idle_connections: String,
    /// Why the relay did not answer, for the person who asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How long it took to answer, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// The UDP port the relay offers QUIC on, when it said it offers it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quic_port: Option<u16>,
    /// Whether the relay said whether it offers QUIC, which it is asked over
    /// TCP: so even when the carrier this computer uses does not get through.
    pub quic_known: bool,
}

/// One control connection the server is holding for a service key.
///
/// Mirrors `PbServiceConnStatus` from the protocol. Every field is real,
/// which is the point: the string this replaces could only ever say that a
/// connection existed.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServiceConnInfo {
    pub conn_id: u32,
    pub generation: u64,
    pub protocol_version: u16,
    pub healthy: bool,
    pub last_rx_age_ms: u64,
}

/// A tunnel's state as the list shows it.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TunnelState {
    Running,
    /// Still dialling, with no failure to report.
    Retrying,
    /// Failed, whether or not it is trying again.
    Failed,
    /// Configured, but not running.
    Stopped,
}

/// One tunnel's state, for a share or a connection alike.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TunnelStatusResponse {
    pub service_key: String,
    pub status: TunnelState,
    pub message: String,
}

/// This computer's tunnels on one relay, as rotating that relay's
/// administrator key leaves them (see [`PbMapperState::tunnels_on`]), or
/// as a change of carrier or LAN-first does, which strands none.
#[derive(Default, Debug)]
pub struct RelayTunnels {
    /// To start again: on the retired administrator key, to start on the
    /// new one, or started with the setting this computer just left.
    pub shares: Vec<String>,
    pub connections: Vec<String>,
    /// On another credential, a temporary one, which the rotation erased.
    /// The administrator key is not theirs to take: a tunnel started under
    /// it lands in the administrator's namespace, not its own.
    pub stranded: Vec<String>,
}
