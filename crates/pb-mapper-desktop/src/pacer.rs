//! Capture pacing for thread C, its only owner. Ticks sit on an even grid at
//! the QoS pace, and a tick is consumed whether it fires or is skipped, so a
//! stall never ends in a catch-up burst.
use std::time::{Duration, Instant};

pub(crate) struct Pacer {
    next: Instant,
    period: Duration,
    last: Option<Instant>,
}
impl Pacer {
    /// The first tick is due at once.
    pub fn new(fps: u32, now: Instant) -> Self {
        Self {
            next: now,
            period: period(fps),
            last: None,
        }
    }

    /// A faster pace may bring the next tick forward; a slower one never
    /// delays a tick already scheduled.
    pub fn set_pace(&mut self, fps: u32, now: Instant) {
        self.period = period(fps);
        self.next = self.next.min(self.last.unwrap_or(now) + self.period);
    }

    pub fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// The tick fired or was skipped. The grid keeps its long-run rate (a
    /// 120 fps pace on a 165 Hz display is not rounded down to 82.5), and
    /// after a stall two ticks are never closer than three quarters of a
    /// period.
    pub fn consume(&mut self, now: Instant) {
        self.next = (self.next + self.period).max(now + self.period * 3 / 4);
        self.last = Some(now);
    }

    /// The next tick, which bounds thread C's encoder polls.
    pub fn next(&self) -> Instant {
        self.next
    }
}

fn period(fps: u32) -> Duration {
    Duration::from_secs(1) / fps.max(1)
}
