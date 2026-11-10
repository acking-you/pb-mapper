use crate::error;
use serde::{Serialize, de::DeserializeOwned};
use std::{fmt, io, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// Why a message could not be read or written. The host and the viewer tell
/// a peer that broke the framing from a transport that failed by the
/// variant; `io::Error::from` gives the error each one always was.
#[derive(Debug)]
pub(crate) enum WireError {
    /// A record breaks the framing: a header too large or not a message,
    /// or a payload size out of bounds.
    Framing(&'static str),
    /// The record did not arrive, or leave, within its time.
    TimedOut,
    /// The transport failed, or the message could not be encoded.
    Io(io::Error),
}
impl From<io::Error> for WireError {
    fn from(failure: io::Error) -> Self {
        Self::Io(failure)
    }
}
impl From<WireError> for io::Error {
    fn from(failure: WireError) -> Self {
        match failure {
            WireError::Framing(framing) => error(framing),
            WireError::TimedOut => timed_out(),
            WireError::Io(failure) => failure,
        }
    }
}
/// The text of the error `io::Error::from` gives.
impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Framing(framing) => f.write_str(framing),
            Self::TimedOut => f.write_str(TIMED_OUT),
            Self::Io(failure) => failure.fmt(f),
        }
    }
}

/// Protocol v2 records are `u32 BE len | bytes`: a JSON header of at most
/// `MAX_HEADER`, and after `HostMessage::Video` only, one payload record of
/// 1..=`MAX_PAYLOAD` bytes.
const MAX_HEADER: usize = 8192;
pub(crate) const MAX_PAYLOAD: usize = 4 * 1024 * 1024;
/// Writes and payload reads get `IO_TIMEOUT` plus the time this rate needs, so
/// a large IDR on a slow cellular link is not mistaken for a dead peer.
const WRITE_FLOOR_BPS: u64 = 1_000_000;

fn write_timeout(bytes: usize) -> Duration {
    IO_TIMEOUT + Duration::from_secs_f64(bytes as f64 * 8.0 / WRITE_FLOOR_BPS as f64)
}

const TIMED_OUT: &str = "desktop transport timed out";
const TOO_LARGE: &str = "desktop message too large";
const PAYLOAD_SIZE: &str = "desktop payload size out of bounds";

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, TIMED_OUT)
}

/// One message: `header` serialized, then written with `payload` as
/// `write_records` writes them.
pub(crate) async fn write_message<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    header: &T,
    payload: Option<&[u8]>,
) -> Result<(), WireError> {
    let header = serde_json::to_vec(header).map_err(io::Error::other)?;
    write_records(w, &header, payload).await
}

/// The header record and, for video only, its payload record, buffered
/// together and flushed once, all within `write_timeout`.
async fn write_records<W: AsyncWrite + Unpin>(
    w: &mut W,
    header: &[u8],
    payload: Option<&[u8]>,
) -> Result<(), WireError> {
    if header.len() > MAX_HEADER {
        return Err(WireError::Framing(TOO_LARGE));
    }
    let payload_len = payload.map_or(0, <[u8]>::len);
    if payload.is_some() && !(1..=MAX_PAYLOAD).contains(&payload_len) {
        return Err(WireError::Framing(PAYLOAD_SIZE));
    }
    let records = 4 + header.len() + payload.map_or(0, |payload| 4 + payload.len());
    tokio::time::timeout(write_timeout(header.len() + payload_len), async {
        let mut out = tokio::io::BufWriter::with_capacity(records, &mut *w);
        out.write_u32(header.len() as u32).await?;
        out.write_all(header).await?;
        if let Some(payload) = payload {
            out.write_u32(payload.len() as u32).await?;
            out.write_all(payload).await?;
        }
        out.flush().await
    })
    .await
    .map_err(|_| WireError::TimedOut)?
    .map_err(WireError::Io)
}

/// The next header, waiting at most `timeout` for it. `None` is a clean EOF
/// between messages; EOF inside a record is an error.
pub(crate) async fn read_header<R: AsyncRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
    timeout: Duration,
) -> Result<Option<T>, WireError> {
    let Some(bytes) = read_record(r, timeout).await? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| WireError::Framing("invalid desktop message"))
}

/// The next header record's bytes, as `read_header` waits for them.
async fn read_record<R: AsyncRead + Unpin>(
    r: &mut R,
    timeout: Duration,
) -> Result<Option<Vec<u8>>, WireError> {
    tokio::time::timeout(timeout, async {
        let mut length = [0; 4];
        if r.read(&mut length[..1]).await? == 0 {
            return Ok(None);
        }
        r.read_exact(&mut length[1..]).await?;
        let len = u32::from_be_bytes(length) as usize;
        if len > MAX_HEADER {
            return Err(WireError::Framing(TOO_LARGE));
        }
        let mut bytes = vec![0; len];
        r.read_exact(&mut bytes).await?;
        Ok(Some(bytes))
    })
    .await
    .map_err(|_| WireError::TimedOut)?
}

/// The payload record that follows a `Video` header, read without a copy.
pub(crate) async fn read_payload<R: AsyncRead + Unpin>(
    r: &mut R,
) -> Result<bytes::Bytes, WireError> {
    let len = tokio::time::timeout(IO_TIMEOUT, r.read_u32())
        .await
        .map_err(|_| WireError::TimedOut)?? as usize;
    if !(1..=MAX_PAYLOAD).contains(&len) {
        return Err(WireError::Framing(PAYLOAD_SIZE));
    }
    tokio::time::timeout(write_timeout(len), async {
        let mut bytes = Vec::with_capacity(len);
        (&mut *r).take(len as u64).read_to_end(&mut bytes).await?;
        if bytes.len() != len {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        Ok(bytes::Bytes::from(bytes))
    })
    .await
    .map_err(|_| WireError::TimedOut)?
    .map_err(WireError::Io)
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
    ) -> Result<Option<ViewerMessage>, WireError> {
        read_header(server, IO_TIMEOUT).await
    }

    fn eof(failure: Option<WireError>) -> bool {
        matches!(failure, Some(WireError::Io(failure)) if failure.kind() == io::ErrorKind::UnexpectedEof)
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
        assert!(eof(next(&mut server).await.err()));

        let (client, mut server) = tls_pair().await;
        drop(client);
        assert!(eof(next(&mut server).await.err()));
    }

    /// A peer that breaks the framing is told apart from a silent one, and
    /// each converts to the `io::Error` it always was: `Other`, or
    /// `TimedOut`, with the same text.
    #[tokio::test(start_paused = true)]
    async fn framing_and_silence_keep_their_errors() {
        let read = async |bytes: &[u8]| {
            let (mut peer, mut reader) = tokio::io::duplex(16 * 1024);
            peer.write_all(bytes).await.unwrap();
            let failure = read_header::<_, ViewerMessage>(&mut reader, IO_TIMEOUT)
                .await
                .err()
                .unwrap();
            let framing = matches!(failure, WireError::Framing(_));
            let shown = failure.to_string();
            let converted = io::Error::from(failure);
            assert_eq!(converted.to_string(), shown);
            (framing, converted.kind())
        };
        let other = (true, io::ErrorKind::Other);
        assert_eq!(read(&(MAX_HEADER as u32 + 1).to_be_bytes()).await, other);
        assert_eq!(read(&[0, 0, 0, 2, b'{', b'}']).await, other);
        let silent = read(&[0, 0, 0, 2]).await;
        assert_eq!(silent, (false, io::ErrorKind::TimedOut));
    }
}
