use crate::{Result, error};
use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// Protocol v2 records are `u32 BE len | bytes`: a JSON header of at most
/// `MAX_HEADER`, and after `HostMessage::Video` only, one payload record of
/// 1..=`MAX_PAYLOAD` bytes.
pub(crate) const MAX_HEADER: usize = 8192;
pub(crate) const MAX_PAYLOAD: usize = 4 * 1024 * 1024;
/// Writes and payload reads get `IO_TIMEOUT` plus the time this rate needs, so
/// a large IDR on a slow cellular link is not mistaken for a dead peer.
pub(crate) const WRITE_FLOOR_BPS: u64 = 1_000_000;

pub(crate) fn write_timeout(bytes: usize) -> Duration {
    IO_TIMEOUT + Duration::from_secs_f64(bytes as f64 * 8.0 / WRITE_FLOOR_BPS as f64)
}

fn timed_out() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "desktop transport timed out")
}

/// One message: the header record and, for video only, its payload record,
/// buffered together and flushed once, all within `write_timeout`.
pub(crate) async fn write_message<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    header: &T,
    payload: Option<&[u8]>,
) -> Result<()> {
    let header = serde_json::to_vec(header).map_err(std::io::Error::other)?;
    if header.len() > MAX_HEADER {
        return Err(error("desktop message too large"));
    }
    let payload_len = payload.map_or(0, <[u8]>::len);
    if payload.is_some() && !(1..=MAX_PAYLOAD).contains(&payload_len) {
        return Err(error("desktop payload size out of bounds"));
    }
    let records = 4 + header.len() + payload.map_or(0, |payload| 4 + payload.len());
    tokio::time::timeout(write_timeout(header.len() + payload_len), async {
        let mut out = tokio::io::BufWriter::with_capacity(records, &mut *w);
        out.write_u32(header.len() as u32).await?;
        out.write_all(&header).await?;
        if let Some(payload) = payload {
            out.write_u32(payload.len() as u32).await?;
            out.write_all(payload).await?;
        }
        out.flush().await
    })
    .await
    .map_err(|_| timed_out())?
}

/// The next header, waiting at most `timeout` for it. `None` is a clean EOF
/// between messages; EOF inside a record is an error.
pub(crate) async fn read_header<R: AsyncRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
    timeout: Duration,
) -> Result<Option<T>> {
    tokio::time::timeout(timeout, async {
        let mut length = [0; 4];
        if r.read(&mut length[..1]).await? == 0 {
            return Ok(None);
        }
        r.read_exact(&mut length[1..]).await?;
        let len = u32::from_be_bytes(length) as usize;
        if len > MAX_HEADER {
            return Err(error("desktop message too large"));
        }
        let mut bytes = vec![0; len];
        r.read_exact(&mut bytes).await?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| error("invalid desktop message"))
    })
    .await
    .map_err(|_| timed_out())?
}

/// The payload record that follows a `Video` header, read without a copy.
pub(crate) async fn read_payload<R: AsyncRead + Unpin>(r: &mut R) -> Result<bytes::Bytes> {
    let len = tokio::time::timeout(IO_TIMEOUT, r.read_u32())
        .await
        .map_err(|_| timed_out())?? as usize;
    if !(1..=MAX_PAYLOAD).contains(&len) {
        return Err(error("desktop payload size out of bounds"));
    }
    tokio::time::timeout(write_timeout(len), async {
        let mut bytes = Vec::with_capacity(len);
        (&mut *r).take(len as u64).read_to_end(&mut bytes).await?;
        if bytes.len() != len {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        Ok(bytes::Bytes::from(bytes))
    })
    .await
    .map_err(|_| timed_out())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{HostMessage, ViewerMessage};

    async fn tls_pair() -> (
        tokio_rustls::client::TlsStream<tokio::io::DuplexStream>,
        tokio_rustls::server::TlsStream<tokio::io::DuplexStream>,
    ) {
        let (acceptor, fingerprint) = crate::security::server().unwrap();
        let connector = crate::security::client(fingerprint).unwrap();
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (client, server) = tokio::join!(
            connector.connect(
                quinn::rustls::pki_types::ServerName::try_from(crate::security::NAME).unwrap(),
                client,
            ),
            acceptor.accept(server),
        );
        (client.unwrap(), server.unwrap())
    }

    async fn next(
        server: &mut tokio_rustls::server::TlsStream<tokio::io::DuplexStream>,
    ) -> Result<Option<ViewerMessage>> {
        read_header(server, IO_TIMEOUT).await
    }

    #[tokio::test]
    async fn clean_tls_close_is_eof_only_between_messages() {
        let (mut client, mut server) = tls_pair().await;
        write_message(&mut client, &ViewerMessage::Ping {}, None)
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        assert!(matches!(
            next(&mut server).await.unwrap(),
            Some(ViewerMessage::Ping {})
        ));
        assert!(next(&mut server).await.unwrap().is_none());

        // A header and its payload travel as one message; a payload is never empty.
        let (mut client, mut server) = tls_pair().await;
        let header = HostMessage::Probe { nonce: 1 };
        write_message(&mut server, &header, Some(&[7; 3000]))
            .await
            .unwrap();
        assert!(matches!(
            read_header(&mut client, IO_TIMEOUT).await.unwrap(),
            Some(HostMessage::Probe { nonce: 1 })
        ));
        assert_eq!(&read_payload(&mut client).await.unwrap()[..], &[7; 3000]);
        assert!(
            write_message(&mut server, &header, Some(&[]))
                .await
                .is_err()
        );

        let (mut client, mut server) = tls_pair().await;
        client.write_all(&[0, 0]).await.unwrap();
        client.shutdown().await.unwrap();
        assert_eq!(
            next(&mut server).await.err().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof,
        );

        let (client, mut server) = tls_pair().await;
        drop(client);
        assert_eq!(
            next(&mut server).await.err().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof,
        );
    }
}
