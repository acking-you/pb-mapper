use std::time::Duration;

/// A retry ladder: a delay that doubles from a minimum up to a maximum with
/// each failure, until [`Self::reset`].
///
/// Plain, it waits exactly that delay, for ladders whose sequence is a
/// contract. [`Self::with_jitter`] spreads it, for retries against a shared
/// server.
#[derive(Debug, Clone, Copy)]
pub struct RetryBackoff {
    failures: u32,
    min_delay: Duration,
    max_delay: Duration,
    jitter: bool,
}

const _: () = assert!(
    RetryBackoff::MAX_DOUBLINGS < u32::BITS,
    "the multiplier is a u32 shifted left by at most MAX_DOUBLINGS"
);

impl Default for RetryBackoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(100), Duration::from_secs(1))
    }
}

impl RetryBackoff {
    /// How many times the delay doubles before it stops growing: past that it
    /// stays at the minimum times `2^MAX_DOUBLINGS` (1024), or the maximum if
    /// that is lower.
    pub const MAX_DOUBLINGS: u32 = 10;

    pub fn new(min_delay: Duration, max_delay: Duration) -> Self {
        assert!(
            !min_delay.is_zero(),
            "RetryBackoff needs a positive minimum delay"
        );
        assert!(
            max_delay >= min_delay,
            "RetryBackoff's maximum delay must not be below its minimum"
        );
        Self {
            failures: 0,
            min_delay,
            max_delay,
            jitter: false,
        }
    }

    /// This ladder, each delay drawn at random from its last quarter: from
    /// three quarters of the plain delay up to the plain delay itself.
    ///
    /// For retries that many processes make against one server, such as
    /// clients of a relay that restarted: they failed together and would
    /// otherwise come back together, at every step. A delay is never longer
    /// than the plain one, so the cap holds and no retry comes later.
    pub fn with_jitter(self) -> Self {
        Self {
            jitter: true,
            ..self
        }
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    pub fn reset(&mut self) {
        self.failures = 0;
    }

    pub fn next_delay(&mut self) -> Duration {
        let multiplier = 1_u32 << self.failures.min(Self::MAX_DOUBLINGS);
        self.failures = self.failures.saturating_add(1);
        let delay = self
            .min_delay
            .saturating_mul(multiplier)
            .min(self.max_delay);
        if !self.jitter {
            return delay;
        }
        // In whole nanoseconds, so the draw stays within the quarter exactly.
        let quarter = u64::try_from((delay / 4).as_nanos()).unwrap_or(u64::MAX);
        delay.saturating_sub(Duration::from_nanos(rand::random_range(0..=quarter)))
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

    /// Below a distant maximum, the delay stops growing at 1024 times the
    /// minimum ([`RetryBackoff::MAX_DOUBLINGS`]), however many failures follow.
    #[test]
    fn retry_backoff_stops_doubling_at_1024_times_the_minimum() {
        let mut backoff = RetryBackoff::new(Duration::from_millis(1), Duration::from_secs(3600));
        let delays: Vec<_> = (0..64).map(|_| backoff.next_delay()).collect();
        assert_eq!(delays[10], Duration::from_millis(1024));
        assert!(delays[10..].iter().all(|delay| *delay == delays[10]));
        assert!(delays[..10].windows(2).all(|pair| pair[1] == pair[0] * 2));
    }

    /// A jittered ladder waits at most the plain ladder's delay, so its cap
    /// holds and its first retry comes no later, and at least three quarters
    /// of it, so it still doubles. Its draws differ, which is what spreads
    /// the retries of clients that failed together.
    #[test]
    fn a_jittered_backoff_draws_from_the_last_quarter_of_the_plain_ladder() {
        let (min, max) = (Duration::from_millis(100), Duration::from_secs(2));
        let mut plain = RetryBackoff::new(min, max);
        let mut jittered = RetryBackoff::new(min, max).with_jitter();
        let draws: Vec<_> = (0..64)
            .map(|_| (plain.next_delay(), jittered.next_delay()))
            .collect();
        for (plain, jittered) in &draws {
            assert!(
                *jittered <= *plain && *jittered >= *plain - *plain / 4,
                "{jittered:?} against {plain:?}"
            );
        }
        assert!(draws.iter().any(|(plain, jittered)| plain != jittered));
        jittered.reset();
        assert!(jittered.next_delay() <= min);
    }
}
