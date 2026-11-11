//! Short, bounded setup attempts; established streams have separate lifetimes.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use pb_mapper_core::config::control_io_timeout;
use pb_mapper_core::timeout::RetryBackoff;

const MIN_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_TIMEOUT: Duration = Duration::from_secs(5);
/// Where a setup estimate starts, before any attempt has been timed.
const INITIAL_BUDGET: Duration = Duration::from_secs(2);
const _: () = assert!(
    MIN_TIMEOUT.as_millis() <= INITIAL_BUDGET.as_millis()
        && INITIAL_BUDGET.as_millis() <= MAX_TIMEOUT.as_millis(),
    "the first estimate lies within the clamp every later one goes through"
);

/// The least a data stream's subscribe attempt is given, whatever the setup
/// estimate: its answer waits on the serving side opening a stream of its own
/// to the relay.
pub(crate) const MIN_SUBSCRIBE_ATTEMPT: Duration = Duration::from_secs(3);

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
            budget: INITIAL_BUDGET,
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

/// The one [`RecoveryTiming`] a connect tunnel's probes and stream setups all
/// learn from. Each call holds the lock for itself alone, never across an
/// await.
#[derive(Debug, Default)]
pub(crate) struct SharedTiming(Mutex<RecoveryTiming>);

impl SharedTiming {
    fn timing(&self) -> MutexGuard<'_, RecoveryTiming> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn timeout(&self) -> Duration {
        self.timing().timeout()
    }

    pub(crate) fn record(&self, elapsed: Duration) {
        self.timing().record(elapsed);
    }

    pub(crate) fn timed_out(&self) {
        self.timing().timed_out();
    }
}

/// The ladder both tunnel directions climb when they cannot reach or hold the
/// relay: quick, because retrying is how a tunnel comes back, and jittered,
/// so the tunnels of every process that lost the relay together do not come
/// back at the same instants.
pub(crate) fn transport_backoff() -> RetryBackoff {
    RetryBackoff::new(Duration::from_millis(100), Duration::from_secs(2)).with_jitter()
}

/// How often a tunnel whose credential the relay refused for good asks again
/// while a direct session it holds is still up: revocation closes the control
/// plane, not the endpoint-owned sessions.
pub(crate) const REVOKED_DIRECT_RECHECK: Duration = Duration::from_secs(30);
