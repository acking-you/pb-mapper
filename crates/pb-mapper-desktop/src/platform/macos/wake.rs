//! When a Mac host lights a sleeping display. ScreenCaptureKit delivers no
//! picture of a display that sleeps, and a viewer gets input only for a
//! picture it was shown, so a display left to sleep would keep a session
//! dark until someone at the Mac touched it. The host declares the local
//! user active instead (`power.rs`), as `caffeinate -u` does, which lights
//! every display: at most once every [`WAKE_EVERY`], however often capture
//! retries, and never for the built-in panel behind a closed lid, which
//! macOS would light for a few seconds and turn off again, over and over.
//!
//! Pure, with the time passed in: built and tested on every platform.

use std::time::{Duration, Instant};

/// The shortest time between two declarations: capture retries every
/// 200 ms while the display sleeps.
pub(crate) const WAKE_EVERY: Duration = Duration::from_secs(2);

/// When the host last declared the user active.
pub(crate) struct WakeLimiter {
    last: Option<Instant>,
}

impl WakeLimiter {
    pub(crate) const fn new() -> Self {
        Self { last: None }
    }

    /// Whether a declaration may go out at `now`, which then counts from
    /// `now`: none was made within [`WAKE_EVERY`] before it.
    pub(crate) fn admit(&mut self, now: Instant) -> bool {
        if !self.settled(now) {
            return false;
        }
        self.last = Some(now);
        true
    }

    /// Whether the last declaration, if any, is [`WAKE_EVERY`] old at
    /// `now`: a display it woke has had the time to light, so it may be
    /// released.
    pub(crate) fn settled(&self, now: Instant) -> bool {
        self.last.is_none_or(|at| {
            now.checked_duration_since(at)
                .is_some_and(|age| age >= WAKE_EVERY)
        })
    }
}

/// Whether a declaration should light a display that is `asleep` and,
/// when `builtin`, whose lid `lid_closed` says is closed: a sleeping one,
/// unless it is the built-in panel behind a closed lid. `lid_closed` is
/// read only for a sleeping built-in panel.
pub(crate) fn wakeable(asleep: bool, builtin: bool, lid_closed: impl FnOnce() -> bool) -> bool {
    asleep && !(builtin && lid_closed())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture's 200 ms retries declare once every 2 s, and a declaration
    /// is released only once it is 2 s old.
    #[test]
    fn a_sleeping_display_is_woken_at_most_every_two_seconds() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut limiter = WakeLimiter::new();
        assert!(limiter.settled(t0), "nothing declared yet");
        assert!(limiter.admit(t0));
        for retry in (200..2_000).step_by(200) {
            assert!(!limiter.admit(ms(retry)), "{retry} ms");
        }
        assert!(!limiter.admit(ms(1_999)));
        assert!(
            !limiter.settled(ms(1_999)),
            "the display may still be lighting"
        );
        assert!(limiter.settled(ms(2_000)));
        assert!(limiter.admit(ms(2_000)));
        assert!(!limiter.admit(ms(2_200)));
        // A time before the last declaration never admits another.
        assert!(!limiter.admit(ms(1_000)));
        assert!(!limiter.settled(ms(1_000)));
    }

    /// Only a sleeping display is woken, and the built-in panel not while
    /// the lid is closed; the lid is read for nothing else.
    #[test]
    fn the_built_in_panel_behind_a_closed_lid_is_not_woken() {
        let unread = || panic!("the lid is read only for a sleeping built-in panel");
        assert!(!wakeable(false, true, unread));
        assert!(!wakeable(false, false, unread));
        assert!(wakeable(true, false, unread), "an external display");
        assert!(wakeable(true, true, || false), "the lid is open");
        assert!(!wakeable(true, true, || true), "behind the closed lid");
    }
}
