//! The viewer's fps cap (D8): when the native consumer cannot keep up, the
//! host is asked to send no faster than this viewer presents, 90% of it on a
//! direct path and 80% through the relay (RustDesk `io_loop.rs:1205-1212`).
//!
//! The rule is gated by backlog. Presented throughput equals the host's rate
//! whenever the consumer keeps up, so capping at a share of it unconditionally
//! would ratchet the host down by 10% each time; under backlog, throughput is
//! the consumer's capacity, which is what RustDesk's `decode_fps` measures. A
//! static desktop never lowers the cap.
use crate::{QualityPreset, qos::ceiling_fps};
use std::time::{Duration, Instant};

/// The cap never goes below this.
const CAP_MIN: u32 = 5;
/// A cap is raised by this much at a time.
const CAP_STEP: u32 = 5;
/// The queue must stay short this long before the cap is raised.
const CAP_UP_HOLD: Duration = Duration::from_secs(3);
/// At most one change per this.
const CAP_CHANGE_MIN: Duration = Duration::from_secs(1);
/// More queued units than this on two consecutive ticks lower the cap.
const BACKLOG_HIGH: usize = 2;
/// Up to this many queued units count as keeping up.
const BACKLOG_LOW: usize = 1;

/// Owned by the sink. `tick` runs every viewer tick while the session
/// streams and is not paused, from `IDR_GRACE` after the consumer took its
/// first IDR; otherwise `freeze` keeps the cap as it is.
pub(crate) struct FpsCap {
    /// The session's `limits.max_fps`. The host clears its cap for any value
    /// at or above it, so this is what removing the cap sends: the preset's
    /// ceiling would outlive a switch to a faster preset.
    max_fps: u32,
    cap: Option<u32>,
    last_change: Option<Instant>,
    /// Consecutive ticks with more than `BACKLOG_HIGH` units queued.
    high_ticks: u32,
    /// Since when the queue has held at most `BACKLOG_LOW` units.
    short_since: Option<Instant>,
    /// The sink dropped its queue to the next IDR since the last tick.
    dropped: bool,
}
impl FpsCap {
    pub fn new(max_fps: u32) -> Self {
        Self {
            max_fps,
            cap: None,
            last_change: None,
            high_ticks: 0,
            short_since: None,
            dropped: false,
        }
    }

    pub fn on_backlog_drop(&mut self) {
        self.dropped = true;
    }

    /// Paused, or the host does not stream: nothing is judged, and the hold
    /// toward a raise starts over afterwards.
    pub fn freeze(&mut self) {
        self.high_ticks = 0;
        self.short_since = None;
        self.dropped = false;
    }

    /// The fps to send the host, when the cap changes: lowered under backlog,
    /// at most halved per change; raised by 5 after 3 s without backlog;
    /// removed at the ceiling.
    pub fn tick(
        &mut self,
        now: Instant,
        presented_fps: f32,
        backlog: usize,
        direct: bool,
        quality: QualityPreset,
    ) -> Option<u32> {
        let ceiling = ceiling_fps(quality).min(self.max_fps);
        let dropped = std::mem::take(&mut self.dropped);
        self.high_ticks = if backlog > BACKLOG_HIGH {
            self.high_ticks.saturating_add(1)
        } else {
            0
        };
        if dropped || backlog > BACKLOG_LOW {
            self.short_since = None;
        } else {
            self.short_since.get_or_insert(now);
        }
        let settled = self
            .last_change
            .is_none_or(|at| now.saturating_duration_since(at) >= CAP_CHANGE_MIN);
        let current = self.cap.map_or(ceiling, |cap| cap.min(ceiling));
        if dropped || self.high_ticks >= 2 {
            let factor = if direct { 0.9 } else { 0.8 };
            let lowered = ((presented_fps * factor).floor() as u32)
                .max(CAP_MIN)
                .max(current / 2);
            if lowered >= current || !settled {
                return None;
            }
            self.cap = Some(lowered);
            self.last_change = Some(now);
            return Some(lowered);
        }
        let cap = self.cap?;
        if !settled
            || self
                .short_since
                .is_none_or(|since| now.saturating_duration_since(since) < CAP_UP_HOLD)
        {
            return None;
        }
        self.last_change = Some(now);
        let raised = cap + CAP_STEP;
        if raised >= ceiling {
            self.cap = None;
            Some(self.max_fps)
        } else {
            self.cap = Some(raised);
            Some(raised)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QualityPreset::{Balanced, Quality};

    /// An fps cap on a 250 ms tick, under Balanced (ceiling 60).
    struct Ticks {
        cap: FpsCap,
        now: Instant,
    }
    impl Ticks {
        fn tick(&mut self, fps: f32, backlog: usize, direct: bool) -> Option<u32> {
            self.now += Duration::from_millis(250);
            self.cap.tick(self.now, fps, backlog, direct, Balanced)
        }
        /// `n` ticks that change nothing.
        fn quiet(&mut self, n: usize, fps: f32, backlog: usize, direct: bool) {
            for _ in 0..n {
                assert_eq!(self.tick(fps, backlog, direct), None);
            }
        }
    }

    #[test]
    fn fps_cap_lowers_only_under_backlog_and_probes_up() {
        let start = Instant::now();
        let mut ticks = Ticks {
            cap: FpsCap::new(120),
            now: start,
        };

        // Presentation limited by what the host sends keeps the ceiling, and
        // so does a single tick of backlog.
        ticks.quiet(40, 24.0, 1, true);
        ticks.quiet(1, 24.0, 3, true);
        ticks.quiet(2, 24.0, 1, true);

        // Two ticks of backlog: 90% of presentation on a direct path, but at
        // most half the ceiling at once; then one change a second.
        ticks.quiet(1, 24.0, 3, true);
        assert_eq!(ticks.tick(24.0, 3, true), Some(30));
        ticks.quiet(3, 24.0, 3, true);
        assert_eq!(ticks.tick(24.0, 3, true), Some(21));
        // A drop to the next IDR lowers it with no backlog left: 80% of
        // presentation on the relay.
        ticks.quiet(3, 20.0, 0, false);
        ticks.cap.on_backlog_drop();
        assert_eq!(ticks.tick(20.0, 0, false), Some(16));

        // +5 once the queue stayed short for 3 s, then every second.
        ticks.quiet(12, 16.0, 1, false);
        assert_eq!(ticks.tick(16.0, 1, false), Some(21));
        ticks.quiet(3, 21.0, 0, false);
        assert_eq!(ticks.tick(21.0, 0, false), Some(26));

        // Paused: nothing is judged however long, and the hold starts over.
        for _ in 0..40 {
            ticks.now += Duration::from_millis(250);
            ticks.cap.freeze();
        }
        ticks.quiet(12, 26.0, 0, false);
        assert_eq!(ticks.tick(26.0, 0, false), Some(31));

        // At the ceiling the cap goes, by sending the session's maximum,
        // which clears the host's cap whatever the preset.
        for raised in [36, 41, 46, 51, 56] {
            ticks.quiet(3, 50.0, 0, false);
            assert_eq!(ticks.tick(50.0, 0, false), Some(raised));
        }
        ticks.quiet(3, 56.0, 0, false);
        assert_eq!(ticks.tick(56.0, 0, false), Some(120));
        ticks.quiet(20, 60.0, 0, false);

        // The preset's ceiling is what a cap lowers from: 90% of 40 is above
        // Quality's 30, so nothing changes.
        let mut cap = FpsCap::new(120);
        cap.on_backlog_drop();
        assert_eq!(cap.tick(start, 40.0, 0, true, Quality), None);
    }
}
