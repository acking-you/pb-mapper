//! Capture pacing for thread C, its only owner. Ticks follow the QoS pace,
//! and a tick is consumed whether it fires or is skipped, so a stall never
//! ends in a catch-up burst.
//!
//! A submit whose picture says when the display presented it anchors the
//! next tick on that present, as airhand's pacer does: the next picture may
//! be taken up to a quarter period early, and the timeline moves at most
//! 1/64 period earlier per picture, so a display presenting at the pace, or
//! a little faster, is taken as each present lands instead of waiting for a
//! grid tick, which a faster display's presents drift ahead of until one is
//! dropped. Only a picture of a single present moves it earlier: one
//! covering several presents that waited, from a display faster than the
//! pace, keeps the tick a period after the last, so a display at least
//! twice as fast is taken at the pace itself. A skip, a repeat or an undated
//! picture keeps the even grid.
use std::time::{Duration, Instant};

pub(crate) struct Pacer {
    next: Instant,
    period: Duration,
    last: Option<Instant>,
    /// The last tick took a dated picture: the next may come early.
    anchored: bool,
}
impl Pacer {
    /// The first tick is due at once.
    pub fn new(fps: u32, now: Instant) -> Self {
        Self {
            next: now,
            period: period(fps),
            last: None,
            anchored: false,
        }
    }

    /// A faster pace may bring the next tick forward; a slower one never
    /// delays a tick already scheduled.
    pub fn set_pace(&mut self, fps: u32, now: Instant) {
        self.period = period(fps);
        self.next = self.next.min(self.last.unwrap_or(now) + self.period);
    }

    pub fn due(&self, now: Instant) -> bool {
        now >= self.earliest()
    }

    /// The earliest the next tick may be taken, which bounds thread C's
    /// waits and encoder polls: the tick itself on the grid; after a dated
    /// picture a quarter period before it, but never sooner than three
    /// quarters of a period after the last tick.
    pub fn earliest(&self) -> Instant {
        if !self.anchored {
            return self.next;
        }
        let early = self.next.checked_sub(self.period / 4).unwrap_or(self.next);
        self.last
            .map_or(early, |last| early.max(last + self.period * 3 / 4))
    }

    /// The tick taken at `now` fired or was skipped. `presented` is when the
    /// display presented the picture it submitted, and how many presents
    /// that picture covers (1 when none was skipped): `None` for a skip, a
    /// repeat or an undated picture. A present later than `now` counts as
    /// `now`, and one older than a period as `None`.
    ///
    /// Undated, the grid keeps its long-run rate (a 120 fps pace on a 165 Hz
    /// display is not rounded down to 82.5), and after a stall two ticks are
    /// never closer than three quarters of a period. Dated, the next tick is
    /// a period after the present, but at most 1/64 period earlier than a
    /// period after this one: a present of its own that had to wait for the
    /// early window moves the timeline that much earlier, until the presents
    /// land in it (within 48 pictures), so the long-run rate is at most
    /// 64/63 of the pace. A picture covering several presents that waited
    /// keeps the next tick a period after this one: a display at least twice
    /// as fast as the pace has several waiting at every tick, and is taken at
    /// the pace rather than 1/63 above it. The three-quarter floor still
    /// holds.
    pub fn consume(&mut self, now: Instant, presented: Option<(Instant, u32)>) {
        let presented = presented
            .map(|(at, presents)| (at.min(now), presents))
            .filter(|(at, _)| now.saturating_duration_since(*at) <= self.period);
        match presented {
            Some((at, presents)) => {
                let early = self.next.checked_sub(self.period / 4).unwrap_or(self.next);
                let base = if at >= early || presents <= 1 {
                    let creep = self.next.checked_sub(self.period / 64).unwrap_or(self.next);
                    at.max(creep)
                } else {
                    self.next
                };
                self.next = base + self.period;
                self.anchored = true;
            }
            None => {
                self.next = (self.next + self.period).max(now + self.period * 3 / 4);
                self.anchored = false;
            }
        }
        self.last = Some(now);
    }

    /// The period of the current pace.
    pub fn period(&self) -> Duration {
        self.period
    }
}

fn period(fps: u32) -> Duration {
    Duration::from_secs(1) / fps.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: u64 = 1_000_000_000;

    /// One picture thread C took: the present's index and time, and when it
    /// was taken, in nanoseconds from the run's origin.
    #[derive(Clone, Copy, Debug)]
    struct Take {
        present: u64,
        stamp: u64,
        taken: u64,
    }

    /// Thread C on a source presenting at `hz`, its k-th present `phase`
    /// ns plus k/`hz` s after the run's origin (to the nanosecond), which
    /// wakes it as each present lands: it takes the newest present not yet
    /// taken once the pacer is due, dated with the present's time and the
    /// presents since the last take when `dated`, and its capture call then
    /// keeps it busy for a millisecond.
    fn run(hz: f64, pace: u32, dated: bool, phase: u64, secs: u64) -> Vec<Take> {
        let origin = Instant::now();
        let at = |ns: u64| origin + Duration::from_nanos(ns);
        let ns = |instant: Instant| instant.duration_since(origin).as_nanos() as u64;
        let stamp = |k: u64| phase + (k as f64 * 1e9 / hz).round() as u64;
        let newest = |now: u64| {
            let mut k = ((now - phase) as f64 * hz / 1e9) as u64;
            while stamp(k + 1) <= now {
                k += 1;
            }
            while stamp(k) > now {
                k -= 1;
            }
            k
        };
        let busy = 1_000_000;
        let start = SECOND;
        let mut pacer = Pacer::new(pace, at(start));
        let mut takes: Vec<Take> = Vec::new();
        let mut now = start;
        while now < start + secs * SECOND {
            let k = newest(now);
            let last = takes.last().map(|take| take.present);
            let fresh = last.is_none_or(|last| k > last);
            if fresh && pacer.due(at(now)) {
                takes.push(Take {
                    present: k,
                    stamp: stamp(k),
                    taken: now,
                });
                let presents = last.map_or(1, |last| u32::try_from(k - last).unwrap());
                pacer.consume(at(now), dated.then(|| (at(stamp(k)), presents)));
                now += busy;
            } else if fresh {
                // A picture waits: thread C's deadline is the pacer's.
                now = ns(pacer.earliest());
            } else {
                // Nothing new: the next present wakes it.
                now = stamp(k + 1);
            }
        }
        takes
    }

    /// Phases of a source against the first tick: a tenth, a third, half and
    /// nine tenths of a period of `pace`.
    fn phases(pace: u32) -> [u64; 4] {
        let p = period(pace).as_nanos() as u64;
        [p / 10, p / 3, p / 2, p * 9 / 10]
    }

    /// No two takes closer than three quarters of a period.
    fn floored(takes: &[Take], pace: u32) {
        let floor = period(pace).as_nanos() as u64 * 3 / 4;
        for pair in takes.windows(2) {
            assert!(pair[1].taken - pair[0].taken >= floor, "{pair:?}");
        }
    }

    /// A display presenting at the pace, a little slower or a little
    /// faster, is taken as each present lands, whatever its phase against
    /// the first tick, once the timeline has crept onto its presents
    /// (within 48 pictures, a few more for a faster display), and no present
    /// is skipped meanwhile.
    #[test]
    fn a_dated_source_at_the_pace_is_taken_as_it_presents() {
        for (hz, pace) in [
            (59.94, 60),
            (60.0, 60),
            (60.05, 60),
            (120.0, 120),
            (120.05, 120),
        ] {
            for phase in phases(pace) {
                let takes = run(hz, pace, true, phase, 30);
                floored(&takes, pace);
                // The first picture is the one there at the start.
                for pair in takes[1..].windows(2) {
                    assert_eq!(pair[1].present, pair[0].present + 1, "{hz} Hz: {pair:?}");
                }
                let late = takes[64..]
                    .iter()
                    .map(|take| take.taken - take.stamp)
                    .max()
                    .unwrap();
                assert!(late <= 1_000_000, "{hz} Hz at {pace}: {late} ns");
            }
        }
    }

    /// What an undated grid does with the same display a little faster than
    /// the pace: each present waits longer for its tick, until one is
    /// replaced by the next. Anchoring is what removes both.
    #[test]
    fn an_undated_grid_drifts_behind_a_display_a_little_faster() {
        let takes = run(60.05, 60, false, 0, 30);
        let skipped = takes[1..]
            .windows(2)
            .filter(|pair| pair[1].present > pair[0].present + 1)
            .count();
        let late = takes.iter().map(|take| take.taken - take.stamp).max();
        assert!(skipped >= 1, "{skipped}");
        assert!(late > Some(10_000_000), "{late:?}");
    }

    /// A display faster than the pace is thinned to it: within 2%, and never
    /// above 64/63 of it, give or take the one early window. One at least
    /// twice as fast has a present waiting at every tick, whose picture
    /// covers several, and is taken at the pace itself, within 0.1%.
    #[test]
    fn a_faster_dated_source_is_thinned_to_the_pace() {
        for (hz, pace) in [
            (120.0, 60),
            (144.0, 60),
            (165.0, 60),
            (240.0, 60),
            (144.0, 120),
            (165.0, 120),
            (240.0, 120),
        ] {
            let p = period(pace).as_nanos() as u64;
            for phase in phases(pace) {
                let takes = run(hz, pace, true, phase, 30);
                floored(&takes, pace);
                let intervals = (takes.len() - 1) as u64;
                let span = takes[takes.len() - 1].taken - takes[0].taken;
                let rate = intervals as f64 / (span as f64 / 1e9);
                let fps = f64::from(pace);
                let within = if hz >= 2.0 * fps { 0.001 } else { 0.02 };
                assert!(
                    (rate - fps).abs() <= fps * within,
                    "{hz} Hz at {pace}: {rate}"
                );
                assert!(
                    span + p / 4 >= intervals * (p - p / 64),
                    "{hz} Hz at {pace}"
                );
            }
        }
    }

    /// The pacer before anchoring: what an undated consume must still do.
    struct Grid {
        next: Instant,
        period: Duration,
        last: Option<Instant>,
    }
    impl Grid {
        fn set_pace(&mut self, fps: u32, now: Instant) {
            self.period = period(fps);
            self.next = self.next.min(self.last.unwrap_or(now) + self.period);
        }
        fn consume(&mut self, now: Instant) {
            self.next = (self.next + self.period).max(now + self.period * 3 / 4);
            self.last = Some(now);
        }
    }

    /// Undated consumes, on time, late, after a stall and across pace
    /// changes, lay exactly the grid they always did.
    #[test]
    fn undated_consumes_keep_the_grid() {
        let start = Instant::now();
        let mut pacer = Pacer::new(60, start);
        let mut grid = Grid {
            next: start,
            period: period(60),
            last: None,
        };
        let mut now = start;
        for step in 0..2_000u64 {
            assert_eq!(pacer.earliest(), grid.next, "step {step}");
            assert_eq!(pacer.due(now), now >= grid.next);
            match step % 50 {
                7 => {
                    pacer.set_pace(30, now);
                    grid.set_pace(30, now);
                }
                23 => {
                    pacer.set_pace(120, now);
                    grid.set_pace(120, now);
                }
                41 => now += Duration::from_millis(250),
                _ => {}
            }
            // On the tick, or up to 3 ms late.
            now = now.max(grid.next) + Duration::from_micros(step * 7 % 3_000);
            pacer.consume(now, None);
            grid.consume(now);
        }
    }

    /// A present later than the consume's own time, as one stamped inside the
    /// capture call, counts as that time; one older than a period as none.
    #[test]
    fn a_future_stamp_is_clamped_and_a_stale_one_ignored() {
        let start = Instant::now();
        let pace = 60;
        let p = period(pace);
        let consumed = |presented: Option<Instant>| {
            let mut pacer = Pacer::new(pace, start);
            pacer.consume(start, presented.map(|at| (at, 1)));
            pacer.earliest()
        };
        assert_eq!(
            consumed(Some(start + Duration::from_millis(3))),
            consumed(Some(start))
        );
        // Anchored on the present: a quarter period early is allowed.
        assert_eq!(consumed(Some(start)), start + p - p / 4);
        let stale = start.checked_sub(p + Duration::from_micros(1)).unwrap();
        assert_eq!(consumed(Some(stale)), consumed(None));
        assert_eq!(consumed(None), start + p);
    }

    /// After a stall, dated or not, the next tick is at least three quarters
    /// of a period after the one that ended it.
    #[test]
    fn after_a_stall_the_three_quarter_floor_holds() {
        let start = Instant::now();
        let p = period(60);
        let ages = [None, Some(Duration::ZERO), Some(p / 2), Some(p)];
        for (presented, presents) in ages.into_iter().flat_map(|age| [(age, 1), (age, 120)]) {
            let mut pacer = Pacer::new(60, start);
            pacer.consume(start, Some((start, 1)));
            let resumed = start + Duration::from_secs(2);
            pacer.consume(resumed, presented.map(|ago| (resumed - ago, presents)));
            assert!(pacer.earliest() >= resumed + p * 3 / 4, "{presented:?}");
            assert!(!pacer.due(resumed + p * 3 / 4 - Duration::from_nanos(1)));
        }
    }
}
