use crate::{Invitation, Result, error};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use quinn::rustls::{
    self,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::WebPkiSupportedAlgorithms,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use serde::{Deserialize, Serialize};
use std::{io, sync::Arc};
use subtle::ConstantTimeEq;

pub(crate) const NAME: &str = "desktop.pb-mapper.invalid";
/// Each side offers only this version, so a peer of another version fails the
/// TLS handshake cleanly instead of mid-session.
pub(crate) const ALPN: &[u8] = b"pb-mapper/desktop/2";

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
            rcgen::generate_simple_self_signed(vec![NAME.into()]).map_err(io::Error::other)?;
        Ok(Self {
            certificate: URL_SAFE_NO_PAD.encode(cert.der()),
            private_key: URL_SAFE_NO_PAD.encode(key.secret_der()),
            token: secret()?,
        })
    }
    /// Contains the private key and bearer capability. Store only in an OS
    /// credential vault or equivalent encrypted application-owned storage.
    pub fn encode_secret(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(io::Error::other)
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
    /// The `pbd1` invitation to this identity's `service`: its certificate
    /// fingerprint and its token. Stored trust therefore re-encodes on load,
    /// with no migration.
    pub fn invitation(&self, service: String) -> Result<Invitation> {
        let certificate = URL_SAFE_NO_PAD
            .decode(&self.certificate)
            .map_err(|_| error("invalid stored desktop certificate"))?;
        let token = URL_SAFE_NO_PAD
            .decode(&self.token)
            .ok()
            .and_then(|token| <[u8; 32]>::try_from(token).ok())
            .ok_or_else(|| error("invalid stored desktop grant"))?;
        Invitation::new(service, &certificate, token).map_err(io::Error::other)
    }
    pub(crate) fn acceptor(&self) -> Result<tokio_rustls::TlsAcceptor> {
        Ok(tokio_rustls::TlsAcceptor::from(Arc::new(
            self.server_config(vec![])?,
        )))
    }
    /// `extra` certificates follow the host's own in its chain (tests only).
    fn server_config(&self, extra: Vec<CertificateDer<'static>>) -> Result<rustls::ServerConfig> {
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
        let mut chain = vec![certificate];
        chain.extend(extra);
        let mut config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::ring::default_provider().into(),
        )
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(io::Error::other)?;
        config.alpn_protocols = vec![ALPN.to_vec()];
        config.max_early_data_size = 0;
        Ok(config)
    }
}

/// A viewer pinned to `fingerprint`, the SHA-256 of the host certificate DER
/// carried by the invitation.
pub(crate) fn client(fingerprint: [u8; 32]) -> Result<tokio_rustls::TlsConnector> {
    Ok(tokio_rustls::TlsConnector::from(Arc::new(client_config(
        fingerprint,
    )?)))
}

fn client_config(fingerprint: [u8; 32]) -> Result<rustls::ClientConfig> {
    let provider = rustls::crypto::ring::default_provider();
    let algorithms = provider.signature_verification_algorithms;
    let mut config = rustls::ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint,
            algorithms,
        }))
        .with_no_client_auth();
    config.alpn_protocols = vec![ALPN.to_vec()];
    config.enable_early_data = false;
    Ok(config)
}

/// The host presented a certificate other than the pinned one, or a chain.
#[derive(Debug)]
pub(crate) struct IdentityChanged;
impl std::fmt::Display for IdentityChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("desktop identity changed")
    }
}
impl std::error::Error for IdentityChanged {}

/// The TLS handshake failures a viewer reports as their own error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandshakeFailure {
    /// The certificate is not the invitation's: revoked trust or an impostor.
    IdentityChanged,
    /// The peer speaks another desktop protocol version.
    VersionMismatch,
}

/// Classify a failed TLS connect or accept (tokio-rustls wraps the rustls error).
pub(crate) fn handshake_failure(failure: &io::Error) -> Option<HandshakeFailure> {
    use rustls::{AlertDescription, CertificateError, Error};
    match failure.get_ref()?.downcast_ref::<Error>()? {
        Error::InvalidCertificate(CertificateError::Other(other))
            if other.0.downcast_ref::<IdentityChanged>().is_some() =>
        {
            Some(HandshakeFailure::IdentityChanged)
        }
        Error::AlertReceived(AlertDescription::NoApplicationProtocol)
        | Error::NoApplicationProtocol
        | Error::PeerIncompatible(_) => Some(HandshakeFailure::VersionMismatch),
        _ => None,
    }
}

/// Pins the host by certificate fingerprint. The self-signed certificate has no
/// chain to validate: the pin is the trust, and the handshake signature proves
/// the host holds its key.
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}
impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let presented = ring::digest::digest(&ring::digest::SHA256, cert.as_ref());
        if !intermediates.is_empty() || !bool::from(presented.as_ref().ct_eq(&self.fingerprint)) {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::Other(rustls::OtherError(Arc::new(IdentityChanged))),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _msg: &[u8],
        _cert: &CertificateDer<'_>,
        _sig: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not supported".into()))
    }
    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(msg, cert, sig, &self.algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
pub(crate) fn server() -> Result<(tokio_rustls::TlsAcceptor, [u8; 32])> {
    let identity = HostIdentity::generate()?;
    let fingerprint = identity.invitation("desktop:test".into())?.fingerprint;
    Ok((identity.acceptor()?, fingerprint))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// Run one handshake; return the viewer's error, or `None` if it connected.
    async fn connect(
        client: rustls::ClientConfig,
        server: rustls::ServerConfig,
    ) -> Option<io::Error> {
        let (viewer, host) = tokio::io::duplex(16 * 1024);
        let accept = async move {
            tokio_rustls::TlsAcceptor::from(Arc::new(server))
                .accept(host)
                .await
        };
        let connect = async move {
            tokio_rustls::TlsConnector::from(Arc::new(client))
                .connect(ServerName::try_from(NAME).unwrap(), viewer)
                .await
        };
        let (connected, _): (io::Result<tokio_rustls::client::TlsStream<DuplexStream>>, _) =
            tokio::join!(connect, accept);
        connected.err()
    }

    /// A viewer and host of different desktop protocol versions: the old side
    /// offers the PR #51 ALPN. Returns the viewer's handshake error.
    pub(crate) async fn alpn_handshake(old_host: bool) -> io::Error {
        let identity = HostIdentity::generate().unwrap();
        let fingerprint = identity
            .invitation("desktop:test".into())
            .unwrap()
            .fingerprint;
        let mut client = client_config(fingerprint).unwrap();
        let mut server = identity.server_config(vec![]).unwrap();
        let old = b"pb-mapper/desktop/1".to_vec();
        if old_host {
            server.alpn_protocols = vec![old];
        } else {
            client.alpn_protocols = vec![old];
        }
        connect(client, server).await.unwrap()
    }

    #[tokio::test]
    async fn fingerprint_pin_rejects_another_certificate() {
        let host = HostIdentity::generate().unwrap();
        let pin = host.invitation("desktop:test".into()).unwrap().fingerprint;
        assert!(
            connect(
                client_config(pin).unwrap(),
                host.server_config(vec![]).unwrap()
            )
            .await
            .is_none()
        );

        let impostor = HostIdentity::generate().unwrap();
        let failure = connect(
            client_config(pin).unwrap(),
            impostor.server_config(vec![]).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            handshake_failure(&failure),
            Some(HandshakeFailure::IdentityChanged)
        );

        // The pinned certificate followed by a chain is still refused.
        let other = CertificateDer::from(URL_SAFE_NO_PAD.decode(&impostor.certificate).unwrap());
        let failure = connect(
            client_config(pin).unwrap(),
            host.server_config(vec![other]).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            handshake_failure(&failure),
            Some(HandshakeFailure::IdentityChanged)
        );
    }
}
