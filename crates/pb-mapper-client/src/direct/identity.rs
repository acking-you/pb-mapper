//! Exact certificates arrive over the existing authenticated control channel.
//! WebPKI still checks validity/EKU and TLS still proves private-key possession.
use quinn::rustls::{
    self, DigitallySignedStruct, DistinguishedName, SignatureScheme,
    client::danger::HandshakeSignatureValid,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex},
};

pub(super) struct Identity {
    pub certificate: Vec<u8>,
    key: Vec<u8>,
    pub permissions: Arc<Permissions>,
}
impl Identity {
    pub fn new() -> io::Result<Self> {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["pb-mapper.invalid".into()])
                .map_err(io::Error::other)?;
        Ok(Self {
            certificate: cert.der().to_vec(),
            key: signing_key.serialize_der(),
            permissions: Arc::new(Permissions::default()),
        })
    }
    pub fn server(&self) -> io::Result<quinn::ServerConfig> {
        let mut tls = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .with_client_cert_verifier(self.permissions.clone())
        .with_single_cert(
            vec![CertificateDer::from(self.certificate.clone())],
            PrivatePkcs8KeyDer::from(self.key.clone()).into(),
        )
        .map_err(io::Error::other)?;
        tls.alpn_protocols = vec![b"pb-mapper/direct/1".to_vec()];
        tls.max_early_data_size = 0;
        let crypto =
            quinn::crypto::rustls::QuicServerConfig::try_from(tls).map_err(io::Error::other)?;
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        config.transport_config(Arc::new(transport()?));
        Ok(config)
    }
    pub fn client(&self, peer: &[u8]) -> io::Result<quinn::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(peer.to_vec()))
            .map_err(io::Error::other)?;
        let mut tls = rustls::ClientConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .with_root_certificates(roots)
        .with_client_auth_cert(
            vec![CertificateDer::from(self.certificate.clone())],
            PrivatePkcs8KeyDer::from(self.key.clone()).into(),
        )
        .map_err(io::Error::other)?;
        tls.alpn_protocols = vec![b"pb-mapper/direct/1".to_vec()];
        tls.enable_early_data = false;
        let mut config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(io::Error::other)?,
        ));
        config.transport_config(Arc::new(transport()?));
        Ok(config)
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
    config.keep_alive_interval(Some(std::time::Duration::from_secs(15)));
    config.max_idle_timeout(Some(
        std::time::Duration::from_secs(60)
            .try_into()
            .map_err(io::Error::other)?,
    ));
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
        if map.len() >= 1024 && !map.contains_key(&cert) {
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
            rustls::crypto::ring::default_provider().into(),
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
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
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
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
