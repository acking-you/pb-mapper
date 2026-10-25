//! Pinned TLS QUIC primitives shared by relay clients and servers.
use quinn::{Connection, Endpoint, RecvStream, SendStream, rustls};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
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
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

fn transport(streams: u32, client: bool) -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config
        .max_concurrent_bidi_streams(streams.into())
        .max_concurrent_uni_streams(0u32.into())
        .stream_receive_window((8u32 << 20).into())
        .receive_window((32u32 << 20).into())
        .send_window(32u64 << 20)
        .max_idle_timeout(Some(quinn::VarInt::from_u32(60_000).into()))
        .keep_alive_interval(client.then_some(Duration::from_secs(15)))
        .datagram_receive_buffer_size(None);
    Arc::new(config)
}

pub fn server_config() -> io::Result<(quinn::ServerConfig, Vec<u8>)> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec![SERVER_NAME.into()]).map_err(io::Error::other)?;
    let certificate = cert.der().to_vec();
    let mut tls = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(io::Error::other)?
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
    )
    .map_err(io::Error::other)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.max_early_data_size = 0;
    let crypto =
        quinn::crypto::rustls::QuicServerConfig::try_from(tls).map_err(io::Error::other)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(transport(1, false));
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
    config.transport_config(transport(0, true));
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
#[derive(Debug)]
pub struct QuicConnection {
    pub connection: Connection,
    pub endpoint: Endpoint,
    pub relay_addr: SocketAddr,
}

#[derive(Debug)]
pub struct QuicStream {
    recv: RecvStream,
    send: SendStream,
    owner: Arc<QuicConnection>,
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
