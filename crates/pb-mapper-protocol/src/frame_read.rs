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
        read_remaining(reader, &mut self.header, &mut self.header_read).await
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
