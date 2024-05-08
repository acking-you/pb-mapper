#[derive(Debug, Default, Clone, Copy)]
pub struct TimeoutCount {
    count: usize,
}

impl TimeoutCount {
    #[inline]
    pub fn new(count: usize) -> Self {
        Self { count }
    }

    #[inline]
    pub fn validate(&mut self) -> bool {
        if self.count == 0 {
            false
        } else {
            self.count -= 1;
            true
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn reset(&mut self, count: usize) {
        self.count = count
    }
}
