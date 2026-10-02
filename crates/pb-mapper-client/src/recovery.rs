//! Short, bounded setup attempts; established streams have separate lifetimes.

use std::time::Duration;

use pb_mapper_core::config::control_io_timeout;

const MIN_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_TIMEOUT: Duration = Duration::from_secs(5);

/// Estimate the whole setup latency, including dial and authenticated response.
/// A variance margin tolerates jitter; expired attempts increase the budget.
/// Neither a dead socket nor a TCP SYN can hold recovery for the OS timeout.
#[derive(Debug)]
pub(crate) struct RecoveryTiming {
    smoothed: Option<Duration>,
    variation: Duration,
    budget: Duration,
}

impl Default for RecoveryTiming {
    fn default() -> Self {
        Self {
            smoothed: None,
            variation: Duration::ZERO,
            budget: Duration::from_secs(2),
        }
    }
}

impl RecoveryTiming {
    pub(crate) fn timeout(&self) -> Duration {
        self.budget
            .clamp(MIN_TIMEOUT, MAX_TIMEOUT)
            .min(control_io_timeout())
    }

    pub(crate) fn record(&mut self, elapsed: Duration) {
        match self.smoothed {
            Some(previous) => {
                self.variation =
                    self.variation.mul_f64(0.75) + previous.abs_diff(elapsed).mul_f64(0.25);
                self.smoothed = Some(previous.mul_f64(0.875) + elapsed.mul_f64(0.125));
            }
            None => {
                self.smoothed = Some(elapsed);
                self.variation = elapsed / 2;
            }
        }
        self.budget = self.smoothed.unwrap_or(elapsed) + self.variation * 4;
    }

    pub(crate) fn timed_out(&mut self) {
        self.budget = (self.timeout() * 2).min(MAX_TIMEOUT);
    }
}

/// Preserve a real delay on fast failures, without synchronizing all pool workers.
pub(crate) fn jitter(delay: Duration) -> Duration {
    delay.mul_f64(rand::random_range(0.75..=1.0))
}
