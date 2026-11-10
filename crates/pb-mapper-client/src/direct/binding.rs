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
    use ring::digest;
    let context = serde_json::to_vec(&(
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
    .map_err(io::Error::other)?;
    let mut exporter = [0; 32];
    connection.export(&mut exporter, label, &context)?;
    Ok(exporter)
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
