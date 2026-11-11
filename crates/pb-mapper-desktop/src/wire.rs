use crate::error;
use bytes::Buf;
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
/// `MAX_HEADER`, and after `HostMessage::Video` and `HostMessage::CursorShape`
/// only, one payload record of 1..=`MAX_PAYLOAD` bytes.
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

/// One message, as `write_message_with` writes it.
pub(crate) async fn write_message<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    header: &T,
    payload: Option<&[u8]>,
) -> Result<(), WireError> {
    write_message_with(w, &mut Vec::new(), header, payload).await
}

/// One message: the header record and, for video and shapes, its payload record,
/// written as one write of their bytes would write them and flushed once, all
/// within `write_timeout`. `records` holds everything but the payload; a
/// writer that keeps it across messages allocates nothing for them, and the
/// payload is not copied.
pub(crate) async fn write_message_with<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    records: &mut Vec<u8>,
    header: &T,
    payload: Option<&[u8]>,
) -> Result<(), WireError> {
    records.clear();
    records.extend_from_slice(&[0; 4]);
    serde_json::to_writer(&mut *records, header).map_err(io::Error::other)?;
    let header_len = records.len() - 4;
    if header_len > MAX_HEADER {
        return Err(WireError::Framing(TOO_LARGE));
    }
    let payload_len = payload.map_or(0, <[u8]>::len);
    if payload.is_some() && !(1..=MAX_PAYLOAD).contains(&payload_len) {
        return Err(WireError::Framing(PAYLOAD_SIZE));
    }
    records[..4].copy_from_slice(&(header_len as u32).to_be_bytes());
    if payload.is_some() {
        records.extend_from_slice(&(payload_len as u32).to_be_bytes());
    }
    tokio::time::timeout(write_timeout(header_len + payload_len), async {
        // A vectored TLS write cuts the records of both slices as it would
        // cut one slice of their bytes.
        let mut bytes = Buf::chain(records.as_slice(), payload.unwrap_or_default());
        w.write_all_buf(&mut bytes).await?;
        w.flush().await
    })
    .await
    .map_err(|_| WireError::TimedOut)?
    .map_err(WireError::Io)
}

/// The next header, as `read_header_with` reads it.
pub(crate) async fn read_header<R: AsyncRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
    timeout: Duration,
) -> Result<Option<T>, WireError> {
    read_header_with(r, &mut Vec::new(), timeout).await
}

/// The next header, waiting at most `timeout` for it. `None` is a clean EOF
/// between messages; EOF inside a record is an error. Its record is read into
/// `record`, which a reader that keeps it reuses.
pub(crate) async fn read_header_with<R: AsyncRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
    record: &mut Vec<u8>,
    timeout: Duration,
) -> Result<Option<T>, WireError> {
    if !read_record(r, record, timeout).await? {
        return Ok(None);
    }
    serde_json::from_slice(record)
        .map(Some)
        .map_err(|_| WireError::Framing("invalid desktop message"))
}

/// The next header record's bytes into `record`, as `read_header_with` waits
/// for them; false at a clean EOF.
async fn read_record<R: AsyncRead + Unpin>(
    r: &mut R,
    record: &mut Vec<u8>,
    timeout: Duration,
) -> Result<bool, WireError> {
    tokio::time::timeout(timeout, async {
        let mut length = [0; 4];
        if r.read(&mut length[..1]).await? == 0 {
            return Ok(false);
        }
        r.read_exact(&mut length[1..]).await?;
        let len = u32::from_be_bytes(length) as usize;
        if len > MAX_HEADER {
            return Err(WireError::Framing(TOO_LARGE));
        }
        record.clear();
        record.resize(len, 0);
        r.read_exact(record).await?;
        Ok(true)
    })
    .await
    .map_err(|_| WireError::TimedOut)?
}

/// The payload record that follows a `Video` header, read without a copy.
pub(crate) async fn read_payload<R: AsyncRead + Unpin>(
    r: &mut R,
) -> Result<bytes::Bytes, WireError> {
    read_payload_within(r, 1..=MAX_PAYLOAD).await
}

/// The payload record of a header that fixed its length, `len`: any other
/// length breaks the framing, before a byte of it is read.
pub(crate) async fn read_payload_of<R: AsyncRead + Unpin>(
    r: &mut R,
    len: usize,
) -> Result<bytes::Bytes, WireError> {
    read_payload_within(r, len.max(1)..=len.min(MAX_PAYLOAD)).await
}

async fn read_payload_within<R: AsyncRead + Unpin>(
    r: &mut R,
    bounds: std::ops::RangeInclusive<usize>,
) -> Result<bytes::Bytes, WireError> {
    let len = tokio::time::timeout(IO_TIMEOUT, r.read_u32())
        .await
        .map_err(|_| WireError::TimedOut)?? as usize;
    if !bounds.contains(&len) {
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

    /// What a TLS stream wrote to its transport.
    struct Tap {
        io: tokio::io::DuplexStream,
        written: Vec<u8>,
    }
    impl tokio::io::AsyncRead for Tap {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.io).poll_read(cx, buf)
        }
    }
    impl AsyncWrite for Tap {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            let this = &mut *self;
            let written = std::task::ready!(std::pin::Pin::new(&mut this.io).poll_write(cx, buf))?;
            this.written.extend_from_slice(&buf[..written]);
            std::task::Poll::Ready(Ok(written))
        }
        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.io).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
        }
    }

    /// The length of each TLS record the host wrote while `write` ran, and
    /// the bytes its viewer read.
    async fn records(
        write: impl AsyncFnOnce(&mut tokio_rustls::server::TlsStream<Tap>),
        expected: usize,
    ) -> (Vec<usize>, Vec<u8>) {
        let (acceptor, fingerprint) = crate::security::server().unwrap();
        let connector = crate::security::client(fingerprint).unwrap();
        let (viewer, host) = tokio::io::duplex(1 << 20);
        let host = Tap {
            io: host,
            written: Vec::new(),
        };
        let (viewer, host) = tokio::join!(
            connector.connect(
                quinn::rustls::pki_types::ServerName::try_from(crate::security::NAME).unwrap(),
                viewer,
            ),
            acceptor.accept(host),
        );
        let (mut viewer, mut host) = (viewer.unwrap(), host.unwrap());
        host.get_mut().0.written.clear();
        write(&mut host).await;
        let mut lengths = Vec::new();
        let mut rest = &host.get_ref().0.written[..];
        while let [_, _, _, high, low, tail @ ..] = rest {
            let length = usize::from(u16::from_be_bytes([*high, *low]));
            lengths.push(length);
            rest = &tail[length..];
        }
        let mut read = vec![0; expected];
        viewer.read_exact(&mut read).await.unwrap();
        (lengths, read)
    }

    /// A message leaves in the TLS records one write of its bytes makes: no
    /// record ends between the header and its payload.
    #[tokio::test]
    async fn a_message_is_cut_into_records_as_one_write_of_its_bytes() {
        let header = HostMessage::Probe { nonce: 1 };
        let json = serde_json::to_vec(&header).unwrap();
        for payload in [None, Some(vec![7; 3000]), Some(vec![9; 100_000])] {
            let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
            bytes.extend_from_slice(&json);
            if let Some(payload) = &payload {
                bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                bytes.extend_from_slice(payload);
            }
            let (cut, read) = records(
                async |host| {
                    write_message(host, &header, payload.as_deref())
                        .await
                        .unwrap();
                },
                bytes.len(),
            )
            .await;
            let (one_write, _) = records(
                async |host| {
                    host.write_all(&bytes).await.unwrap();
                    host.flush().await.unwrap();
                },
                bytes.len(),
            )
            .await;
            assert_eq!(cut, one_write);
            assert!(read == bytes, "the viewer read other bytes");
        }
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
