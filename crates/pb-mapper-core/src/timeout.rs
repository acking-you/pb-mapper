use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct RetryBackoff {
    failures: u32,
    min_delay: Duration,
    max_delay: Duration,
}

impl Default for RetryBackoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(100), Duration::from_secs(1))
    }
}

impl RetryBackoff {
    pub fn new(min_delay: Duration, max_delay: Duration) -> Self {
        assert!(!min_delay.is_zero());
        assert!(max_delay >= min_delay);
        Self {
            failures: 0,
            min_delay,
            max_delay,
        }
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    pub fn reset(&mut self) {
        self.failures = 0;
    }

    pub fn next_delay(&mut self) -> Duration {
        let multiplier = 1_u32.checked_shl(self.failures.min(10)).unwrap_or(1);
        self.failures = self.failures.saturating_add(1);
        self.min_delay
            .saturating_mul(multiplier)
            .min(self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_backoff_caps_and_resets_without_exhausting() {
        let mut backoff = RetryBackoff::new(Duration::from_millis(100), Duration::from_secs(1));

        assert_eq!(backoff.next_delay(), Duration::from_millis(100));
        assert_eq!(backoff.next_delay(), Duration::from_millis(200));
        assert_eq!(backoff.next_delay(), Duration::from_millis(400));
        assert_eq!(backoff.next_delay(), Duration::from_millis(800));
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        for _ in 0..32 {
            assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        }

        assert!(backoff.failures() > 0);
        backoff.reset();
        assert_eq!(backoff.failures(), 0);
        assert_eq!(backoff.next_delay(), Duration::from_millis(100));
    }
}
