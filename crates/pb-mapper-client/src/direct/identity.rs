//! Exact certificates arrive over the existing authenticated control channel.
//! WebPKI still checks validity/EKU and TLS still proves private-key possession.
use quinn::rustls::{
    self, DigitallySignedStruct, DistinguishedName, SignatureScheme,
    client::danger::HandshakeSignatureValid,
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use std::{
    collections::HashMap,
    io,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

/// The protocol a direct connection speaks, by its carrier: a connection
/// made for one is refused by the other.
const QUIC_ALPN: &[u8] = b"pb-mapper/direct/1";
const TCP_ALPN: &[u8] = b"pb-mapper/direct-tcp/1";

/// The name every direct certificate carries, and every dial asks for.
pub(super) const SERVER_NAME: &str = "pb-mapper.invalid";

/// Keep-alive on both carriers: a peer gone without a word is noticed
/// rather than waited for.
pub(super) const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// How long a QUIC connection may go without hearing from its peer.
const QUIC_IDLE: Duration = Duration::from_secs(60);
const _: () = assert!(
    QUIC_IDLE.as_secs() > KEEP_ALIVE.as_secs(),
    "keep-alives arrive before the idle timeout"
);

/// Peer certificates with a live grant that a provider admits at once.
const MAX_PERMITTED: usize = 1024;

/// The ring provider behind every direct TLS config and client certificate
/// check, built once rather than per config and per handshake.
static PROVIDER: LazyLock<Arc<CryptoProvider>> =
    LazyLock::new(|| Arc::new(rustls::crypto::ring::default_provider()));

pub(super) struct Identity {
    pub certificate: Vec<u8>,
    key: PrivateKeyDer<'static>,
    pub permissions: Arc<Permissions>,
}
impl Identity {
    pub fn new() -> io::Result<Self> {
        let (rcgen::CertifiedKey { cert, .. }, key) =
            rcgen::generate_simple_self_signed(vec![SERVER_NAME.into()])
                .map_err(io::Error::other)?;
        Ok(Self {
            certificate: cert.der().to_vec(),
            key,
            permissions: Arc::new(Permissions::default()),
        })
    }
    /// TLS 1.3 only, with this identity, admitting only a client certificate
    /// with a live grant, and no early data.
    fn tls_server(&self, alpn: &[u8]) -> io::Result<rustls::ServerConfig> {
        let mut tls = rustls::ServerConfig::builder_with_provider(PROVIDER.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(io::Error::other)?
            .with_client_cert_verifier(self.permissions.clone())
            .with_single_cert(
                vec![CertificateDer::from(self.certificate.clone())],
                self.key.clone_key(),
            )
            .map_err(io::Error::other)?;
        tls.alpn_protocols = vec![alpn.to_vec()];
        tls.max_early_data_size = 0;
        Ok(tls)
    }
    /// TLS 1.3 only, trusting `peer`'s exact certificate and nothing else,
    /// presenting this identity, and no early data.
    fn tls_client(&self, peer: &[u8], alpn: &[u8]) -> io::Result<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(peer.to_vec()))
            .map_err(io::Error::other)?;
        let mut tls = rustls::ClientConfig::builder_with_provider(PROVIDER.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(io::Error::other)?
            .with_root_certificates(roots)
            .with_client_auth_cert(
                vec![CertificateDer::from(self.certificate.clone())],
                self.key.clone_key(),
            )
            .map_err(io::Error::other)?;
        tls.alpn_protocols = vec![alpn.to_vec()];
        tls.enable_early_data = false;
        Ok(tls)
    }
    pub fn server(&self) -> io::Result<quinn::ServerConfig> {
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(self.tls_server(QUIC_ALPN)?)
            .map_err(io::Error::other)?;
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        config.transport_config(Arc::new(transport()?));
        Ok(config)
    }
    pub fn client(&self, peer: &[u8]) -> io::Result<quinn::ClientConfig> {
        let mut config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(self.tls_client(peer, QUIC_ALPN)?)
                .map_err(io::Error::other)?,
        ));
        config.transport_config(Arc::new(transport()?));
        Ok(config)
    }
    /// The server's config over TCP. Neither tickets nor a session cache:
    /// every connection presents, and has checked, a certificate with a
    /// live grant, as a QUIC one does.
    pub fn tcp_server(&self) -> io::Result<Arc<rustls::ServerConfig>> {
        let mut tls = self.tls_server(TCP_ALPN)?;
        tls.send_tls13_tickets = 0;
        tls.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        Ok(Arc::new(tls))
    }
    /// The client's config over TCP, to `peer`: no resumption either.
    pub fn tcp_client(&self, peer: &[u8]) -> io::Result<Arc<rustls::ClientConfig>> {
        let mut tls = self.tls_client(peer, TCP_ALPN)?;
        tls.resumption = rustls::client::Resumption::disabled();
        Ok(Arc::new(tls))
    }
}
/// Flow-control window of the single stream a direct connection carries.
/// Quinn's 1.25 MB default caps one stream at window/RTT, about 100 Mbit/s at
/// 100 ms; 8 MiB keeps long paths bandwidth-limited rather than window-limited.
/// Memory is only committed while the local application reads slower than
/// the peer sends.
const STREAM_WINDOW: u32 = 8 << 20;

fn transport() -> io::Result<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config
        .max_concurrent_bidi_streams(1u32.into())
        .max_concurrent_uni_streams(0u32.into())
        .stream_receive_window(STREAM_WINDOW.into())
        .send_window(u64::from(STREAM_WINDOW));
    config.keep_alive_interval(Some(KEEP_ALIVE));
    config.max_idle_timeout(Some(QUIC_IDLE.try_into().map_err(io::Error::other)?));
    config.datagram_receive_buffer_size(None);
    Ok(config)
}
#[derive(Default)]
pub(super) struct Permissions(Mutex<HashMap<Vec<u8>, tokio::time::Instant>>);
impl std::fmt::Debug for Permissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthorizedPeerCertificates")
    }
}
impl Permissions {
    pub fn allow(&self, cert: Vec<u8>, expires: tokio::time::Instant) -> io::Result<()> {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, until| *until > tokio::time::Instant::now());
        if map.len() >= MAX_PERMITTED && !map.contains_key(&cert) {
            return Err(io::Error::other("peer certificate capacity reached"));
        }
        map.entry(cert)
            .and_modify(|old| *old = (*old).max(expires))
            .or_insert(expires);
        Ok(())
    }
}
impl ClientCertVerifier for Permissions {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        let allowed = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(end_entity.as_ref())
            .is_some_and(|until| *until > tokio::time::Instant::now());
        if !allowed || !intermediates.is_empty() {
            return Err(rustls::Error::General(
                "certificate has no active session grant".into(),
            ));
        }
        let mut roots = rustls::RootCertStore::empty();
        roots.add(end_entity.clone().into_owned())?;
        rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            PROVIDER.clone(),
        )
        .build()
        .map_err(|e| rustls::Error::General(e.to_string()))?
        .verify_client_cert(end_entity, intermediates, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &PROVIDER.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &PROVIDER.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        PROVIDER
            .signature_verification_algorithms
            .supported_schemes()
    }
}
