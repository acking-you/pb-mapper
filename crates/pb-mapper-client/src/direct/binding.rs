//! TLS session binding: what ties a grant to the one TLS session that
//! claims it, on either carrier.
use pb_mapper_protocol::direct::SessionGrant;
use std::io;

/// A TLS session's keying-material exporter, and the label a session's
/// binding is exported under on its carrier: one per carrier, so that a
/// binding made on one is worthless on the other.
pub(super) trait Exporter {
    const BINDING: &'static [u8];
    fn export(&self, out: &mut [u8], label: &[u8], context: &[u8]) -> io::Result<()>;
}

impl Exporter for quinn::Connection {
    const BINDING: &'static [u8] = b"pb-mapper/direct/1";

    fn export(&self, out: &mut [u8], label: &[u8], context: &[u8]) -> io::Result<()> {
        self.export_keying_material(out, label, context)
            .map_err(|_| io::Error::other("TLS exporter unavailable"))
    }
}

pub(super) fn binding<E: Exporter>(connection: &E, grant: &SessionGrant) -> io::Result<[u8; 32]> {
    tag(connection, E::BINDING, grant)
}
/// The binding of `grant` to `connection`'s TLS session under `label`.
pub(super) fn tag(
    connection: &impl Exporter,
    label: &[u8],
    grant: &SessionGrant,
) -> io::Result<[u8; 32]> {
    let exporter = binding_exporter(connection, label, grant)?;
    let tag = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &grant.secret.0),
        &exporter,
    );
    tag.as_ref().try_into().map_err(io::Error::other)
}
pub(super) fn verify_binding<E: Exporter>(
    connection: &E,
    grant: &SessionGrant,
    tag: &[u8],
) -> io::Result<()> {
    let exporter = binding_exporter(connection, E::BINDING, grant)?;
    ring::hmac::verify(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &grant.secret.0),
        &exporter,
        tag,
    )
    .map_err(|_| io::Error::other("TLS session binding mismatch"))
}
fn binding_exporter(
    connection: &impl Exporter,
    label: &[u8],
    grant: &SessionGrant,
) -> io::Result<[u8; 32]> {
    let context = binding_context(grant)?;
    let mut exporter = [0; 32];
    connection.export(&mut exporter, label, &context)?;
    Ok(exporter)
}
/// What a binding is exported with: the grant's identity, as JSON, with the
/// certificates as digests. Every peer computes the same bytes, so they never
/// change.
fn binding_context(grant: &SessionGrant) -> io::Result<Vec<u8>> {
    use ring::digest;
    serde_json::to_vec(&(
        grant.id,
        grant.server_instance,
        grant.generation,
        grant.namespace,
        &grant.service,
        grant.is_datagram,
        grant.consumer_key,
        grant.provider_key,
        digest::digest(&digest::SHA256, &grant.consumer.certificate).as_ref(),
        digest::digest(&digest::SHA256, &grant.provider.certificate).as_ref(),
    ))
    .map_err(io::Error::other)
}
/// The key UDP services frame their datagrams with: each TLS session
/// exports its own, whatever the carrier.
pub(super) fn framing_key(
    connection: &impl Exporter,
    grant: &SessionGrant,
) -> io::Result<[u8; 32]> {
    let mut key = [0; 32];
    connection.export(&mut key, b"pb-mapper/framing/1", &grant.id)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_protocol::direct::{PeerInfo, SessionSecret};
    use std::sync::Mutex;

    /// An exporter that keeps the context it was asked to export with.
    #[derive(Default)]
    struct Context(Mutex<Vec<u8>>);

    impl Exporter for Context {
        const BINDING: &'static [u8] = b"test";

        fn export(&self, _: &mut [u8], _: &[u8], context: &[u8]) -> io::Result<()> {
            *self.0.lock().unwrap() = context.to_vec();
            Ok(())
        }
    }

    /// Older peers bind a session to its TLS session with exactly this
    /// context: the grant's identity as JSON, the certificates as digests.
    #[test]
    fn the_binding_context_keeps_its_bytes() {
        let peer = |byte| PeerInfo {
            certificate: vec![byte; 4],
            candidates: Vec::new(),
            lan_tcp: None,
        };
        let grant = SessionGrant {
            id: [1; 16],
            server_instance: [2; 16],
            generation: 3,
            namespace: 4,
            service: "svc".into(),
            is_datagram: true,
            consumer_key: 5,
            provider_key: 6,
            consumer: peer(7),
            provider: peer(8),
            secret: SessionSecret([9; 32]),
            ttl_ms: 10,
        };
        let exporter = Context::default();
        binding(&exporter, &grant).unwrap();
        let context = String::from_utf8(exporter.0.into_inner().unwrap()).unwrap();
        assert_eq!(
            context,
            "[[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],3,4,\"svc\",true,5,6,\
             [180,81,216,167,204,109,239,222,156,176,90,11,220,11,19,233,\
             192,24,96,71,157,134,252,231,219,77,9,62,7,147,179,73],\
             [145,139,208,39,245,144,135,190,248,224,85,249,181,135,178,84,\
             134,213,140,96,109,134,88,212,206,123,17,153,39,79,103,68]]"
        );
    }
}
