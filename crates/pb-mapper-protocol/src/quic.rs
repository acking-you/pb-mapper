//! Pinned TLS QUIC primitives shared by relay clients and servers.
use quinn::{Connection, Endpoint, RecvStream, SendStream, rustls};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub const ALPN: &[u8] = b"pb-mapper/relay/1";
pub const SERVER_NAME: &str = "pb-mapper-relay.invalid";
pub const MAX_STREAMS: u32 = 1024;
pub use crate::transport::CONNECT_TIMEOUT;

/// What one stream may have in flight unread: 4 MiB, a stream per 4 MiB of
/// round trip, as much as a TCP carrier's autotuned buffer allows.
const STREAM_RECEIVE_WINDOW: u32 = 4 << 20;

/// The connection-wide receive window once the relay has admitted a peer.
///
/// Every tunnel of a client shares its connection, and bytes received but
/// not yet forwarded count against this window whichever stream they are
/// on. A stream whose far end stops reading holds a full 4 MiB stream
/// window here, so this is sixteen of them: fewer
/// stalled downloads cannot hold back the client's other tunnels, its
/// registrations' heartbeats among them.
pub const RECEIVE_WINDOW: u32 = 16 * STREAM_RECEIVE_WINDOW;

/// What an unauthenticated peer may make the relay buffer: its first frame
/// and some slack. Raised to [`RECEIVE_WINDOW`] on admission, so a flood of
/// handshakes that never authenticate cannot pin a stream window each.
const PRE_ADMISSION_WINDOW: u32 = 256 << 10;
const _: () = assert!(
    PRE_ADMISSION_WINDOW as usize >= crate::secure::MAX_FIRST_FLIGHT_LEN,
    "an unauthenticated peer must be able to send one whole first flight"
);

/// Which end of a relay connection a transport configuration is for.
#[derive(Clone, Copy)]
enum Side {
    Relay,
    Client,
}

fn transport(side: Side) -> Arc<quinn::TransportConfig> {
    let (peer_streams, receive_window, keep_alive) = match side {
        Side::Relay => (1_u32, PRE_ADMISSION_WINDOW.into(), None),
        // A client buffers only for streams it opened itself, a stream
        // window each, so it does not cap the sum: a relay stream that the
        // local application stops reading never stalls the others. The
        // relay opens none.
        Side::Client => (0, quinn::VarInt::MAX, Some(Duration::from_secs(15))),
    };
    let mut config = quinn::TransportConfig::default();
    config
        .max_concurrent_bidi_streams(peer_streams.into())
        .max_concurrent_uni_streams(0u32.into())
        .stream_receive_window(STREAM_RECEIVE_WINDOW.into())
        .receive_window(receive_window)
        .send_window(32u64 << 20)
        .max_idle_timeout(Some(quinn::VarInt::from_u32(60_000).into()))
        .keep_alive_interval(keep_alive)
        .datagram_receive_buffer_size(None);
    Arc::new(config)
}

pub fn server_config() -> io::Result<(quinn::ServerConfig, Vec<u8>)> {
    let (rcgen::CertifiedKey { cert, .. }, key) =
        rcgen::generate_simple_self_signed(vec![SERVER_NAME.into()]).map_err(io::Error::other)?;
    let certificate = cert.der().to_vec();
    let mut tls = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(io::Error::other)?
    .with_no_client_auth()
    .with_single_cert(vec![cert.der().clone()], key)
    .map_err(io::Error::other)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.max_early_data_size = 0;
    let crypto =
        quinn::crypto::rustls::QuicServerConfig::try_from(tls).map_err(io::Error::other)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(transport(Side::Relay));
    config.max_incoming(64).incoming_buffer_size_total(4 << 20);
    Ok((config, certificate))
}

/// Trust only the exact self-signed leaf received over authenticated PBM2.
pub fn client_config(certificate: &[u8]) -> io::Result<quinn::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(certificate.to_vec()))
        .map_err(io::Error::other)?;
    let mut tls = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(io::Error::other)?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
        certificate: certificate.to_vec(),
        webpki: rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            rustls::crypto::ring::default_provider().into(),
        )
        .build()
        .map_err(io::Error::other)?,
    }))
    .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.enable_early_data = false;
    let crypto =
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(io::Error::other)?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(transport(Side::Client));
    Ok(config)
}

#[derive(Debug)]
struct PinnedVerifier {
    certificate: Vec<u8>,
    webpki: Arc<rustls::client::WebPkiServerVerifier>,
}
impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        certificate: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if certificate.as_ref() != self.certificate || !intermediates.is_empty() {
            return Err(rustls::Error::General(
                "relay certificate does not match authenticated discovery".into(),
            ));
        }
        self.webpki
            .verify_server_cert(certificate, intermediates, name, ocsp, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(message, cert, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

pub fn binding(connection: &Connection) -> io::Result<[u8; 32]> {
    let mut bytes = [0; 32];
    connection
        .export_keying_material(&mut bytes, b"EXPORTER-pb-mapper-relay-v1", b"connection")
        .map_err(|_| io::Error::other("QUIC TLS exporter unavailable"))?;
    Ok(bytes)
}

/// Streams retain their UDP driver independently of the pool/control connection.
pub struct QuicConnection {
    pub connection: Connection,
    pub endpoint: Endpoint,
    pub relay_addr: SocketAddr,
}

/// Which connection, and where to: Quinn's own `Debug` prints its whole
/// connection state, kilobytes into any log line a stream appears in.
impl std::fmt::Debug for QuicConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicConnection")
            .field("id", &self.connection.stable_id())
            .field("peer", &self.connection.remote_address())
            .finish()
    }
}

pub struct QuicStream {
    recv: RecvStream,
    send: SendStream,
    owner: Arc<QuicConnection>,
}

impl std::fmt::Debug for QuicStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicStream")
            .field("connection", &self.owner)
            .field("stream", &self.send.id())
            .finish()
    }
}
impl QuicStream {
    pub fn new(owner: Arc<QuicConnection>, (send, recv): (SendStream, RecvStream)) -> Self {
        Self { recv, send, owner }
    }
    pub fn relay_addr(&self) -> SocketAddr {
        self.owner.relay_addr
    }
    pub fn peer_addr(&self) -> SocketAddr {
        self.owner.connection.remote_address()
    }
    pub fn split(&mut self) -> (&mut RecvStream, &mut SendStream) {
        (&mut self.recv, &mut self.send)
    }
    pub fn into_split(self) -> (QuicReadHalf, QuicWriteHalf) {
        (
            QuicReadHalf {
                recv: self.recv,
                _owner: self.owner.clone(),
            },
            QuicWriteHalf {
                send: self.send,
                _owner: self.owner,
            },
        )
    }
}
pub struct QuicReadHalf {
    recv: RecvStream,
    _owner: Arc<QuicConnection>,
}
pub struct QuicWriteHalf {
    send: SendStream,
    _owner: Arc<QuicConnection>,
}
macro_rules! read_impl {
    ($ty:ty) => {
        impl AsyncRead for $ty {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                AsyncRead::poll_read(Pin::new(&mut self.recv), cx, buf)
            }
        }
    };
}
macro_rules! write_impl {
    ($ty:ty) => {
        impl AsyncWrite for $ty {
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf)
            }
            fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                AsyncWrite::poll_flush(Pin::new(&mut self.send), cx)
            }
            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<io::Result<()>> {
                AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
            }
        }
    };
}
read_impl!(QuicStream);
read_impl!(QuicReadHalf);
write_impl!(QuicStream);
write_impl!(QuicWriteHalf);
