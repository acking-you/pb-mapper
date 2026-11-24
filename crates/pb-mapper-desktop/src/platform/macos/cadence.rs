//! How often ScreenCaptureKit may deliver a picture of a Mac host's display
//! (`SCStreamConfiguration.minimumFrameInterval`), and when that changes.
//!
//! The window server composes at the display's refreshes, so a picture can
//! only come at one. A minimum interval that is not a whole number of
//! refresh periods, if ScreenCaptureKit holds each picture to it strictly,
//! rounds every delivered interval up to the next whole number of them, and
//! where the refresh is not a multiple of the frame rate that passes fewer
//! pictures than the frame rate. The interval here is half a refresh short
//! of the refreshes it spans, `(n - 0.5) / refresh`, which passes every
//! n-th refresh with half a period to spare either way. `n` is the pace's
//! own multiple where the refresh is one (within 1%), so that
//! ScreenCaptureKit delivers at the pace and thread C's anchored pacing
//! takes each picture as it arrives; otherwise it is every refresh while
//! the refresh is below eight times the pace, the freshest pictures for
//! thread C to take from, and every `floor(refresh / pace / 4)`-th above.
//! With no refresh known, the interval stays `0.9 / max_fps`, as before,
//! and does not follow the pace. Whether ScreenCaptureKit gates that
//! strictly is what the ignored measurement `sck_cadence` shows on a
//! display.
//!
//! No interval is longer than that one before, the limit's [`window`]: a
//! change on screen that ScreenCaptureKit holds back for the interval must
//! still arrive at the next refresh it allows, and until a display has
//! shown that it does (the ignored measurement `sck_held_back_change`), no
//! change waits for its picture longer than it did before. So `n` is at
//! most the largest whole number of refreshes that fits in the window, and
//! a slower pace, or every viewer pausing, slows the stream only that far.
//!
//! [`Follow`] decides the interval a stream runs at: a faster pace applies
//! at once, a slower one once it has held for [`SLOWER_HOLD`], a new
//! refresh, frame-rate limit or pause at once. [`Updates`] hands the stream
//! one change at a time. Pure: built and tested on every platform.

use std::time::{Duration, Instant};

/// How long a slower pace must hold before the stream slows to it, so that
/// a pace that dips and comes back does not change the stream twice.
pub(crate) const SLOWER_HOLD: Duration = Duration::from_secs(1);
/// How close to a whole multiple of the pace a refresh counts as one,
/// relative to the multiple: 119.88 Hz is twice 60.
const MULTIPLE_TOLERANCE: f64 = 0.01;
/// The fastest refresh believed, in hertz.
const REFRESH_MAX: f64 = 1_000.0;

/// What a stream's interval is made of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cadence {
    /// Every how many refreshes a picture may come; `None` with no refresh.
    pub every: Option<u32>,
    pub interval: Duration,
    /// `every` is the pace's own multiple of refreshes, so that pictures
    /// come at the pace.
    pub paced: bool,
}

/// A display's refresh rate: its mode's when that reports one above 1 Hz,
/// else the nominal period of a display link on it (a built-in panel's
/// mode may report 0), else none.
pub(crate) fn refresh_hz(mode_hz: f64, link_period: Option<Duration>) -> Option<f64> {
    believed(mode_hz).or_else(|| {
        link_period
            .filter(|period| !period.is_zero())
            .and_then(|period| believed(period.as_secs_f64().recip()))
    })
}

/// A refresh rate a display can have.
fn believed(hz: f64) -> Option<f64> {
    (hz.is_finite() && hz > 1.0 && hz <= REFRESH_MAX).then_some(hz)
}

/// The longest interval a stream runs at under a frame-rate limit of
/// `max_fps`: the one it ran at before the interval followed the refresh, a
/// tenth short of the limit's period, in whole nanoseconds.
pub(crate) fn window(max_fps: u32) -> Duration {
    Duration::from_nanos(900_000_000 / u64::from(max_fps.clamp(1, 1_000)))
}

/// The most refreshes an interval may span on a display at `refresh` Hz
/// under a limit of `max_fps`: the largest `n` whose `(n - 0.5) / refresh`
/// is within the [`window`], and at least 1, half a refresh, which holds no
/// picture back from the refresh after the last.
fn most(refresh: f64, max_fps: u32) -> f64 {
    // A nanosecond's slack, so that an interval equal to the window fits.
    (window(max_fps).as_secs_f64() * refresh + 0.5 + 1e-9)
        .floor()
        .max(1.0)
}

/// A picture every `every` refreshes of a display at `refresh` Hz.
fn spanning(refresh: f64, every: f64, paced: bool) -> Cadence {
    Cadence {
        // At most `refresh`, itself at most `REFRESH_MAX`.
        every: Some(every as u32),
        interval: Duration::from_secs_f64((every - 0.5) / refresh),
        paced,
    }
}

/// With no refresh known: the window, whatever the pace.
fn unknown(max_fps: u32) -> Cadence {
    Cadence {
        every: None,
        interval: window(max_fps),
        paced: false,
    }
}

/// The interval for a session paced at `target_fps` on a display refreshing
/// at `refresh_hz`, or, with none, for one whose frame-rate limit is
/// `max_fps`; never longer than that limit's [`window`].
pub(crate) fn cadence(refresh_hz: Option<f64>, target_fps: u32, max_fps: u32) -> Cadence {
    let Some(refresh) = refresh_hz.and_then(believed) else {
        return unknown(max_fps);
    };
    let multiple = refresh / f64::from(target_fps.max(1));
    let nearest = multiple.round();
    let exact = nearest >= 1.0 && (multiple - nearest).abs() <= MULTIPLE_TOLERANCE * nearest;
    let wanted = if exact {
        nearest
    } else {
        (multiple / 4.0).floor().max(1.0)
    };
    let every = wanted.min(most(refresh, max_fps));
    spanning(refresh, every, exact && every == wanted)
}

/// The longest interval allowed under a limit of `max_fps`, which a stream
/// runs at while every viewer is paused.
pub(crate) fn slowest(refresh_hz: Option<f64>, max_fps: u32) -> Cadence {
    match refresh_hz.and_then(believed) {
        Some(refresh) => spanning(refresh, most(refresh, max_fps), false),
        None => unknown(max_fps),
    }
}

/// How much faster than thread C's grid at `target_fps` a picture every
/// `every` periods of a display link comes, in parts per million: positive
/// where such a display drifted ahead of an even grid, each picture waiting
/// longer for its tick until one was replaced. It means something only
/// where `every` is the pace's multiple ([`Cadence::paced`]).
pub(crate) fn drift_ppm(every: u32, link_period: Duration, target_fps: u32) -> Option<i64> {
    let delivered = link_period.as_secs_f64() * f64::from(every);
    (delivered > 0.0 && target_fps > 0).then(|| {
        let grid = f64::from(target_fps).recip();
        ((grid / delivered - 1.0) * 1e6).round() as i64
    })
}

/// The interval a stream runs at, as the session's pace, the display's
/// refresh, the frame-rate limit and its viewers' pauses change.
#[derive(Debug)]
pub(crate) struct Follow {
    refresh: Option<f64>,
    max_fps: u32,
    pace: u32,
    paused: bool,
    /// The interval last handed to the stream.
    applied: Duration,
    /// Since when a slower interval than `applied` has been wanted without
    /// a break.
    slower_since: Option<Instant>,
}

impl Follow {
    /// A stream opened at what `refresh`, `pace` and `max_fps` want.
    pub(crate) fn new(refresh: Option<f64>, pace: u32, max_fps: u32) -> Self {
        let mut follow = Self {
            refresh,
            max_fps,
            pace,
            paused: false,
            applied: Duration::ZERO,
            slower_since: None,
        };
        follow.applied = follow.wanted().interval;
        follow
    }

    /// What the stream should run at now: the pace's cadence, or, while
    /// every viewer is paused, the slowest allowed.
    pub(crate) fn wanted(&self) -> Cadence {
        if self.paused {
            slowest(self.refresh, self.max_fps)
        } else {
            cadence(self.refresh, self.pace.min(self.max_fps), self.max_fps)
        }
    }

    /// The interval last handed to the stream.
    pub(crate) fn applied(&self) -> Duration {
        self.applied
    }

    /// Every viewer is paused.
    pub(crate) fn is_paused(&self) -> bool {
        self.paused
    }

    /// The session's pace at `now`: the interval to hand the stream, when it
    /// changes. A faster one applies at once, a slower one once it has been
    /// wanted for `SLOWER_HOLD`.
    pub(crate) fn pace(&mut self, pace: u32, now: Instant) -> Option<Duration> {
        self.pace = pace;
        if self.paused {
            return None;
        }
        let wanted = self.wanted().interval;
        if wanted <= self.applied {
            return self.apply(wanted);
        }
        match self.slower_since {
            Some(since) if now.saturating_duration_since(since) >= SLOWER_HOLD => {
                self.apply(wanted)
            }
            Some(_) => None,
            None => {
                self.slower_since = Some(now);
                None
            }
        }
    }

    /// The display's refresh, read again: a change applies at once.
    pub(crate) fn refresh(&mut self, refresh: Option<f64>) -> Option<Duration> {
        if refresh == self.refresh {
            return None;
        }
        self.refresh = refresh;
        self.settle()
    }

    /// A new frame-rate limit the stream serves without a rebuild: applies
    /// at once.
    pub(crate) fn max_fps(&mut self, max_fps: u32) -> Option<Duration> {
        self.max_fps = max_fps;
        self.settle()
    }

    /// Every viewer paused (the slowest interval allowed), or one resumed
    /// (the pace's), at once.
    pub(crate) fn paused(&mut self, paused: bool) -> Option<Duration> {
        if paused == self.paused {
            return None;
        }
        self.paused = paused;
        self.settle()
    }

    /// What is wanted now, at once.
    fn settle(&mut self) -> Option<Duration> {
        self.apply(self.wanted().interval)
    }

    fn apply(&mut self, interval: Duration) -> Option<Duration> {
        self.slower_since = None;
        (interval != self.applied).then(|| {
            self.applied = interval;
            interval
        })
    }
}

/// A stream's interval changes, handed to ScreenCaptureKit one at a time:
/// one asked for while another is in flight waits for its completion, and
/// only the newest waits.
#[derive(Debug, Default)]
pub(crate) struct Updates {
    /// The change ScreenCaptureKit has yet to complete.
    in_flight: Option<Duration>,
    /// The newest asked for since, unless it is the one in flight.
    next: Option<Duration>,
    /// The stream is stopping: nothing more is handed over.
    stopped: bool,
}

impl Updates {
    /// Ask for `interval`: `Some` when it is to be handed over now.
    pub(crate) fn ask(&mut self, interval: Duration) -> Option<Duration> {
        if self.stopped {
            return None;
        }
        match self.in_flight {
            Some(flying) => {
                self.next = (interval != flying).then_some(interval);
                None
            }
            None => {
                self.in_flight = Some(interval);
                Some(interval)
            }
        }
    }

    /// The change in flight completed, applied or not: the next to hand
    /// over, if one waits.
    pub(crate) fn completed(&mut self) -> Option<Duration> {
        let next = self.next.take().filter(|_| !self.stopped);
        self.in_flight = next;
        next
    }

    /// The stream stops: no change waits, and none is handed over.
    pub(crate) fn stop(&mut self) {
        self.stopped = true;
        self.next = None;
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFRESHES: [f64; 11] = [
        48.0, 59.94, 60.0, 75.0, 90.0, 100.0, 119.88, 120.0, 144.0, 165.0, 240.0,
    ];

    /// The pace's exact multiple where the refresh is one, within 1%; every
    /// refresh otherwise, below eight times the pace; neither beyond the
    /// limit's window; with no refresh, the interval before this rule,
    /// whatever the pace.
    #[test]
    fn the_interval_passes_the_pace_multiple_or_every_refresh() {
        // Refresh, pace, frame-rate limit, every how many refreshes, at the
        // pace.
        for (refresh, target, max_fps, n, paced) in [
            (144.0, 60, 60, 1, false),
            (120.0, 60, 60, 2, true),
            (119.88, 60, 60, 2, true),
            (59.94, 60, 60, 1, true),
            (240.0, 60, 60, 4, true),
            (60.0, 25, 30, 1, false),
            (60.0, 30, 30, 2, true),
            (60.0, 20, 20, 3, true),
            (144.0, 72, 72, 2, true),
            (165.0, 120, 120, 1, false),
            (48.0, 60, 60, 1, false),
            (240.0, 25, 30, 2, false),
            // The pace's multiple beyond the window: as many refreshes as
            // fit in it.
            (60.0, 30, 60, 1, false),
            (144.0, 72, 120, 1, false),
            (240.0, 30, 30, 7, false),
            (60.0, 2, 60, 1, false),
            (240.0, 2, 30, 7, false),
        ] {
            let cadence = cadence(Some(refresh), target, max_fps);
            let at = format!("{refresh} Hz at {target} of {max_fps}");
            assert_eq!(cadence.every, Some(n), "{at}");
            assert_eq!(cadence.paced, paced, "{at}");
            let expected = (f64::from(n) - 0.5) / refresh;
            assert!(
                (cadence.interval.as_secs_f64() - expected).abs() < 2e-9,
                "{at}"
            );
        }
        for target in [2, 15, 30, 60, 120] {
            let fallback = cadence(None, target, 60);
            assert_eq!(fallback.every, None);
            assert!(!fallback.paced);
            assert_eq!(fallback.interval, Duration::from_micros(15_000));
        }
        assert_eq!(
            cadence(None, 60, 120).interval,
            Duration::from_micros(7_500)
        );
        // A refresh no display has is none.
        for unknown in [0.0, 1.0, f64::NAN, f64::INFINITY, 5_000.0] {
            assert_eq!(cadence(Some(unknown), 60, 60).every, None, "{unknown}");
        }
    }

    /// The mode's rate where it reports one, else the display link's.
    #[test]
    fn the_refresh_is_the_modes_or_the_display_links() {
        let link = Some(Duration::from_nanos(6_944_446));
        assert_eq!(refresh_hz(60.0, link), Some(60.0));
        let linked = refresh_hz(0.0, link).unwrap();
        assert!((linked - 144.0).abs() < 1e-3, "{linked}");
        assert!(refresh_hz(1.0, link).is_some_and(|hz| hz > 143.0));
        assert!(refresh_hz(f64::NAN, link).is_some_and(|hz| hz > 143.0));
        assert_eq!(refresh_hz(0.0, None), None);
        assert_eq!(refresh_hz(0.0, Some(Duration::ZERO)), None);
    }

    /// A picture every `every` link periods against the grid: positive when
    /// the display is faster than the pace.
    #[test]
    fn drift_is_positive_for_a_display_ahead_of_the_grid() {
        let period = |hz: f64| Duration::from_secs_f64(hz.recip());
        assert_eq!(drift_ppm(2, period(119.88), 60), Some(-1_000));
        assert_eq!(drift_ppm(1, period(60.006), 60), Some(100));
        assert_eq!(drift_ppm(1, Duration::ZERO, 60), None);
    }

    /// Pictures a strict gate passes from `seconds` of a display at
    /// `refresh` Hz whose refreshes each land up to `jitter` of a period
    /// early or late: each one, unless it came less than `interval` after
    /// the last one passed.
    fn strict_gate(refresh: f64, interval: Duration, seconds: u32, jitter: f64) -> u64 {
        let period = refresh.recip();
        let interval = interval.as_secs_f64();
        let mut state = 0x2545_f491_u32;
        let mut offset = || {
            // A small LCG, the same every run.
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (f64::from(state >> 8) / f64::from(1u32 << 24) - 0.5) * 2.0 * jitter * period
        };
        let frames = (refresh * f64::from(seconds)) as u64;
        let mut last: Option<f64> = None;
        let mut passed = 0;
        for frame in 0..frames {
            let at = frame as f64 * period + offset();
            if last.is_none_or(|last| at - last >= interval) {
                last = Some(at);
                passed += 1;
            }
        }
        passed
    }

    /// On every refresh common on Macs and their displays, for a session
    /// paced at its limit, at every pace from 20 to 120, a strict gate
    /// passes at least the pace (or the refresh, if slower) and, where the
    /// pace's exact multiple fits in the window, the pace itself, with each
    /// refresh up to a fifth of a period early or late. The interval before
    /// this rule passes only every third refresh of 144 Hz there, at 60.
    #[test]
    fn a_strict_gate_passes_the_pace_on_every_refresh() {
        const SECONDS: u32 = 10;
        for refresh in REFRESHES {
            for target in 20..=120u32 {
                let cadence = cadence(Some(refresh), target, target);
                let passed = strict_gate(refresh, cadence.interval, SECONDS, 0.2);
                let rate = passed as f64 / f64::from(SECONDS);
                let floor = f64::from(target).min(refresh) * 0.99;
                assert!(rate >= floor, "{refresh} Hz at {target}: {rate}/s");
                if cadence.paced
                    && refresh.fract() == 0.0
                    && (refresh as u32).is_multiple_of(target)
                {
                    let exact = u64::from(target * SECONDS);
                    assert!(
                        (exact..=exact + 1).contains(&passed),
                        "{refresh} Hz at {target}: {passed}"
                    );
                }
            }
        }
        let before = strict_gate(144.0, window(60), SECONDS, 0.0);
        assert_eq!(before, 144 / 3 * u64::from(SECONDS));
    }

    /// Whatever the pace, unresponsive (2) included, paused or not, no
    /// interval is longer than the limit's window, or half a refresh where
    /// that is longer, and a strict gate passes no fewer pictures than at
    /// the window, so a change waits no longer for its picture than before.
    #[test]
    fn no_interval_holds_a_change_back_longer_than_before() {
        const SECONDS: u32 = 5;
        for refresh in REFRESHES {
            let half = Duration::from_secs_f64(0.5 / refresh);
            for max_fps in [30, 60, 120] {
                let longest = window(max_fps).max(half) + Duration::from_nanos(1);
                let before = strict_gate(refresh, window(max_fps), SECONDS, 0.2);
                let slowest = slowest(Some(refresh), max_fps);
                assert!(slowest.interval <= longest, "{refresh} Hz of {max_fps}");
                for target in std::iter::once(2).chain(10..=max_fps) {
                    let cadence = cadence(Some(refresh), target, max_fps);
                    let at = format!("{refresh} Hz at {target} of {max_fps}");
                    assert!(cadence.interval <= slowest.interval, "{at}");
                    let passed = strict_gate(refresh, cadence.interval, SECONDS, 0.2);
                    assert!(passed >= before, "{at}: {passed} against {before}");
                }
            }
        }
    }

    fn follow(refresh: Option<f64>, pace: u32, max_fps: u32) -> Follow {
        Follow::new(refresh, pace, max_fps)
    }

    fn interval(refresh: f64, every: u32) -> Duration {
        Duration::from_secs_f64((f64::from(every) - 0.5) / refresh)
    }

    /// A faster interval applies at once; a slower one only once it has
    /// been wanted for a second without a break, so that a pace that
    /// changes and comes back changes nothing. On 240 Hz under a limit of
    /// 60, a pace of 60 is every fourth refresh and one of 50 every
    /// refresh.
    #[test]
    fn a_slower_pace_waits_a_second_and_a_faster_one_does_not() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let (every_fourth, every_one) = (interval(240.0, 4), interval(240.0, 1));
        let mut follow = follow(Some(240.0), 60, 60);
        assert_eq!(follow.applied(), every_fourth);
        assert_eq!(follow.pace(60, at(0)), None);
        assert_eq!(follow.pace(50, at(0)), Some(every_one));
        assert_eq!(follow.pace(60, at(10)), None);
        assert_eq!(follow.pace(60, at(500)), None);
        assert_eq!(follow.pace(60, at(1_009)), None);
        assert_eq!(follow.pace(60, at(1_010)), Some(every_fourth));
        // A slower interval wanted for under a second, and back: nothing.
        assert_eq!(follow.pace(50, at(2_000)), Some(every_one));
        assert_eq!(follow.pace(60, at(2_100)), None);
        assert_eq!(follow.pace(50, at(2_600)), None);
        assert_eq!(follow.pace(60, at(2_900)), None);
        assert_eq!(follow.pace(60, at(3_800)), None);
        assert_eq!(follow.pace(60, at(3_900)), Some(every_fourth));
        // A pace that keeps falling slows once, to the latest, and no
        // further than the window.
        assert_eq!(follow.pace(50, at(4_000)), Some(every_one));
        assert_eq!(follow.pace(40, at(4_100)), None);
        assert_eq!(follow.pace(30, at(4_600)), None);
        assert_eq!(follow.pace(2, at(5_100)), Some(every_fourth));
        // Above the frame-rate limit, the limit.
        assert_eq!(follow.pace(240, at(6_000)), None);
        assert_eq!(follow.wanted().every, Some(4));
    }

    /// Every viewer paused gives the slowest interval the window allows at
    /// once, which a new limit or refresh meanwhile changes at once, and a
    /// pace does not; a resume gives the pace's interval at once.
    #[test]
    fn a_pause_slows_to_the_window_and_a_resume_restores_the_pace() {
        let now = Instant::now();
        let mut follow = follow(Some(144.0), 60, 60);
        assert_eq!(follow.applied(), interval(144.0, 1));
        assert_eq!(follow.paused(false), None);
        assert_eq!(follow.paused(true), Some(interval(144.0, 2)));
        assert!(follow.is_paused());
        assert_eq!(follow.paused(true), None);
        assert_eq!(follow.pace(30, now), None);
        assert_eq!(follow.max_fps(30), Some(interval(144.0, 4)));
        assert_eq!(follow.refresh(Some(120.0)), Some(interval(120.0, 4)));
        // At 120 Hz a pace of 30 under a limit of 30 is the same.
        assert_eq!(follow.paused(false), None);
        assert_eq!(follow.applied(), interval(120.0, 4));
        assert_eq!(follow.max_fps(60), Some(interval(120.0, 2)));
        // A resume mid-way through a slower pace's hold applies the pace.
        let mut follow = self::follow(Some(240.0), 50, 60);
        assert_eq!(follow.pace(60, now), None);
        assert_eq!(follow.paused(true), Some(interval(240.0, 4)));
        assert_eq!(follow.paused(false), None);
        assert_eq!(follow.pace(50, now), Some(interval(240.0, 1)));
        // With no refresh, the window, paused or not.
        let mut follow = self::follow(None, 60, 60);
        assert_eq!(follow.paused(true), None);
        assert_eq!(follow.applied(), Duration::from_micros(15_000));
    }

    /// A new refresh or frame-rate limit applies at once, either way; with
    /// no refresh the pace is not followed, and the limit sets the interval.
    #[test]
    fn a_refresh_or_limit_change_applies_at_once() {
        let now = Instant::now();
        let mut follow = follow(Some(120.0), 120, 120);
        assert_eq!(follow.max_fps(60), Some(interval(120.0, 2)));
        assert_eq!(follow.max_fps(60), None);
        assert_eq!(follow.max_fps(120), Some(interval(120.0, 1)));
        assert_eq!(follow.refresh(Some(120.0)), None);
        assert_eq!(follow.refresh(Some(60.0)), Some(interval(60.0, 1)));
        assert_eq!(follow.refresh(None), Some(Duration::from_micros(7_500)));
        assert_eq!(follow.pace(30, now), None);
        assert_eq!(follow.pace(30, now + SLOWER_HOLD * 2), None);
        assert_eq!(follow.max_fps(60), Some(Duration::from_micros(15_000)));
    }

    /// One change in flight at a time; while it is, only the newest asked
    /// for waits, and none if the newest is the one in flight; nothing is
    /// handed over once the stream stops.
    #[test]
    fn interval_changes_go_one_at_a_time_newest_first() {
        let ms = Duration::from_millis;
        let mut updates = Updates::default();
        assert_eq!(updates.ask(ms(10)), Some(ms(10)));
        assert_eq!(updates.ask(ms(20)), None);
        assert_eq!(updates.ask(ms(30)), None);
        assert_eq!(updates.completed(), Some(ms(30)));
        assert_eq!(updates.ask(ms(40)), None);
        assert_eq!(updates.ask(ms(30)), None);
        assert_eq!(updates.completed(), None);
        assert_eq!(updates.ask(ms(50)), Some(ms(50)));
        assert_eq!(updates.ask(ms(60)), None);
        updates.stop();
        assert!(updates.stopped());
        assert_eq!(updates.completed(), None);
        assert_eq!(updates.ask(ms(70)), None);
    }
}
