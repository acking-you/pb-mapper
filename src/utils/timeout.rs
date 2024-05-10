#[derive(Debug, Default, Clone, Copy)]
pub struct TimeoutCount {
    cur_count: u32,
    max_count: u32,
}

impl TimeoutCount {
    #[inline]
    pub fn new(max_count: u32) -> Self {
        assert!(max_count > 0);
        Self {
            cur_count: 0,
            max_count,
        }
    }

    #[inline]
    pub fn validate(&mut self) -> bool {
        if self.cur_count == self.max_count {
            false
        } else {
            self.cur_count += 1;
            true
        }
    }

    pub fn count(&self) -> usize {
        self.cur_count as usize
    }

    #[inline]
    pub fn reset(&mut self) {
        self.cur_count = 0
    }

    /// Based on the current count, calculate an exponent, and then determine the interval based on
    /// this exponent.
    #[inline]
    pub fn get_interval_by_count(&self) -> u64 {
        // maximum exponent of 2 is 10, which corresponds to 1024 seconds
        let exponent = self.cur_count.min(10);
        1 << exponent
    }
}
