//! Wall-clock time and durations in the whole units the wire, state files
//! and diagnostics carry.
//!
//! Each conversion saturates rather than wraps, and a clock set before the
//! Unix epoch reads as the epoch itself.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Whole seconds since the Unix epoch.
pub fn unix_seconds() -> u64 {
    since_epoch().as_secs()
}

/// Whole milliseconds since the Unix epoch.
pub fn unix_millis() -> u64 {
    saturating_millis(since_epoch())
}

/// `duration` in whole milliseconds, `u64::MAX` past what that holds.
pub fn saturating_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// `duration` in whole microseconds, `u64::MAX` past what that holds.
pub fn saturating_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_truncate_and_saturate() {
        assert_eq!(saturating_millis(Duration::from_micros(1_999)), 1);
        assert_eq!(saturating_micros(Duration::from_nanos(1_999)), 1);
        assert_eq!(saturating_millis(Duration::MAX), u64::MAX);
        assert_eq!(saturating_micros(Duration::MAX), u64::MAX);
    }
}
