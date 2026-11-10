//! A committed session's connection, on whichever carrier won, and the
//! forwarding over it that both ends run.
use super::{
    binding,
    tcp::{self, TlsTcp},
};
use crate::diagnostics::LinkProbe;
use pb_mapper_protocol::{
    direct::{DirectTransport, SessionGrant},
    forward::{ByteMeter, StreamForward},
};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uni_stream::stream::NetworkStream;

/// How long a TCP session's end waits for the peer's, after its own: the
/// TCP twin of waiting for QUIC to acknowledge the last bytes.
const TCP_DRAIN: Duration = Duration::from_secs(3);

pub(super) struct QuicGuard(pub quinn::Connection);
impl Drop for QuicGuard {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"session finished");
    }
}

/// A direct session's connection on the carrier that won: one QUIC
/// connection's single stream, or one TLS stream over TCP. The commit bytes
/// and the binding are the same on both; forwarding picks its loop once.
pub(super) enum DirectConn {
    Quic {
        connection: QuicGuard,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
    },
    Tcp {
        stream: Box<TlsTcp>,
        /// The handshake's round trip, which stands in for TCP's figures.
        rtt: Duration,
    },
}

impl DirectConn {
    pub(super) fn transport(&self) -> DirectTransport {
        match self {
            Self::Quic { .. } => DirectTransport::Quic,
            Self::Tcp { .. } => DirectTransport::Tcp,
        }
    }
    pub(super) fn remote_address(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Quic { connection, .. } => Ok(connection.0.remote_address()),
            Self::Tcp { stream, .. } => tcp::addresses(stream).0,
        }
    }
    pub(super) fn local_ip(&self) -> Option<IpAddr> {
        match self {
            Self::Quic { connection, .. } => connection.0.local_ip(),
            Self::Tcp { stream, .. } => tcp::addresses(stream).1,
        }
    }
    pub(super) fn probe(&self) -> LinkProbe {
        match self {
            Self::Quic { connection, .. } => LinkProbe::Quic(connection.0.clone()),
            Self::Tcp { rtt, .. } => LinkProbe::Tcp {
                handshake_rtt: *rtt,
            },
        }
    }
    /// The certificate the other end presented, when it presented one.
    pub(super) fn peer_certificate(&self) -> Option<Vec<u8>> {
        match self {
            Self::Quic { connection, .. } => {
                let certs = connection
                    .0
                    .peer_identity()?
                    .downcast::<Vec<quinn::rustls::pki_types::CertificateDer<'static>>>()
                    .ok()?;
                match certs.as_slice() {
                    [certificate] => Some(certificate.to_vec()),
                    _ => None,
                }
            }
            Self::Tcp { stream, .. } => tcp::peer_certificate(stream).map(<[u8]>::to_vec),
        }
    }
    pub(super) fn binding(&self, grant: &SessionGrant) -> io::Result<[u8; 32]> {
        match self {
            Self::Quic { connection, .. } => binding::binding(&connection.0, grant),
            Self::Tcp { stream, .. } => binding::binding(&**stream, grant),
        }
    }
    pub(super) fn verify_binding(&self, grant: &SessionGrant, tag: &[u8]) -> io::Result<()> {
        match self {
            Self::Quic { connection, .. } => binding::verify_binding(&connection.0, grant, tag),
            Self::Tcp { stream, .. } => binding::verify_binding(&**stream, grant, tag),
        }
    }
    pub(super) fn framing_key(&self, grant: &SessionGrant) -> io::Result<[u8; 32]> {
        match self {
            Self::Quic { connection, .. } => binding::framing_key(&connection.0, grant),
            Self::Tcp { stream, .. } => binding::framing_key(&**stream, grant),
        }
    }
    /// Setup bytes, sent at once: over TLS, flushed rather than left with
    /// its records.
    pub(super) async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Quic { send, .. } => send.write_all(bytes).await.map_err(io::Error::other),
            Self::Tcp { stream, .. } => {
                stream.write_all(bytes).await?;
                stream.flush().await
            }
        }
    }
    pub(super) async fn read_u8(&mut self) -> io::Result<u8> {
        match self {
            Self::Quic { recv, .. } => recv.read_u8().await,
            Self::Tcp { stream, .. } => stream.read_u8().await,
        }
    }
    pub(super) async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        match self {
            Self::Quic { recv, .. } => recv.read_exact(buf).await.map_err(io::Error::other),
            Self::Tcp { stream, .. } => stream.read_exact(buf).await.map(|_| ()),
        }
    }
}

/// Forward a committed direct session between `local` and the peer, on the
/// carrier its connection runs over.
pub(super) async fn forward_direct<L: NetworkStream + StreamForward>(
    mut local: L,
    conn: DirectConn,
    framing: [u8; 32],
    idle: Option<Duration>,
    meter: Option<Arc<ByteMeter>>,
) -> io::Result<()> {
    let (read, write) = local.split();
    match conn {
        DirectConn::Quic {
            connection: _connection,
            mut send,
            recv,
        } => {
            let forward = L::forward_local_to_remote_metered(
                None, framing, read, write, recv, &mut send, meter,
            );
            idle_bounded(idle, forward).await?;
            // FIN queues bytes but does not acknowledge delivery. Keep the
            // connection alive until Quinn confirms the final response; local
            // stop still cancels us.
            let _ = send.finish();
            let _ = send.stopped().await;
        }
        DirectConn::Tcp { stream, .. } => {
            let (mut remote_read, mut remote_write) = tcp::split(*stream);
            let forward = L::forward_local_to_remote_metered(
                None,
                framing,
                read,
                write,
                &mut remote_read,
                &mut remote_write,
                meter,
            );
            idle_bounded(idle, forward).await?;
            // The TCP twin of the above: close_notify and FIN after the last
            // byte, then the peer's end read, so that nothing left unread
            // here turns this end's close into a reset that loses what it
            // sent. Bounded, as an unreachable peer must not hold a stop.
            let _ = tokio::time::timeout(TCP_DRAIN, async {
                let _ = remote_write.shutdown().await;
                let _ = tokio::io::copy(&mut remote_read, &mut tokio::io::sink()).await;
            })
            .await;
        }
    }
    Ok(())
}

async fn idle_bounded<T>(
    idle: Option<Duration>,
    forward: impl std::future::Future<Output = pb_mapper_core::error::Result<T>>,
) -> io::Result<()> {
    match idle {
        Some(idle) => pb_mapper_protocol::forward::with_tunnel_idle_timeout(idle, forward)
            .await
            .map(|_| ())
            .map_err(io::Error::other),
        None => forward.await.map(|_| ()).map_err(io::Error::other),
    }
}
