//! When a Mac host lights a sleeping display. ScreenCaptureKit delivers no
//! picture of a display that sleeps, and a viewer gets input only for a
//! picture it was shown, so a display left to sleep would keep a session
//! dark until someone at the Mac touched it. The host declares the local
//! user active instead (`power.rs`), as `caffeinate -u` does, which lights
//! every display: at most once every [`WAKE_EVERY`], however often capture
//! retries, and never for the built-in panel behind a closed lid, which
//! macOS would light for a few seconds and turn off again, over and over.
//! A display that turns off under a running capture is left dark for
//! [`SETTLE`] first ([`SleepWatch`]).
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

/// Whether a declaration should light a display that is `asleep`, which
/// `builtin` says is the built-in panel and `lid_closed` whether its lid is
/// closed: a sleeping one, unless it is the built-in panel behind a closed
/// lid. `builtin` is read only for a sleeping display, and `lid_closed`
/// only for a sleeping built-in panel.
pub(crate) fn wakeable(
    asleep: bool,
    builtin: impl FnOnce() -> bool,
    lid_closed: impl FnOnce() -> bool,
) -> bool {
    asleep && !(builtin() && lid_closed())
}

/// Whether a declaration should light a display that is not listed as awake
/// or asleep (an external monitor in deep standby that dropped HPD): when no
/// display is awake at all and no lid is closed. A portable with a closed lid
/// must never light its built-in panel over and over when its external
/// display was unplugged.
pub(crate) fn wake_unlisted(any_awake: bool, lid_closed: impl FnOnce() -> bool) -> bool {
    !any_awake && !lid_closed()
}

/// How long a display that turned off under a running capture is left
/// dark before it is lit, the session checked before each look: a Mac that
/// locks as its display turns off then has the time to say so, and stays
/// dark.
pub(crate) const SETTLE: Duration = Duration::from_secs(1);

/// What one capture found of its display. Found asleep with nothing found
/// in the [`SETTLE`] before (a session's start, a resume after a pause), it
/// is lit at once. Found asleep within [`SETTLE`] of being found awake, it
/// may just have turned off as the Mac locks, and is lit only once it has
/// been found asleep for [`SETTLE`].
#[derive(Default)]
pub(crate) struct SleepWatch {
    /// When the display was last found awake.
    awake: Option<Instant>,
    /// When it was first found asleep since.
    asleep: Option<Instant>,
}

impl SleepWatch {
    /// The display was found awake at `now`.
    pub(crate) fn awake(&mut self, now: Instant) {
        self.awake = Some(now);
        self.asleep = None;
    }

    /// The display was found asleep at `now`, after the session check
    /// passed: whether to light it now.
    pub(crate) fn asleep(&mut self, now: Instant) -> bool {
        let since = *self.asleep.get_or_insert(now);
        let turned_off = self
            .awake
            .is_some_and(|awake| since.saturating_duration_since(awake) < SETTLE);
        !turned_off || now.saturating_duration_since(since) >= SETTLE
    }
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
    /// the lid is closed; an awake display is asked nothing more, and the
    /// lid is read for nothing else.
    #[test]
    fn the_built_in_panel_behind_a_closed_lid_is_not_woken() {
        let unread = || panic!("read only for a sleeping display");
        let lid_unread = || panic!("the lid is read only for a sleeping built-in panel");
        assert!(!wakeable(false, unread, unread));
        assert!(wakeable(true, || false, lid_unread), "an external display");
        assert!(wakeable(true, || true, || false), "the lid is open");
        assert!(!wakeable(true, || true, || true), "behind the closed lid");
        assert!(
            !wake_unlisted(true, lid_unread),
            "another display already draws"
        );
        assert!(
            wake_unlisted(false, || false),
            "deep-standby external monitor with no awake display"
        );
        assert!(
            !wake_unlisted(false, || true),
            "unplugged external monitor with closed lid"
        );
    }

    /// A display found asleep at a session's start or a resume is lit at
    /// once; one that turns off under a running capture, found awake a
    /// moment before, only once it has been found asleep for 1 s, and
    /// found awake again it starts over.
    #[test]
    fn a_display_that_turns_off_under_capture_is_left_dark_for_a_second() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        assert!(SleepWatch::default().asleep(t0), "nothing found before");

        let mut watch = SleepWatch::default();
        watch.awake(t0);
        // Stills every 125 ms, then retries every 200 ms from 250 ms.
        for at in [250, 450, 650, 850, 1_050, 1_249] {
            assert!(!watch.asleep(ms(at)), "{at} ms");
        }
        assert!(watch.asleep(ms(1_250)), "asleep for 1 s");
        assert!(watch.asleep(ms(1_450)), "and still lit while asleep");

        watch.awake(ms(2_000));
        assert!(!watch.asleep(ms(2_500)), "turned off again");
        assert!(watch.asleep(ms(3_500)));

        // Found awake long before: a resume after a pause.
        let mut watch = SleepWatch::default();
        watch.awake(t0);
        assert!(watch.asleep(ms(5_000)));
    }
}
