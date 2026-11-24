//! The out-of-process backend's framing: `u32 BE len | bytes` records over
//! any transport.
use crate::{Result, error};
use serde::{Serialize, de::DeserializeOwned};
use std::io::IoSlice;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The pipes' records are `u32 BE len | bytes`: JSON of at most `MAX_JSON`,
/// or a captured image of at most `MAX_FRAME`.
pub const MAX_JSON: usize = 8192;
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

pub async fn write_json<W: AsyncWrite + Unpin, T: Serialize>(io: &mut W, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    write_bytes(io, &bytes, MAX_JSON).await
}
pub async fn read_json<R: AsyncRead + Unpin, T: DeserializeOwned>(io: &mut R) -> Result<T> {
    let bytes = read_bytes(io, MAX_JSON).await?;
    serde_json::from_slice(&bytes).map_err(|_| error("invalid desktop message"))
}
/// One record, its length and its bytes in one write.
pub async fn write_bytes<W: AsyncWrite + Unpin>(
    io: &mut W,
    bytes: &[u8],
    limit: usize,
) -> Result<()> {
    let len = prefix(bytes, limit)?;
    write_slices(io, &mut [IoSlice::new(&len), IoSlice::new(bytes)]).await
}
/// The JSON record of `value` and then the byte record `bytes`, as
/// `write_json` and `write_bytes` frame them, in one vectored write: a
/// transport that completes each write on its own, such as an overlapped
/// named pipe, takes one completion for an answer and its image instead of
/// one per length and per body, and no byte is copied to join them. Nothing
/// is written unless both records are within their limits.
pub async fn write_record<W: AsyncWrite + Unpin, T: Serialize>(
    io: &mut W,
    value: &T,
    bytes: &[u8],
) -> Result<()> {
    let json = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    let json_len = prefix(&json, MAX_JSON)?;
    let bytes_len = prefix(bytes, MAX_FRAME)?;
    write_slices(
        io,
        &mut [
            IoSlice::new(&json_len),
            IoSlice::new(&json),
            IoSlice::new(&bytes_len),
            IoSlice::new(bytes),
        ],
    )
    .await
}
pub async fn read_bytes<R: AsyncRead + Unpin>(io: &mut R, limit: usize) -> Result<Vec<u8>> {
    let len = io.read_u32().await? as usize;
    if len > limit {
        return Err(error("desktop message too large"));
    }
    let mut bytes = vec![0; len];
    io.read_exact(&mut bytes).await?;
    Ok(bytes)
}

/// The length prefix of `bytes`, a record of at most `limit`.
fn prefix(bytes: &[u8], limit: usize) -> Result<[u8; 4]> {
    match u32::try_from(bytes.len()) {
        Ok(len) if bytes.len() <= limit => Ok(len.to_be_bytes()),
        _ => Err(error("desktop message too large")),
    }
}

/// All of `slices`, in as few writes as `io` takes them: one, unless it takes
/// part, when the rest goes on from where it stopped.
async fn write_slices<W: AsyncWrite + Unpin>(
    io: &mut W,
    mut slices: &mut [IoSlice<'_>],
) -> Result<()> {
    while !slices.is_empty() {
        let written = io.write_vectored(slices).await?;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        IoSlice::advance_slices(&mut slices, written);
    }
    io.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    /// A transport that takes at most `take` bytes a write, across slices,
    /// and counts its writes.
    struct Partial {
        take: usize,
        written: Vec<u8>,
        writes: usize,
    }
    impl Partial {
        fn new(take: usize) -> Self {
            Self {
                take,
                written: vec![],
                writes: 0,
            }
        }
    }
    impl AsyncWrite for Partial {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.poll_write_vectored(cx, &[IoSlice::new(buf)])
        }
        fn poll_write_vectored(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            slices: &[IoSlice<'_>],
        ) -> Poll<std::io::Result<usize>> {
            let mut left = self.take;
            for slice in slices {
                let part = &slice[..left.min(slice.len())];
                self.written.extend_from_slice(part);
                left -= part.len();
            }
            self.writes += 1;
            Poll::Ready(Ok(self.take - left))
        }
        fn is_write_vectored(&self) -> bool {
            true
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// The records as they always were: `u32 BE len | JSON`, then
    /// `u32 BE len | bytes`.
    fn framed(json: &[u8], bytes: &[u8]) -> Vec<u8> {
        let mut framed = vec![];
        for record in [json, bytes] {
            framed.extend_from_slice(&(record.len() as u32).to_be_bytes());
            framed.extend_from_slice(record);
        }
        framed
    }

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Answer {
        pending: u32,
    }

    /// An answer and its image leave in one write when the transport takes
    /// it whole, and byte for byte as before however little each write
    /// takes, ending in the middle of a slice or on its edge.
    #[tokio::test]
    async fn a_record_goes_out_in_one_write_and_resumes_after_a_partial_one() {
        let answer = Answer { pending: 3 };
        let image: Vec<u8> = (0..=255).cycle().take(1000).collect();
        let expected = framed(&serde_json::to_vec(&answer).unwrap(), &image);
        let mut whole = Partial::new(usize::MAX);
        write_record(&mut whole, &answer, &image).await.unwrap();
        assert_eq!(whole.writes, 1);
        assert!(whole.written == expected, "the records as they were");
        for take in [1, 3, 4, 17, 1000] {
            let mut partial = Partial::new(take);
            write_record(&mut partial, &answer, &image).await.unwrap();
            assert!(partial.written == expected, "{take} bytes a write");
            assert_eq!(partial.writes, expected.len().div_ceil(take));
        }
        // A lone record, such as the host's request, is one write too.
        let mut request = Partial::new(usize::MAX);
        write_json(&mut request, &answer).await.unwrap();
        assert_eq!(request.writes, 1);
        // An empty image is still its own record.
        let mut empty = Partial::new(usize::MAX);
        write_record(&mut empty, &answer, &[]).await.unwrap();
        assert_eq!(
            empty.written,
            framed(&serde_json::to_vec(&answer).unwrap(), &[])
        );
    }

    /// The reader takes back what one write sent as the two records it
    /// always read, over a pipe that moves a few bytes at a time.
    #[tokio::test]
    async fn the_reader_takes_back_both_records() {
        let (mut worker, mut host) = tokio::io::duplex(7);
        let image: Vec<u8> = (0..=255).cycle().take(5000).collect();
        let sent = image.clone();
        let writer = tokio::spawn(async move {
            write_record(&mut worker, &Answer { pending: 1 }, &sent).await?;
            write_record(&mut worker, &Answer { pending: 2 }, &[]).await
        });
        let first: Answer = read_json(&mut host).await.unwrap();
        assert_eq!(first, Answer { pending: 1 });
        assert_eq!(read_bytes(&mut host, MAX_FRAME).await.unwrap(), image);
        let second: Answer = read_json(&mut host).await.unwrap();
        assert_eq!(second, Answer { pending: 2 });
        assert!(read_bytes(&mut host, MAX_FRAME).await.unwrap().is_empty());
        writer.await.unwrap().unwrap();
    }

    /// An image over its limit fails the answer before any of it is
    /// written, so no half answer reaches the reader.
    #[tokio::test]
    async fn an_oversized_record_writes_nothing() {
        let mut pipe = Partial::new(usize::MAX);
        let image = vec![0; MAX_FRAME + 1];
        assert!(
            write_record(&mut pipe, &Answer { pending: 0 }, &image)
                .await
                .is_err()
        );
        let long = "x".repeat(MAX_JSON);
        assert!(write_record(&mut pipe, &long, &[]).await.is_err());
        assert_eq!((pipe.writes, pipe.written.len()), (0, 0));
    }
}
