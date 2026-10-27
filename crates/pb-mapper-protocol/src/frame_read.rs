//! Frame progress belongs to the reader, not the cancellable read future.

use std::io::{Error, ErrorKind, Result};

use tokio::io::AsyncReadExt;

pub(crate) struct FrameRead<const N: usize> {
    pub(crate) header: [u8; N],
    header_read: usize,
    body_read: usize,
}

impl<const N: usize> FrameRead<N> {
    pub(crate) fn new() -> Self {
        Self {
            header: [0; N],
            header_read: 0,
            body_read: 0,
        }
    }

    pub(crate) async fn header(&mut self, reader: &mut (impl AsyncReadExt + Unpin)) -> Result<()> {
        if self.header_or_eof(reader).await? {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::UnexpectedEof,
            "missing message frame",
        ))
    }

    /// FIN is clean only between frames, never within a header or body.
    pub(crate) async fn header_or_eof(
        &mut self,
        reader: &mut (impl AsyncReadExt + Unpin),
    ) -> Result<bool> {
        while self.header_read < N {
            let n = reader.read(&mut self.header[self.header_read..]).await?;
            if n == 0 {
                return if self.header_read == 0 {
                    Ok(false)
                } else {
                    Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "incomplete message header",
                    ))
                };
            }
            self.header_read += n;
        }
        Ok(true)
    }

    pub(crate) async fn body(
        &mut self,
        reader: &mut (impl AsyncReadExt + Unpin),
        buffer: &mut [u8],
    ) -> Result<()> {
        read_remaining(reader, buffer, &mut self.body_read).await
    }

    pub(crate) fn finish(&mut self) {
        self.header_read = 0;
        self.body_read = 0;
    }
}

async fn read_remaining(
    reader: &mut (impl AsyncReadExt + Unpin),
    buffer: &mut [u8],
    offset: &mut usize,
) -> Result<()> {
    while *offset < buffer.len() {
        // AsyncReadExt::read is cancellation safe. Persist progress before
        // awaiting again, so heartbeat/task completion branches cannot lose it.
        let n = reader.read(&mut buffer[*offset..]).await?;
        if n == 0 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "incomplete message frame",
            ));
        }
        *offset += n;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_eof_between_complete_frames_is_clean() {
        let mut frame = FrameRead::<8>::new();
        assert!(!frame.header_or_eof(&mut &[][..]).await.unwrap());
        assert!(frame.header(&mut &[][..]).await.is_err());
        assert!(frame.header_or_eof(&mut &[0; 3][..]).await.is_err());
        // A later poll cannot forget that part of the header was consumed.
        assert!(frame.header_or_eof(&mut &[][..]).await.is_err());
        let mut frame = FrameRead::<8>::new();
        frame.header(&mut &[0; 8][..]).await.unwrap();
        assert!(frame.body(&mut &[0; 1][..], &mut [0; 2]).await.is_err());
    }
}
