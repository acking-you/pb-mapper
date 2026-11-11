//! Buffers for reading frames of a known length and raw chunks of an unknown one.

use snafu::ResultExt;
use tokio::io::AsyncReadExt;

use pb_mapper_core::error::MsgNetworkReadBufferdRawDataSnafu;

const INIT_BUF_SIZE: usize = 8 * 1024;
/// The largest read handed on in one piece, and so the largest frame an
/// encrypting forwarder sends; frames a peer sends are read up to
/// `MAX_PLAINTEXT_LEN` whatever this is. A reader keeps the buffer of its
/// largest read, and of its largest frame, until its stream ends: between
/// peers of this version, this is the most a direction of an idle tunnel holds
/// after a burst. 64 KiB cost QUIC throughput; see
/// `docs/benchmarks/2026-11-11-refactor/open-items-relay.md`.
const MAX_BUF_SIZE: usize = 1024 * 1024;
const _: () = assert!(
    MAX_BUF_SIZE <= crate::MAX_PLAINTEXT_LEN as usize,
    "a forwarder's frame is one its peer reads"
);

pub struct CommonBuffer {
    buffer: Vec<u8>,
    need_size: usize,
}

impl Default for CommonBuffer {
    fn default() -> Self {
        CommonBuffer::new()
    }
}

impl CommonBuffer {
    pub fn new() -> Self {
        Self {
            buffer: vec![0; INIT_BUF_SIZE],
            need_size: INIT_BUF_SIZE,
        }
    }

    /// Whether the next unknown-length read wants a different buffer length.
    #[inline]
    pub fn need_resize(&self) -> bool {
        self.buffer.len() != self.need_size
    }

    /// Dynamic capacity adjustment based on `need_size`
    #[inline]
    pub fn dyn_resize(&mut self) {
        if self.need_size >= MAX_BUF_SIZE {
            self.need_size = MAX_BUF_SIZE;
        }
        self.buffer.resize(self.need_size, 0);
    }

    /// If the `buffer` is filled, it is expanded; if the `buffer` is not filled and the filled
    /// content is less than `INIT_BUF_SIZE` bytes and the `need_size` is greater than
    /// `INIT_BUF_SIZE`, it is shrunk.
    ///
    /// # Arguments `n` : how many bytes were read into the
    /// buffer this time
    #[inline]
    pub fn update_need_size(&mut self, n: usize) {
        // update `need_size` for expand
        if n == self.buffer.len() {
            self.need_size = n * 2;
        }
        // update `need_size` for shrink
        else if n != 0 && n < INIT_BUF_SIZE && self.need_size > INIT_BUF_SIZE {
            self.need_size = INIT_BUF_SIZE;
        }
    }

    /// Sets the length to exactly `size`, for a read whose length is known.
    #[inline]
    pub fn fixed_resize(&mut self, size: usize) {
        self.buffer.resize(size, 0)
    }

    #[inline]
    pub fn buffer(&self) -> &'_ [u8] {
        &self.buffer
    }

    #[inline]
    pub fn buffer_mut(&mut self) -> &'_ mut [u8] {
        &mut self.buffer
    }
}

/// Reads whatever has arrived, into a buffer that grows and shrinks with it.
pub struct BufferReader<'a, T> {
    reader: &'a mut T,
    buffer: CommonBuffer,
}

impl<'reader, T: AsyncReadExt + Unpin> BufferReader<'reader, T> {
    pub fn new(reader: &'reader mut T) -> Self {
        Self {
            reader,
            buffer: CommonBuffer::new(),
        }
    }

    pub async fn read(&mut self) -> pb_mapper_core::error::Result<&[u8]> {
        if self.buffer.need_resize() {
            self.buffer.dyn_resize()
        }
        let n = self
            .reader
            .read(self.buffer.buffer_mut())
            .await
            .context(MsgNetworkReadBufferdRawDataSnafu)?;
        self.buffer.update_need_size(n);
        Ok(&self.buffer.buffer()[0..n])
    }
}
