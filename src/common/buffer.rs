//! Define the buffer interface for reading data in different situations

const INIT_BUF_SIZE: usize = 8 * 1024;
const MAX_BUF_SIZE: usize = 8 * 1024 * 1024;
/// Buffer for situations where the length of the data to be read is not known
pub trait DynamicSizeBuffer {
    fn need_resize(&self) -> bool;

    /// Dynamic capacity adjustment based on `need_size`
    fn dyn_resize(&mut self);

    /// If the `buffer` is filled, it is expanded; if the `buffer` is not filled and the filled
    /// content is less than `INIT_BUF_SIZE` bytes and the `need_size` is greater than
    /// `INIT_BUF_SIZE`, it is shrunk.
    ///
    /// # Arguments `n` : how many bytes were read into the
    /// buffer this time
    fn update_need_size(&mut self, n: usize);
}

/// Buffer for situations where the length of the data to be read is already known
pub trait FixedSizeBuffer {
    /// The length of the buffer must reach `size` after resize, as opposed to
    /// [`DynamicSizeBuffer`]'s internal state-based resize.
    fn fixed_resize(&mut self, size: usize);
}

pub trait BufferGetter {
    fn buffer(&self) -> &'_ [u8];

    fn buffer_mut(&mut self) -> &'_ mut [u8];
}

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
}

impl DynamicSizeBuffer for CommonBuffer {
    #[inline]
    fn need_resize(&self) -> bool {
        self.buffer.len() != self.need_size
    }

    #[inline]
    fn dyn_resize(&mut self) {
        if self.need_size >= MAX_BUF_SIZE {
            self.need_size = MAX_BUF_SIZE;
        }
        self.buffer.resize(self.need_size, 0);
    }

    #[inline]
    fn update_need_size(&mut self, n: usize) {
        // update `need_size` for expand
        if n == self.buffer.len() {
            self.need_size = n * 2;
        }
        // update `need_size` for shrink
        else if n != 0 && n < INIT_BUF_SIZE && self.need_size > INIT_BUF_SIZE {
            self.need_size = INIT_BUF_SIZE;
        }
    }
}

impl FixedSizeBuffer for CommonBuffer {
    #[inline]
    fn fixed_resize(&mut self, size: usize) {
        self.buffer.resize(size, 0)
    }
}

impl BufferGetter for CommonBuffer {
    #[inline]
    fn buffer(&self) -> &'_ [u8] {
        &self.buffer
    }

    #[inline]
    fn buffer_mut(&mut self) -> &'_ mut [u8] {
        &mut self.buffer
    }
}
