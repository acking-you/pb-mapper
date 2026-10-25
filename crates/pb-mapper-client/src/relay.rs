//! Relay-carrier configuration and on-demand observations, independent of P2P.
pub use pb_mapper_protocol::transport::RelayTransport;
use serde::Serialize;

#[cfg(feature = "relay-quic")]
pub(crate) mod quic;

#[derive(Clone, Debug, Serialize)]
pub struct RelayDiagnostics {
    pub transport: RelayTransport,
    /// Successful QUIC handshakes over this pool's lifetime.
    pub connections_created: u64,
    pub streams_opened: u64,
    pub streams_reused: u64,
    pub last_error: Option<String>,
    pub connections: Vec<QuicConnectionDiagnostics>,
}
impl RelayDiagnostics {
    pub(crate) fn empty(transport: RelayTransport) -> Self {
        Self {
            transport,
            connections_created: 0,
            streams_opened: 0,
            streams_reused: 0,
            last_error: None,
            connections: Vec::new(),
        }
    }
}

/// Cumulative Quinn counters for one live pooled connection; sampled only on request.
#[derive(Clone, Debug, Serialize)]
pub struct QuicConnectionDiagnostics {
    pub connection_id: u64,
    pub peer_addr: std::net::SocketAddr,
    pub rtt_us: u64,
    pub congestion_window: u64,
    pub mtu: u16,
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub congestion_events: u64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}
