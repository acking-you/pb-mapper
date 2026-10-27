//! Relay-carrier configuration and on-demand observations, independent of P2P.
use pb_mapper_core::{checksum::Credential, config::ResolvedAddrs};
pub use pb_mapper_protocol::transport::RelayTransport;
use pb_mapper_protocol::{
    command::{MessageSerializer, PbConnRequest, PbConnResponse, PbErrorResponse, QuicEndpoint},
    secure::ClientHeaderSession,
    transport::CONNECT_TIMEOUT,
};
use serde::Serialize;
use std::{io, net::SocketAddr};

/// Carrier setup can already receive an authenticated refusal, before a
/// registration, subscription or administrator request has been sent.
#[derive(Debug, snafu::Snafu)]
pub(crate) enum ConnectError {
    #[snafu(display("{source}"))]
    Transport { source: std::io::Error },
    #[snafu(display("{}: {}", response.code, response.message))]
    Rejected { response: PbErrorResponse },
}

impl From<std::io::Error> for ConnectError {
    fn from(source: std::io::Error) -> Self {
        Self::Transport { source }
    }
}

impl From<ConnectError> for std::io::Error {
    fn from(error: ConnectError) -> Self {
        match error {
            ConnectError::Transport { source } => source,
            error @ ConnectError::Rejected { .. } => {
                Self::new(std::io::ErrorKind::PermissionDenied, error)
            }
        }
    }
}

/// The refusal code for a relay that offers no QUIC: one started without a
/// QUIC listener, or one from before QUIC, which cannot read the question.
/// Final, since retrying the same relay cannot change it, and so a tunnel
/// asked to use QUIC there fails rather than retrying forever.
pub const QUIC_UNAVAILABLE: &str = "quic_unavailable";

/// What an older relay answers a request it cannot decode, which is how it
/// answers [`PbConnRequest::QuicEndpoint`].
const REQUEST_DECODE_FAILED: &str = "request_decode_failed";

fn quic_unavailable() -> ConnectError {
    ConnectError::Rejected {
        response: PbErrorResponse {
            code: QUIC_UNAVAILABLE.into(),
            message: "the relay does not offer QUIC; select TCP to use it".into(),
            retryable: false,
            server_time: 0,
        },
    }
}

/// Ask the relay at `addr`, over one authenticated TCP exchange, where it
/// takes QUIC. Answers the relay address that TCP reached, whose IP QUIC
/// shares, with the advertised endpoint.
pub(crate) async fn discover(
    addr: SocketAddr,
    credential: Credential,
) -> Result<(SocketAddr, QuicEndpoint), ConnectError> {
    let mut tcp = tokio::net::TcpStream::connect(addr).await?;
    tcp.set_nodelay(true)?;
    let relay_addr = tcp.peer_addr()?;
    let session = ClientHeaderSession::new_v2(&credential).map_err(io::Error::other)?;
    let response = session
        .exchange(
            &mut tcp,
            &PbConnRequest::QuicEndpoint
                .encode()
                .map_err(io::Error::other)?,
            CONNECT_TIMEOUT,
        )
        .await
        .map_err(io::Error::other)?;
    match PbConnResponse::decode(&response).map_err(io::Error::other)? {
        PbConnResponse::QuicEndpoint(Some(endpoint))
            if endpoint.port != 0
                && !endpoint.certificate.is_empty()
                && endpoint.certificate.len() <= 4096 =>
        {
            Ok((relay_addr, endpoint))
        }
        // Named apart from an unexpected answer: a misconfigured relay is
        // found by what it advertised.
        PbConnResponse::QuicEndpoint(Some(endpoint)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the relay advertised an unusable QUIC endpoint: UDP port {}, \
                 a {}-byte certificate",
                endpoint.port,
                endpoint.certificate.len()
            ),
        )
        .into()),
        PbConnResponse::QuicEndpoint(None) => Err(quic_unavailable()),
        PbConnResponse::Error(error) if error.code == REQUEST_DECODE_FAILED => {
            Err(quic_unavailable())
        }
        PbConnResponse::Error(response) => Err(ConnectError::Rejected { response }),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected QUIC discovery response: {other:?}"),
        )
        .into()),
    }
}

/// The UDP port the relay offers QUIC on, or `None` when it offers none.
pub(crate) async fn offered_quic_port(
    addrs: &ResolvedAddrs,
    credential: Credential,
) -> Result<Option<u16>, ConnectError> {
    match crate::addr::race_candidates(addrs.as_slice(), CONNECT_TIMEOUT, move |addr| {
        discover(addr, credential)
    })
    .await
    {
        Ok((_, endpoint)) => Ok(Some(endpoint.port)),
        Err(ConnectError::Rejected { response }) if response.code == QUIC_UNAVAILABLE => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(feature = "relay-quic")]
pub(crate) mod quic;

/// The relay carrier as this client's pool sees it, sampled on request.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct RelayDiagnostics {
    pub transport: RelayTransport,
    /// Successful QUIC handshakes over this pool's lifetime.
    pub connections_created: u64,
    pub streams_opened: u64,
    pub streams_reused: u64,
    pub last_error: Option<String>,
    /// Every open QUIC connection: those the pool hands out, then those it
    /// has retired that streams opened earlier still carry traffic on.
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

/// Cumulative Quinn counters for one live connection; sampled only on request.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct QuicConnectionDiagnostics {
    pub connection_id: u64,
    pub peer_addr: std::net::SocketAddr,
    /// Whether the pool still hands it out. A retired one stays open only
    /// for the streams already on it.
    pub pooled: bool,
    pub rtt_us: u64,
    pub congestion_window: u64,
    pub mtu: u16,
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub congestion_events: u64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}
