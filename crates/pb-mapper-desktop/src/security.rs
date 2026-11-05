use crate::{Result, error};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use quinn::rustls::{
    self,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub(crate) const NAME: &str = "desktop.pb-mapper.invalid";
const ALPN: &[u8] = b"pb-mapper/desktop/1";

pub(crate) fn secret() -> Result<String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0; 32];
    SystemRandom::new().fill(&mut bytes).map_err(error)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Durable endpoint identity and bearer grant. The embedding application owns
/// storage and revocation. Anyone holding its invitation can view the desktop;
/// input still requires a separately enabled host control policy.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostIdentity {
    certificate: String,
    private_key: String,
    token: String,
}
impl std::fmt::Debug for HostIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostIdentity([redacted])")
    }
}
impl HostIdentity {
    pub fn generate() -> Result<Self> {
        let (rcgen::CertifiedKey { cert, .. }, key) =
            rcgen::generate_simple_self_signed(vec![NAME.into()]).map_err(error)?;
        Ok(Self {
            certificate: URL_SAFE_NO_PAD.encode(cert.der()),
            private_key: URL_SAFE_NO_PAD.encode(key.secret_der()),
            token: secret()?,
        })
    }
    /// Contains the private key and bearer capability. Store only in an OS
    /// credential vault or equivalent encrypted application-owned storage.
    pub fn encode_secret(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(error)
    }
    pub fn decode_secret(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 16384 {
            return Err(error("desktop identity exceeds the storage limit"));
        }
        let identity: Self =
            serde_json::from_slice(bytes).map_err(|_| error("invalid stored desktop identity"))?;
        identity.acceptor()?;
        Ok(identity)
    }
    pub(crate) fn invitation(&self, service: String) -> crate::Invitation {
        crate::Invitation {
            version: 1,
            service,
            certificate: self.certificate.clone(),
            token: self.token.clone(),
        }
    }
    pub(crate) fn acceptor(&self) -> Result<tokio_rustls::TlsAcceptor> {
        if self.certificate.len() > 4096
            || self.private_key.len() > 4096
            || URL_SAFE_NO_PAD
                .decode(&self.token)
                .map_err(|_| error("invalid stored desktop grant"))?
                .len()
                != 32
        {
            return Err(error("invalid stored desktop identity"));
        }
        let certificate = CertificateDer::from(
            URL_SAFE_NO_PAD
                .decode(&self.certificate)
                .map_err(|_| error("invalid stored desktop certificate"))?,
        );
        let key = rustls::pki_types::PrivateKeyDer::try_from(
            URL_SAFE_NO_PAD
                .decode(&self.private_key)
                .map_err(|_| error("invalid stored desktop key"))?,
        )
        .map_err(error)?;
        let mut config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(error)?
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .map_err(error)?;
        config.alpn_protocols = vec![ALPN.to_vec()];
        config.max_early_data_size = 0;
        Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
    }
}
#[cfg(test)]
pub(crate) fn server() -> Result<(tokio_rustls::TlsAcceptor, String)> {
    let identity = HostIdentity::generate()?;
    Ok((identity.acceptor()?, identity.certificate))
}

pub(crate) fn client(certificate: &str) -> Result<tokio_rustls::TlsConnector> {
    let certificate = URL_SAFE_NO_PAD
        .decode(certificate)
        .map_err(|_| error("invalid certificate"))?;
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(certificate.clone()))
        .map_err(error)?;
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        rustls::crypto::ring::default_provider().into(),
    )
    .build()
    .map_err(error)?;
    let mut config = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(error)?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(Pinned {
        certificate,
        verifier,
    }))
    .with_no_client_auth();
    config.alpn_protocols = vec![ALPN.to_vec()];
    config.enable_early_data = false;
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

#[derive(Debug)]
struct Pinned {
    certificate: Vec<u8>,
    verifier: Arc<rustls::client::WebPkiServerVerifier>,
}
impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if cert.as_ref() != self.certificate || !chain.is_empty() {
            return Err(rustls::Error::General("desktop identity changed".into()));
        }
        self.verifier
            .verify_server_cert(cert, chain, name, ocsp, now)
    }
    fn verify_tls12_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.verifier.verify_tls12_signature(msg, cert, sig)
    }
    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.verifier.verify_tls13_signature(msg, cert, sig)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.verifier.supported_verify_schemes()
    }
}
