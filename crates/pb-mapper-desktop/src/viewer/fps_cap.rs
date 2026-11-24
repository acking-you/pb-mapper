//! The viewer's fps cap (D8): when the native consumer cannot keep up, the
//! host is asked to send no faster than the consumer can take, 90% of it on a
//! direct path and 80% through the relay (RustDesk `io_loop.rs:1205-1212`).
//!
//! The rule is gated by backlog. Presented throughput equals the host's rate
//! whenever the consumer keeps up, so capping at a share of it unconditionally
//! would ratchet the host down by 10% each time; under backlog, throughput is
//! the consumer's capacity, which is what RustDesk's `decode_fps` measures. A
//! static desktop never lowers the cap.
//!
//! The capacity is measured rather than inferred where it can be: a tick in
//! which the consumer took units and never found the queue empty kept it
//! busy, so its pulls over such ticks are what it can take. A lowering
//! remembers the capacity, and raises then stop just below it. Above that a
//! raise is a probe of a tenth of it, watched for `PROBE_WATCH` and taken back
//! at once at the first sign that the consumer falls behind (not while it
//! decodes a fresh IDR, which may be slow at any rate), with a backoff that
//! doubles while probes fail; a pause in the watch takes it back too, as no
//! verdict. A probe that holds well above the capacity shows the consumer got
//! faster, and the capacity is forgotten, as it is when a new generation, a
//! new picture size or a preset change makes it stale. A consumer takes at
//! least one unit per its median turnaround, the time from a hand-off to that
//! unit's presentation: when that is well above the capacity, it replaces it.
//!
//! A consumer that queues inside itself (GStreamer's four units in flight,
//! MediaCodec) hides a backlog from the sink's queue, but not from the time it
//! takes from a hand-off to that unit's presentation: that lag rising well
//! above its own floor lowers the cap too.
use crate::{QualityPreset, qos::ceiling_fps};
use pb_mapper_core::timeout::RetryBackoff;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// The cap never goes below this.
const CAP_MIN: u32 = 5;
/// A raise at a ceiling of `STEP_CEILING` fps, and in proportion to higher
/// ceilings (20 at 120); at least a quarter of the cap.
const CAP_STEP: u32 = 5;
const STEP_CEILING: u32 = 30;
const STEP_DIVISOR: u32 = 4;
/// The queue must stay short this long before the cap is raised.
const CAP_UP_HOLD: Duration = Duration::from_secs(3);
/// At most one change per this, but for taking a failed probe back.
const CAP_CHANGE_MIN: Duration = Duration::from_secs(1);
/// More queued units than this on two consecutive ticks lower the cap.
const BACKLOG_HIGH: usize = 2;
/// Up to this many queued units count as keeping up.
const BACKLOG_LOW: usize = 1;
const _: () = assert!(
    BACKLOG_LOW < BACKLOG_HIGH,
    "keeping up is below falling behind"
);
/// Raises toward a known capacity stop at this share of it.
const CAPACITY_HOLD: f32 = 0.95;
/// A probe raises the cap by this share of the capacity, at least 1.
const PROBE_SHARE: f32 = 0.1;
/// A probe that held at this multiple of the capacity, or above, forgets it;
/// a median turnaround that allows this multiple replaces it.
const CAPACITY_OUTGROWN: f32 = 1.1;
/// A turnaround counts as at least this long: a bound on what it implies.
const TURNAROUND_MIN: Duration = Duration::from_millis(1);
/// How long a probe is watched.
const PROBE_WATCH: Duration = Duration::from_secs(3);
/// After a failed probe the next waits this long, doubling to the maximum; a
/// probe that holds resets it.
const PROBE_BACKOFF_MIN: Duration = Duration::from_secs(3);
const PROBE_BACKOFF_MAX: Duration = Duration::from_secs(60);
/// During a probe's watch, this many queued units with the head late fail it,
/// and so does the consumer kept busy this many ticks in a row.
const PROBE_QUEUED: usize = 2;
const PROBE_BUSY_TICKS: u32 = 4;
/// A wait or a lag is late beyond this many frame intervals at the cap, and
/// never before `LATE_MIN`.
const LATE_FRAMES: u32 = 2;
const LATE_MIN: Duration = Duration::from_millis(40);
/// The consumer's pulls are measured over this many ticks; a capacity needs
/// this many busy ones among them (1 s).
const METER_TICKS: usize = 8;
const METER_BUSY_MIN: usize = 4;
const _: () = assert!(METER_BUSY_MIN <= METER_TICKS, "a measurement is reachable");
const _: () = assert!(
    PROBE_BUSY_TICKS as usize >= METER_BUSY_MIN,
    "a probe that fails for a busy consumer leaves a measured capacity"
);
/// The newest turnarounds kept; a median is taken over `LAG_RECENT`, of at
/// least `LAG_MIN_SAMPLES`, and the floor over `LAG_FLOOR_SECONDS`.
const LAG_SAMPLES: usize = 128;
const LAG_RECENT: Duration = Duration::from_secs(1);
const LAG_MIN_SAMPLES: usize = 3;
const LAG_FLOOR_SECONDS: usize = 10;
const _: () = assert!(
    LAG_SAMPLES as u32 >= crate::video::MAX_FPS,
    "a second of presentations at the top rate is kept"
);

/// What the sink sees of its queue and its consumer at a tick.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Observed {
    /// Presentations in the last second.
    pub presented_fps: f32,
    pub queued: usize,
    /// How long the head has waited; zero when nothing is queued.
    pub head_wait: Duration,
    /// The consumer took an IDR within the sink's `IDR_GRACE`: what queued
    /// behind it may be its slow decode, not the rate.
    pub idr_recent: bool,
}

/// One tick of the meter that kept the consumer busy.
#[derive(Clone, Copy)]
struct BusyTick {
    pulls: u32,
    length: Duration,
}

/// The consumer's pulls in ticks that kept it busy.
#[derive(Default)]
struct Meter {
    /// Since the last tick: units handed out, whether the consumer found the
    /// queue empty, and whether the queue was cleared.
    pulls: u32,
    starved: bool,
    cleared: bool,
    last_tick: Option<Instant>,
    /// The last `METER_TICKS` ticks; `None` for one that did not keep the
    /// consumer busy.
    ticks: [Option<BusyTick>; METER_TICKS],
    next: usize,
}
impl Meter {
    /// Closes the tick; whether it kept the consumer busy.
    fn tick(&mut self, now: Instant) -> bool {
        let busy = self.pulls > 0 && !self.starved && !self.cleared;
        let recorded = self.last_tick.replace(now).map(|last| {
            busy.then(|| BusyTick {
                pulls: self.pulls,
                length: now.saturating_duration_since(last),
            })
        });
        if let Some(tick) = recorded {
            self.ticks[self.next] = tick;
            self.next = (self.next + 1) % METER_TICKS;
        }
        self.pulls = 0;
        self.starved = false;
        self.cleared = false;
        busy && recorded.is_some()
    }

    /// Units per second over the busy ticks, once there are enough of them.
    fn capacity(&self) -> Option<f32> {
        let busy = self.ticks.iter().flatten();
        if busy.clone().count() < METER_BUSY_MIN {
            return None;
        }
        let (pulls, length) = busy.fold((0, Duration::ZERO), |(pulls, length), tick| {
            (pulls + tick.pulls, length + tick.length)
        });
        (!length.is_zero()).then(|| pulls as f32 / length.as_secs_f32())
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The consumer's turnaround: from handing a unit out to its accepted
/// presentation.
struct Lag {
    epoch: Instant,
    /// The newest samples, oldest first.
    recent: VecDeque<(Instant, Duration)>,
    /// The least turnaround of each of the last `LAG_FLOOR_SECONDS` seconds
    /// since `epoch`.
    floors: [Option<(u64, Duration)>; LAG_FLOOR_SECONDS],
}
impl Lag {
    fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            recent: VecDeque::with_capacity(LAG_SAMPLES),
            floors: [None; LAG_FLOOR_SECONDS],
        }
    }

    fn record(&mut self, now: Instant, lag: Duration) {
        if self.recent.len() == LAG_SAMPLES {
            self.recent.pop_front();
        }
        self.recent.push_back((now, lag));
        let second = now.saturating_duration_since(self.epoch).as_secs();
        let slot = &mut self.floors[(second % LAG_FLOOR_SECONDS as u64) as usize];
        *slot = match *slot {
            Some((at, floor)) if at == second => Some((second, floor.min(lag))),
            _ => Some((second, lag)),
        };
    }

    /// The median over the last second, of at least `LAG_MIN_SAMPLES`.
    fn median(&self, now: Instant) -> Option<Duration> {
        let mut values = [Duration::ZERO; LAG_SAMPLES];
        let mut count = 0;
        for (at, lag) in self.recent.iter().rev() {
            if now.saturating_duration_since(*at) > LAG_RECENT {
                break;
            }
            values[count] = *lag;
            count += 1;
        }
        if count < LAG_MIN_SAMPLES {
            return None;
        }
        let middle = count / 2;
        Some(*values[..count].select_nth_unstable(middle).1)
    }

    /// How far the median rose above the floor of the last ten seconds.
    fn excess(&self, now: Instant) -> Option<Duration> {
        let median = self.median(now)?;
        let second = now.saturating_duration_since(self.epoch).as_secs();
        let floor = self
            .floors
            .iter()
            .flatten()
            .filter(|(at, _)| second.saturating_sub(*at) < LAG_FLOOR_SECONDS as u64)
            .map(|(_, floor)| *floor)
            .min()?;
        Some(median.saturating_sub(floor))
    }

    fn reset(&mut self) {
        self.recent.clear();
        self.floors = [None; LAG_FLOOR_SECONDS];
    }
}

/// A raise above the capacity, under watch.
#[derive(Clone, Copy, Debug)]
struct Probe {
    /// The cap it raised.
    from: u32,
    at: Instant,
    /// Ticks in a row that kept the consumer busy.
    busy_ticks: u32,
}

/// Owned by the sink. `tick` runs every viewer tick while the session
/// streams and is not paused, from `IDR_GRACE` after the consumer took its
/// first IDR; otherwise `freeze` keeps the cap as it is, but for a probe
/// under watch, which it takes back.
pub(crate) struct FpsCap {
    /// The session's `limits.max_fps`. The host clears its cap for any value
    /// at or above it, so this is what removing the cap sends: the preset's
    /// ceiling would outlive a switch to a faster preset.
    max_fps: u32,
    /// `max_fps` is the user's own choice, the host's ceiling for every
    /// preset (`fps_override`), and so the cap's.
    fps_override: bool,
    cap: Option<u32>,
    last_change: Option<Instant>,
    /// Consecutive ticks with more than `BACKLOG_HIGH` units queued, and with
    /// the consumer's turnaround late.
    high_ticks: u32,
    lag_ticks: u32,
    /// Since when the queue has held at most `BACKLOG_LOW` units.
    short_since: Option<Instant>,
    /// The sink had a backlog episode or dropped its queue since the last tick.
    backlog: bool,
    /// The preset of the last tick.
    quality: Option<QualityPreset>,
    meter: Meter,
    lag: Lag,
    /// What the consumer could take when the cap was last lowered, in fps.
    capacity: Option<f32>,
    probe: Option<Probe>,
    probe_backoff: RetryBackoff,
    probe_after: Option<Instant>,
}
impl FpsCap {
    pub fn new(epoch: Instant, max_fps: u32, fps_override: bool) -> Self {
        Self {
            max_fps,
            fps_override,
            cap: None,
            last_change: None,
            high_ticks: 0,
            lag_ticks: 0,
            short_since: None,
            backlog: false,
            quality: None,
            meter: Meter::default(),
            lag: Lag::new(epoch),
            capacity: None,
            probe: None,
            probe_backoff: RetryBackoff::new(PROBE_BACKOFF_MIN, PROBE_BACKOFF_MAX),
            probe_after: None,
        }
    }

    /// The sink opened a backlog episode or dropped its queue to the next IDR.
    pub fn on_backlog(&mut self) {
        self.backlog = true;
    }

    /// The consumer took a unit.
    pub fn on_pull(&mut self) {
        self.meter.pulls = self.meter.pulls.saturating_add(1);
    }

    /// The consumer asked for a unit and found the queue empty.
    pub fn on_starved(&mut self) {
        self.meter.starved = true;
    }

    /// The sink dropped what was queued.
    pub fn on_cleared(&mut self) {
        self.meter.cleared = true;
    }

    /// An accepted presentation, `lag` after its unit was handed out.
    pub fn on_presented(&mut self, now: Instant, lag: Duration) {
        self.lag.record(now, lag);
    }

    /// A new generation or picture size: what the consumer took per unit no
    /// longer applies.
    pub fn forget(&mut self) {
        self.capacity = None;
        self.probe = None;
        self.probe_backoff.reset();
        self.probe_after = None;
        self.meter.reset();
        self.lag.reset();
        self.lag_ticks = 0;
    }

    /// Paused, or the host does not stream: nothing is judged, and the hold
    /// toward a raise starts over afterwards. A probe under watch is taken
    /// back, without a verdict: the cap to send then, so the host never
    /// stays above what the consumer was seen to take.
    /// The session moved to another host session, which knows no cap: the
    /// cap in force, a probe under watch taken back, to send it again.
    pub fn resend(&mut self, now: Instant) -> Option<u32> {
        self.freeze(now);
        self.cap
    }

    pub fn freeze(&mut self, now: Instant) -> Option<u32> {
        self.high_ticks = 0;
        self.lag_ticks = 0;
        self.short_since = None;
        self.backlog = false;
        self.meter.reset();
        let probe = self.probe.take()?;
        self.cap = Some(probe.from);
        self.last_change = Some(now);
        Some(probe.from)
    }

    /// The fps to send the host, when the cap changes: lowered under backlog
    /// to a share of what the consumer can take, at most halved per change
    /// when that is only what it presented; raised after 3 s without backlog,
    /// in steps scaled to the ceiling up to just below a known capacity and
    /// by watched probes above it; removed at the ceiling.
    pub fn tick(
        &mut self,
        now: Instant,
        observed: Observed,
        direct: bool,
        quality: QualityPreset,
    ) -> Option<u32> {
        let ceiling = if self.fps_override {
            self.max_fps
        } else {
            ceiling_fps(quality).min(self.max_fps)
        };
        if self
            .quality
            .replace(quality)
            .is_some_and(|before| before != quality)
        {
            self.forget();
        }
        let backlog = std::mem::take(&mut self.backlog);
        let busy = self.meter.tick(now);
        let current = self.cap.map_or(ceiling, |cap| cap.min(ceiling));
        let late = late(current);
        let lagging = self.lag.excess(now).is_some_and(|excess| excess > late);
        self.high_ticks = if observed.queued > BACKLOG_HIGH {
            self.high_ticks.saturating_add(1)
        } else {
            0
        };
        self.lag_ticks = if lagging {
            self.lag_ticks.saturating_add(1)
        } else {
            0
        };
        if backlog || lagging || observed.queued > BACKLOG_LOW {
            self.short_since = None;
        } else {
            self.short_since.get_or_insert(now);
        }
        let settled = self
            .last_change
            .is_none_or(|at| now.saturating_duration_since(at) >= CAP_CHANGE_MIN);

        // A consumer that turns a unit around in its median time takes at
        // least one per that time: well above the remembered capacity, that
        // is the capacity now, a decoder that got faster or one only its
        // presentations had measured. Raises below it need no probe; the
        // probes above it keep their backoff, which only a probe that held
        // resets.
        if let Some(capacity) = self.capacity
            && let Some(median) = self.lag.median(now)
        {
            let turnaround = 1.0 / median.max(TURNAROUND_MIN).as_secs_f32();
            if turnaround >= capacity * CAPACITY_OUTGROWN {
                self.capacity = Some(turnaround);
            }
        }

        if let Some(probe) = &mut self.probe {
            // A slow IDR decode holds the queue and keeps the consumer busy
            // at any rate: those ticks are no verdict on the probe.
            if !observed.idr_recent {
                probe.busy_ticks = if busy { probe.busy_ticks + 1 } else { 0 };
            }
            let failed = backlog
                || lagging
                || probe.busy_ticks >= PROBE_BUSY_TICKS
                || (!observed.idr_recent
                    && observed.queued >= PROBE_QUEUED
                    && observed.head_wait > late);
            if failed {
                // Back at once, whenever the probe was.
                let from = probe.from;
                self.probe = None;
                self.probe_failed(now);
                if let Some(measured) = self.meter.capacity() {
                    self.capacity = Some(measured);
                }
                self.short_since = None;
                self.cap = Some(from);
                self.last_change = Some(now);
                return Some(from);
            }
            if now.saturating_duration_since(probe.at) >= PROBE_WATCH {
                self.probe = None;
                self.probe_backoff.reset();
                self.probe_after = None;
                if self
                    .capacity
                    .is_some_and(|capacity| current as f32 >= capacity * CAPACITY_OUTGROWN)
                {
                    self.capacity = None;
                }
            }
        }

        if backlog || self.high_ticks >= 2 || self.lag_ticks >= 2 {
            let measured = self.meter.capacity();
            let capacity = measured.unwrap_or(observed.presented_fps);
            let factor = if direct { 0.9 } else { 0.8 };
            let mut lowered = ((capacity * factor).floor() as u32).max(CAP_MIN);
            if measured.is_none() {
                // Only what it presented: at most halve at once.
                lowered = lowered.max(current / 2);
            }
            if lowered >= current || !settled {
                return None;
            }
            if self.probe.take().is_some() {
                self.probe_failed(now);
            }
            self.capacity = (capacity > 0.0).then_some(capacity);
            self.cap = Some(lowered);
            self.last_change = Some(now);
            return Some(lowered);
        }

        let cap = self.cap?;
        if self.probe.is_some()
            || !settled
            || self
                .short_since
                .is_none_or(|since| now.saturating_duration_since(since) < CAP_UP_HOLD)
        {
            return None;
        }
        let raised = match self.capacity {
            Some(capacity) if cap >= hold(capacity) => {
                if self.probe_after.is_some_and(|after| now < after) {
                    return None;
                }
                self.probe = Some(Probe {
                    from: cap,
                    at: now,
                    busy_ticks: 0,
                });
                cap.saturating_add(probe_step(capacity))
            }
            capacity => {
                let raised = cap.saturating_add(raise_step(ceiling, cap));
                capacity.map_or(raised, |capacity| raised.min(hold(capacity)))
            }
        };
        self.last_change = Some(now);
        if raised >= ceiling {
            self.cap = None;
            Some(self.max_fps)
        } else {
            self.cap = Some(raised);
            Some(raised)
        }
    }

    fn probe_failed(&mut self, now: Instant) {
        self.probe_after = Some(now + self.probe_backoff.next_delay());
    }
}

/// A queue wait or a turnaround past two frame intervals at `cap`, and past
/// `LATE_MIN`, is late.
fn late(cap: u32) -> Duration {
    (Duration::from_secs(u64::from(LATE_FRAMES)) / cap.max(1)).max(LATE_MIN)
}

/// Raises toward a known `capacity` stop here.
fn hold(capacity: f32) -> u32 {
    (capacity * CAPACITY_HOLD).floor() as u32
}

/// A raise far from any known capacity: `CAP_STEP` at a 30 fps ceiling, in
/// proportion above it, and at least a quarter of the cap.
fn raise_step(ceiling: u32, cap: u32) -> u32 {
    let scaled = CAP_STEP * ((ceiling + STEP_CEILING / 2) / STEP_CEILING).max(1);
    scaled.max(cap.div_ceil(STEP_DIVISOR))
}

/// A probe above a known `capacity`.
fn probe_step(capacity: f32) -> u32 {
    ((capacity * PROBE_SHARE).ceil() as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QualityPreset::{Balanced, Quality};

    /// An fps cap on a 250 ms tick, and the consumer the sink reports to it.
    struct Ticks {
        cap: FpsCap,
        now: Instant,
        quality: QualityPreset,
    }
    impl Ticks {
        /// Under Balanced: a ceiling of 60, or `max_fps` with `fps_override`.
        fn new(max_fps: u32, fps_override: bool) -> Self {
            let now = Instant::now();
            Self {
                cap: FpsCap::new(now, max_fps, fps_override),
                now,
                quality: Balanced,
            }
        }
        fn observe(&mut self, observed: Observed, direct: bool) -> Option<u32> {
            self.now += Duration::from_millis(250);
            self.cap.tick(self.now, observed, direct, self.quality)
        }
        fn tick(&mut self, fps: f32, queued: usize, direct: bool) -> Option<u32> {
            let observed = Observed {
                presented_fps: fps,
                queued,
                head_wait: Duration::ZERO,
                idr_recent: false,
            };
            self.observe(observed, direct)
        }
        /// What the consumer did before the next tick: took `pulls` units,
        /// without ever finding the queue empty (busy) or not.
        fn consumer(&mut self, pulls: u32, busy: bool) {
            for _ in 0..pulls {
                self.cap.on_pull();
            }
            if !busy {
                self.cap.on_starved();
            }
        }
        /// `n` ticks that change nothing.
        fn quiet(&mut self, n: usize, fps: f32, queued: usize, direct: bool) {
            for _ in 0..n {
                assert_eq!(self.tick(fps, queued, direct), None);
            }
        }
        /// `n` ticks on the relay of a consumer taking `pulls` a tick that
        /// change nothing.
        fn quiet_consumer(&mut self, n: usize, pulls: u32, busy: bool, queued: usize) {
            for _ in 0..n {
                self.consumer(pulls, busy);
                assert_eq!(self.tick(pulls as f32 * 4.0, queued, false), None);
            }
        }
        /// `per_tick` presentations before the next tick, spread over it,
        /// each `lag` after its hand-off.
        fn presented(&mut self, per_tick: u32, lag: Duration) {
            let spacing = Duration::from_millis(250) / per_tick;
            for n in 1..=per_tick {
                self.cap
                    .on_presented(self.now + spacing * n - spacing / 2, lag);
            }
        }
        /// A relayed session whose consumer, busy behind a queue, took 16
        /// units a second: the cap is lowered to 80% of that at once, not
        /// halved from the ceiling.
        fn measured_16() -> Self {
            let mut ticks = Self::new(60, false);
            ticks.quiet_consumer(1, 4, false, 0);
            ticks.quiet_consumer(3, 4, true, 2);
            ticks.quiet_consumer(1, 4, true, 3);
            ticks.consumer(4, true);
            assert_eq!(ticks.tick(16.0, 3, false), Some(12));
            ticks
        }
    }

    /// Far from any measured capacity, as before: lowered only under
    /// backlog, to a share of what was presented and at most halved at once;
    /// raised once the queue stayed short for 3 s, one change a second, now
    /// in steps scaled to the ceiling.
    #[test]
    fn fps_cap_lowers_only_under_backlog_and_raises_in_scaled_steps() {
        let mut ticks = Ticks::new(120, false);

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
        // A backlog episode lowers it with no backlog left: 80% of
        // presentation on the relay.
        ticks.quiet(3, 20.0, 0, false);
        ticks.cap.on_backlog();
        assert_eq!(ticks.tick(20.0, 0, false), Some(16));

        // A new picture size forgets the 20 fps it presented, so raises are
        // far from any capacity: +10 at a ceiling of 60 once the queue stayed
        // short for 3 s, then every second, and a quarter of the cap once
        // that is more.
        ticks.cap.forget();
        ticks.quiet(12, 16.0, 1, false);
        assert_eq!(ticks.tick(16.0, 1, false), Some(26));
        ticks.quiet(3, 26.0, 0, false);
        assert_eq!(ticks.tick(26.0, 0, false), Some(36));

        // Paused: nothing is judged however long, and the hold starts over.
        for _ in 0..40 {
            ticks.now += Duration::from_millis(250);
            assert_eq!(ticks.cap.freeze(ticks.now), None);
        }
        ticks.quiet(12, 36.0, 0, false);
        assert_eq!(ticks.tick(36.0, 0, false), Some(46));
        ticks.quiet(3, 46.0, 0, false);
        assert_eq!(ticks.tick(46.0, 0, false), Some(58));

        // At the ceiling the cap goes, by sending the session's maximum,
        // which clears the host's cap whatever the preset.
        ticks.quiet(3, 58.0, 0, false);
        assert_eq!(ticks.tick(58.0, 0, false), Some(120));
        ticks.quiet(20, 60.0, 0, false);

        // The preset's ceiling is what a cap lowers from: 90% of 40 is above
        // Quality's 30, so nothing changes.
        let start = Instant::now();
        let observed = Observed {
            presented_fps: 40.0,
            queued: 0,
            head_wait: Duration::ZERO,
            idr_recent: false,
        };
        let mut cap = FpsCap::new(start, 120, false);
        cap.on_backlog();
        assert_eq!(cap.tick(start, observed, true, Quality), None);
        // Unless the user chose the session's rate, which is every preset's
        // ceiling then.
        let mut cap = FpsCap::new(start, 120, true);
        cap.on_backlog();
        assert_eq!(cap.tick(start, observed, true, Quality), Some(60));
    }

    /// The step scales with the ceiling: a 120 fps session halved to 60 is
    /// back at the ceiling 3 s and three changes later, not after twelve.
    #[test]
    fn the_raise_step_scales_with_the_ceiling() {
        let mut ticks = Ticks::new(120, true);
        ticks.quiet(4, 100.0, 1, true);
        ticks.cap.on_backlog();
        assert_eq!(ticks.tick(50.0, 0, true), Some(60));
        // A preset change, like a new generation, forgets what it presented.
        ticks.quality = Quality;
        ticks.quiet(12, 60.0, 0, true);
        for raised in [80, 100] {
            assert_eq!(ticks.tick(60.0, 0, true), Some(raised));
            ticks.quiet(3, 60.0, 0, true);
        }
        assert_eq!(ticks.tick(100.0, 0, true), Some(120));

        assert_eq!(
            [30, 45, 60, 120].map(|ceiling| raise_step(ceiling, 10)),
            [5, 10, 10, 20]
        );
        // At least a quarter of the cap.
        assert_eq!(raise_step(60, 50), 13);
    }

    /// Raises stop at 95% of a measured capacity; above that only probes of
    /// a tenth of it go. A probe the consumer cannot take is taken back at
    /// once, within a second of it, and the next waits 3, 6, 12, then 24 s;
    /// one it takes for its 3 s watch resets that.
    #[test]
    fn probes_above_the_capacity_back_off_and_are_taken_back_at_once() {
        let mut ticks = Ticks::measured_16();

        // To 95% of 16 after 3 s of a short queue.
        ticks.quiet_consumer(12, 3, false, 0);
        ticks.consumer(3, false);
        assert_eq!(ticks.tick(12.0, 0, false), Some(15));
        ticks.quiet_consumer(3, 4, false, 0);
        // A probe: a tenth of the capacity above.
        ticks.consumer(4, false);
        assert_eq!(ticks.tick(15.0, 0, false), Some(17));
        // The consumer is busy for four ticks: back to 15 at once.
        ticks.quiet_consumer(3, 4, true, 1);
        ticks.consumer(4, true);
        assert_eq!(ticks.tick(16.0, 1, false), Some(15));

        // The next probe waits for the hold (3 s after the queue is short
        // again) and for the backoff, which doubles with each failure.
        let mut gaps = Vec::new();
        let mut since_failure = 0;
        while gaps.len() < 4 {
            since_failure += 1;
            assert!(since_failure < 1000, "no probe after {gaps:?}");
            ticks.consumer(4, false);
            if let Some(cap) = ticks.tick(15.0, 0, false) {
                assert_eq!(cap, 17, "only probes");
                gaps.push(since_failure);
                ticks.quiet_consumer(3, 4, true, 1);
                ticks.consumer(4, true);
                assert_eq!(ticks.tick(16.0, 1, false), Some(15));
                since_failure = 0;
            }
        }
        // In ticks of 250 ms: the hold, then 6, 12 and 24 s.
        assert_eq!(gaps, [13, 24, 48, 96]);

        // Two queued with the head waiting longer than two frame intervals
        // at the probe's cap fail it in one tick, too; the backoff is 48 s.
        ticks.quiet_consumer(191, 4, false, 0);
        ticks.consumer(4, false);
        assert_eq!(ticks.tick(15.0, 0, false), Some(17));
        ticks.consumer(4, false);
        let late = Observed {
            presented_fps: 16.0,
            queued: 2,
            head_wait: Duration::from_millis(150),
            idr_recent: false,
        };
        assert_eq!(ticks.observe(late, false), Some(15));

        // A probe the consumer takes for its 3 s watch resets the backoff,
        // and the next follows at once. The backoff after a failure is 3 s
        // again.
        ticks.quiet_consumer(239, 4, false, 0);
        ticks.consumer(4, false);
        assert_eq!(ticks.tick(15.0, 0, false), Some(17));
        ticks.quiet_consumer(11, 4, false, 0);
        ticks.consumer(4, false);
        assert_eq!(ticks.tick(17.0, 0, false), Some(19));
        ticks.quiet_consumer(3, 4, true, 1);
        ticks.consumer(4, true);
        assert_eq!(ticks.tick(16.0, 1, false), Some(17));
        ticks.quiet_consumer(12, 4, false, 0);
        ticks.consumer(4, false);
        assert_eq!(ticks.tick(17.0, 0, false), Some(19));
    }

    /// A slow IDR decode is no verdict on a probe: while the consumer took
    /// one within the sink's grace, the queue behind it and a busy consumer do
    /// not take the probe back; past the grace they do. A pause during the
    /// watch takes the probe back at once without counting it as failed, so
    /// the host does not stay above the 15 the consumer was seen to take.
    #[test]
    fn a_probe_is_spared_an_idr_decode_and_taken_back_by_a_pause() {
        /// Raised to 95% of the 16 measured, then probed to 17.
        fn probing() -> Ticks {
            let mut ticks = Ticks::measured_16();
            ticks.quiet_consumer(12, 3, false, 0);
            ticks.consumer(3, false);
            assert_eq!(ticks.tick(12.0, 0, false), Some(15));
            ticks.quiet_consumer(3, 4, false, 0);
            ticks.consumer(4, false);
            assert_eq!(ticks.tick(15.0, 0, false), Some(17));
            ticks
        }

        // Two queued behind an IDR the consumer is still decoding, the head
        // late and the consumer busy: the probe stands, and only the same
        // queue past the grace takes it back.
        let mut ticks = probing();
        let decoding = Observed {
            presented_fps: 16.0,
            queued: 2,
            head_wait: Duration::from_millis(150),
            idr_recent: true,
        };
        for _ in 0..2 {
            ticks.consumer(4, true);
            assert_eq!(ticks.observe(decoding, false), None);
        }
        ticks.consumer(4, true);
        let late = Observed {
            idr_recent: false,
            ..decoding
        };
        assert_eq!(ticks.observe(late, false), Some(15));

        // A pause in the watch: back to 15 at once, and nothing judged while
        // it lasts.
        let mut ticks = probing();
        ticks.now += Duration::from_millis(250);
        assert_eq!(ticks.cap.freeze(ticks.now), Some(15));
        for _ in 0..8 {
            ticks.now += Duration::from_millis(250);
            assert_eq!(ticks.cap.freeze(ticks.now), None);
        }
        // No verdict, so no backoff: the next probe waits only for the hold.
        ticks.quiet_consumer(12, 4, false, 0);
        ticks.consumer(4, false);
        assert_eq!(ticks.tick(15.0, 0, false), Some(17));
    }

    /// A capacity measured while the consumer was briefly slow does not hold
    /// the cap down: once it is fast, the probe after one backoff holds, the
    /// next, at 1.1 times the capacity, forgets it, and raises go on in full
    /// steps. Where the consumer's turnaround shows it is fast, the capacity
    /// goes at once.
    #[test]
    fn a_transient_low_capacity_recovers_within_one_backoff() {
        /// Busy at 8 a second: lowered to 6 on the relay.
        fn slow() -> Ticks {
            let mut ticks = Ticks::new(60, false);
            ticks.quiet_consumer(1, 2, false, 0);
            ticks.quiet_consumer(3, 2, true, 2);
            ticks.quiet_consumer(1, 2, true, 3);
            ticks.consumer(2, true);
            assert_eq!(ticks.tick(8.0, 3, false), Some(6));
            ticks
        }
        let mut ticks = slow();
        // Still slow at the first probe, which fails.
        ticks.quiet_consumer(12, 2, false, 0);
        ticks.consumer(2, false);
        assert_eq!(ticks.tick(8.0, 0, false), Some(7));
        ticks.quiet_consumer(3, 2, false, 0);
        ticks.consumer(2, false);
        assert_eq!(ticks.tick(8.0, 0, false), Some(8));
        ticks.quiet_consumer(3, 2, true, 1);
        ticks.consumer(2, true);
        assert_eq!(ticks.tick(8.0, 1, false), Some(7));

        // Fast from here, with no turnaround to tell.
        let mut caps = Vec::new();
        for tick in 1..=60 {
            ticks.consumer(1, false);
            if let Some(cap) = ticks.tick(7.0, 0, false) {
                caps.push((tick, cap));
            }
        }
        assert_eq!(
            caps,
            [
                (13, 8),
                (25, 9),
                (37, 19),
                (41, 29),
                (45, 39),
                (49, 49),
                (53, 60)
            ]
        );

        // Presentations 5 ms after their hand-off show a consumer that could
        // take far more than 8 a second.
        let mut ticks = slow();
        ticks.presented(4, Duration::from_millis(5));
        ticks.quiet_consumer(12, 1, false, 0);
        ticks.consumer(1, false);
        assert_eq!(ticks.tick(6.0, 0, false), Some(16));
    }

    /// What only presentations measured gives way to what the turnaround
    /// shows: a consumer that presents each unit 13 ms after its hand-off
    /// takes about 77 a second, so the cap rises to 95% of that and probes
    /// above it, instead of probing from 95% of the 50 it presented, or
    /// jumping a full step past it.
    #[test]
    fn the_turnaround_raises_a_capacity_presentations_underestimated() {
        let mut ticks = Ticks::new(120, true);
        ticks.quiet(4, 100.0, 1, true);
        ticks.cap.on_backlog();
        assert_eq!(ticks.tick(50.0, 0, true), Some(60));
        let mut caps = Vec::new();
        for tick in 1..=24 {
            ticks.presented(16, Duration::from_millis(13));
            if let Some(cap) = ticks.tick(64.0, 0, true) {
                caps.push((tick, cap));
            }
        }
        assert_eq!(caps, [(13, 73), (17, 81)]);
    }

    /// A consumer that queues inside itself shows a backlog only in its
    /// turnaround: risen more than two frame intervals above its floor on two
    /// ticks, it lowers the cap; on one tick of a probe's watch, it takes the
    /// probe back.
    #[test]
    fn consumer_lag_lowers_the_cap() {
        let mut ticks = Ticks::new(60, false);
        for _ in 0..8 {
            ticks.presented(4, Duration::from_millis(67));
            ticks.quiet(1, 16.0, 0, false);
        }
        // Four units held inside: the median over a second rises after two
        // ticks of them, and only its second late tick lowers the cap, from
        // what was presented and at most by half.
        for _ in 0..2 {
            ticks.presented(4, Duration::from_millis(268));
            ticks.quiet(1, 16.0, 0, false);
        }
        ticks.presented(4, Duration::from_millis(268));
        assert_eq!(ticks.tick(16.0, 0, false), Some(30));
        for _ in 0..3 {
            ticks.presented(4, Duration::from_millis(268));
            ticks.quiet(1, 16.0, 0, false);
        }
        ticks.presented(4, Duration::from_millis(268));
        assert_eq!(ticks.tick(16.0, 0, false), Some(15));

        // Back at its floor, the hold runs, and the cap, already at 95% of
        // the 16 presented, rises only by probes.
        let mut caps = Vec::new();
        for tick in 1..=30 {
            ticks.presented(4, Duration::from_millis(67));
            if let Some(cap) = ticks.tick(16.0, 0, false) {
                caps.push((tick, cap));
            }
        }
        assert_eq!(caps[..2], [(15, 17), (27, 19)]);

        // A probe whose units wait inside: taken back on its first late tick.
        let mut ticks = Ticks::measured_16();
        let mut probed = false;
        for _ in 0..40 {
            ticks.presented(3, Duration::from_millis(67));
            ticks.consumer(3, false);
            if ticks.tick(12.0, 0, false) == Some(17) {
                probed = true;
                break;
            }
        }
        assert!(probed);
        ticks.presented(4, Duration::from_millis(200));
        ticks.consumer(4, false);
        assert_eq!(ticks.tick(16.0, 0, false), None);
        ticks.presented(4, Duration::from_millis(200));
        ticks.consumer(4, false);
        assert_eq!(ticks.tick(16.0, 0, false), Some(15));
    }
}
