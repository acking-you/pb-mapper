//! Thread C's pace cap at its encoder's throughput. An encoder slower than
//! the pace that still takes input, as a Media Foundation encoder serial at
//! its bottleneck does, fills to `ENCODER_DEPTH` units, each waiting behind
//! the others: a unit's dwell grows to that many service times, and the ticks
//! the full encoder skips leave the frames unevenly spaced. Capping the pace
//! a little below what the encoder delivers keeps one or two units inside, at
//! one or two service times, evenly spaced. A pipelined encoder whose
//! latency, not its throughput, filled the queue gains no shorter dwell from
//! the cap, and the guard lifts it again.
//!
//! The cap holds what it measured. It rises on a dwell that shows the encoder
//! faster, short of where it last filled the encoder unless the dwell shows
//! it faster than that too, and probes upward only after a hold that doubles
//! each time the encoder filled. A second that does not load the encoder at
//! the cap, as on a still desktop, and a pace asked at or below the cap, as
//! in a viewer's startup hold, leave it as it is. A rebuilt encoder starts
//! over.
//!
//! Thread C paces within the cap itself, and publishes it to each viewer
//! slot (`SessionShared::encoder_cap`), whose QoS takes it into the pace it
//! expects, cuts from and reports in `Stats`.

use super::{Backend, Clock, ENCODER_DEPTH, Engine, window::WindowStats};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

/// The units a full encoder holds beyond the one it serves.
const BACKLOG: f64 = (ENCODER_DEPTH - 1) as f64;
/// A second is full when thread C skipped a due tick for an encoder holding
/// `ENCODER_DEPTH` units, and the encoder held at least this many units on
/// average over it (its units a second times their median dwell, by Little's
/// law), so a unit waited behind two others or more. However close the
/// encoder is to the pace, it then skips a tick now and then; a heavy picture
/// skips ticks while most units wait behind none, and a stall returns too
/// few units to hold this many: neither is full.
const OCCUPANCY_FULL: f64 = BACKLOG;
/// The cap's share of the units the full encoder returned a second.
const CAP_SHARE: f64 = 0.95;
/// A second loads a cap when the encoder returned at least this share of it.
/// Only such a second shows whether the encoder keeps up at the cap: one on a
/// still desktop moves nothing.
const LOAD_SHARE: f64 = 0.9;
/// Loaded seconds, none full, since the last full one before the cap rises:
/// to `CAP_SHARE` ÷ the second's median dwell, when that is a step (a
/// twentieth, at least 1) more; or, once `probe_after` passed, by a step at
/// least, a probe. It clears once it reaches the pace the viewers ask for.
const QUIET_SECONDS: u32 = 3;
const RAISE_DIVISOR: u32 = 20;
/// How long after a measurement (the encoder filled, or its dwell raised the
/// cap) no probe raises the cap. It doubles each time the encoder fills
/// again, up to `PROBE_HOLD_MAX`, so a probe past an encoder's throughput
/// costs the seconds it fills the encoder ever more rarely.
const PROBE_HOLD: Duration = Duration::from_secs(30);
const PROBE_HOLD_MAX: Duration = Duration::from_secs(240);
/// The guard judges a new cap by the first loaded second at least this many
/// rolls after it, more while the backlog drains at the units a second the
/// cap leaves spare, at most `VERIFY_ROLLS_MAX`: a slow encoder capped at
/// 95% of 20 a second drains its three in 3 s.
const VERIFY_ROLLS: u32 = 2;
const VERIFY_ROLLS_MAX: u32 = 8;
/// A cap that did not bring the dwell below this share of what it was lifts
/// again: latency, not throughput, filled the encoder.
const DWELL_SHARE: f64 = 0.6;
/// How long no cap is set after the guard lifted one, doubling with each
/// lift in a row up to `LIFTED_HOLD_MAX`, so a pipelined encoder is capped
/// for two seconds ever more rarely.
const LIFTED_HOLD: Duration = Duration::from_secs(30);
const LIFTED_HOLD_MAX: Duration = Duration::from_secs(480);

const _: () = assert!(
    CAP_SHARE < 1.0 && LOAD_SHARE < 1.0 && DWELL_SHARE < 1.0,
    "the cap sits below the encoder's throughput, a second at the cap loads \
     it, and the cap must shorten the dwell"
);
const _: () = assert!(
    VERIFY_ROLLS <= VERIFY_ROLLS_MAX
        && PROBE_HOLD.as_secs() <= PROBE_HOLD_MAX.as_secs()
        && LIFTED_HOLD.as_secs() <= LIFTED_HOLD_MAX.as_secs(),
    "each bound is at least where it starts"
);

/// What a rolled second did to the cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    /// The encoder was full: the pace is capped at what it delivered.
    Capped,
    /// The guard found the cap shortened the dwell, and keeps it.
    Kept,
    /// The guard found the dwell no shorter, and lifted the cap for a hold.
    Lifted,
    /// A capped encoder was full again, at a raised cap or a heavier
    /// picture: the cap falls to what it delivered.
    Lowered,
    /// Quiet seconds raised the cap, by the dwell or a probe.
    Raised,
    /// The cap reached the pace the viewers ask for.
    Cleared,
    /// Thread C rebuilt the encoder: what was measured was the old one's.
    Reset,
}
impl Action {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Capped => "capped",
            Self::Kept => "kept",
            Self::Lifted => "lifted",
            Self::Lowered => "lowered",
            Self::Raised => "raised",
            Self::Cleared => "cleared",
            Self::Reset => "reset",
        }
    }
}

/// A change of the cap, with the dwells the guard compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Change {
    pub action: Action,
    pub cap: Option<u32>,
    /// The dwell p50 of the second that capped.
    pub dwell_before: Option<Duration>,
    /// The dwell p50 of the second the guard judged by.
    pub dwell_after: Option<Duration>,
}

/// The guard's pending judgement of a new cap.
#[derive(Clone, Copy, Debug)]
struct Verify {
    before: Duration,
    /// Rolls left until the guard judges, by the next loaded second.
    rolls: u32,
}

/// The cap and what moves it, one rolled second at a time.
#[derive(Debug, Default)]
pub(super) struct ThroughputCap {
    cap: Option<u32>,
    /// The dwell p50 of the second that set the cap.
    dwell_before: Option<Duration>,
    /// Loaded seconds, while capped, since the last full one.
    quiet: u32,
    verify: Option<Verify>,
    /// No probe raises the cap before this.
    probe_after: Option<Instant>,
    /// The cap the encoder last filled again at, which no rise reaches
    /// before `probe_after` unless the dwell shows the encoder a step faster.
    ceiling: Option<u32>,
    /// How often this encoder filled, which doubles the probe hold.
    fills: u32,
    /// Lifts in a row since a cap was last kept, which double their hold.
    lifts: u32,
    /// No cap is set before this: the guard lifted the last one.
    held_until: Option<Instant>,
}

/// `base` doubled `times` times, at most `max`.
fn doubled(base: Duration, times: u32, max: Duration) -> Duration {
    base.saturating_mul(1 << times.min(16)).min(max)
}

/// A step of a rise from `cap`: a twentieth, at least 1.
fn step(cap: u32) -> u32 {
    (cap / RAISE_DIVISOR).max(1)
}

impl ThroughputCap {
    pub(super) fn cap(&self) -> Option<u32> {
        self.cap
    }

    /// The encoder was rebuilt: start over, its holds included. The change
    /// to trace, if a cap was set. The second the rebuild falls in mostly
    /// shows the new encoder, which a still slow one fills within a period
    /// or two; a cap it set wrongly the guard or the dwell undoes.
    pub(super) fn reset(&mut self) -> Option<Change> {
        let change = self.cap.map(|_| Change {
            cap: None,
            ..self.change(Action::Reset, None)
        });
        *self = Self::default();
        change
    }

    /// One rolled second, while thread C captured: `asked` is the least pace
    /// the viewers ask for, before the cap, and `floor` the highest of their
    /// presets' least paces, below which no cap goes.
    pub(super) fn roll(
        &mut self,
        second: &WindowStats,
        asked: u32,
        floor: u32,
        now: Instant,
    ) -> Option<Change> {
        let seconds = second.elapsed.as_secs_f64().max(1e-3);
        let units = f64::from(second.units) / seconds;
        let full = second.full_ticks > 0
            && second
                .encoder_dwell
                .is_some_and(|dwell| units * dwell.as_secs_f64() >= OCCUPANCY_FULL);
        let measured = ((CAP_SHARE * units).floor() as u32).max(floor).max(1);
        let Some(cap) = self.cap else {
            let before = second.encoder_dwell.filter(|_| full)?;
            // A cap at or above what the encoder returned would drain
            // nothing: an encoder slower than the presets' least pace keeps
            // its queue.
            let spare = units - f64::from(measured);
            if measured >= asked || spare <= 0.0 || self.held_until.is_some_and(|until| now < until)
            {
                return None;
            }
            let rolls = ((BACKLOG / spare).ceil() as u32)
                .saturating_add(1)
                .clamp(VERIFY_ROLLS, VERIFY_ROLLS_MAX);
            *self = Self {
                cap: Some(measured),
                dwell_before: Some(before),
                verify: Some(Verify { before, rolls }),
                fills: self.fills.saturating_add(1),
                lifts: self.lifts,
                held_until: self.held_until,
                ..Self::default()
            };
            self.probe_after = Some(now + self.probe_hold());
            return Some(self.change(Action::Capped, None));
        };
        // A pace asked at or below the cap, as in a startup hold, leaves it
        // nothing to hold back, and its seconds tell nothing about it: it
        // waits for the pace to come back.
        if asked <= cap {
            return None;
        }
        let loaded = units >= LOAD_SHARE * f64::from(cap);
        if full {
            self.quiet = 0;
        } else if loaded {
            self.quiet = self.quiet.saturating_add(1);
        }
        if let Some(verify) = &mut self.verify {
            verify.rolls = verify.rolls.saturating_sub(1);
            let (0, true, Some(after)) = (verify.rolls, loaded, second.encoder_dwell) else {
                return None;
            };
            let before = verify.before;
            self.verify = None;
            if after.as_secs_f64() >= DWELL_SHARE * before.as_secs_f64() {
                self.held_until = Some(now + doubled(LIFTED_HOLD, self.lifts, LIFTED_HOLD_MAX));
                self.lifts = self.lifts.saturating_add(1);
                return Some(self.clear(Action::Lifted, Some(after)));
            }
            self.lifts = 0;
            return Some(self.change(Action::Kept, Some(after)));
        }
        if full {
            // Full again at the cap: a probe raised it past the encoder, or
            // the pictures grew heavier. What it delivered now bounds the
            // cap, and the cap it filled at bounds the rises until a longer
            // hold passes.
            if measured >= cap {
                return None;
            }
            self.fills = self.fills.saturating_add(1);
            self.ceiling = Some(cap);
            self.cap = Some(measured);
            self.probe_after = Some(now + self.probe_hold());
            return Some(self.change(Action::Lowered, None));
        }
        if !loaded || self.quiet < QUIET_SECONDS {
            return None;
        }
        // A unit's dwell is at least its service time, a unit queued behind
        // others' or a pipeline's latency only longer, so the encoder serves
        // at least one unit a dwell.
        let served = second.encoder_dwell.map_or(0, |dwell| {
            (CAP_SHARE / dwell.as_secs_f64().max(1e-6)).min(f64::from(u32::MAX)) as u32
        });
        let raised = if self.probe_after.is_none_or(|after| now >= after) {
            // A probe: the hold passed, and the dwell may understate what a
            // pipelined encoder serves.
            self.ceiling = None;
            cap.saturating_add(step(cap)).max(served)
        } else {
            // Only the dwell raises a measured cap, and only by a step or
            // more, which noise does not: short of where the encoder last
            // filled, unless it shows the encoder a step faster than that
            // too. That is a measurement as well, and holds off a probe.
            if served < cap.saturating_add(step(cap)) {
                return None;
            }
            let raised = match self.ceiling {
                Some(ceiling) if served < ceiling.saturating_add(step(ceiling)) => {
                    served.min(ceiling.saturating_sub(1))
                }
                _ => {
                    self.ceiling = None;
                    served
                }
            };
            if raised <= cap {
                return None;
            }
            self.probe_after = Some(now + self.probe_hold());
            raised
        };
        if raised >= asked {
            return Some(self.clear(Action::Cleared, None));
        }
        self.cap = Some(raised);
        Some(self.change(Action::Raised, None))
    }

    /// How long no probe follows a measurement, for how often the encoder
    /// filled.
    fn probe_hold(&self) -> Duration {
        doubled(PROBE_HOLD, self.fills.saturating_sub(1), PROBE_HOLD_MAX)
    }

    fn change(&self, action: Action, dwell_after: Option<Duration>) -> Change {
        Change {
            action,
            cap: self.cap,
            dwell_before: self.dwell_before,
            dwell_after,
        }
    }

    /// No cap: what this encoder showed (how often it filled, and the lifts
    /// with their hold) stays.
    fn clear(&mut self, action: Action, dwell_after: Option<Duration>) -> Change {
        let change = Change {
            cap: None,
            ..self.change(action, dwell_after)
        };
        *self = Self {
            fills: self.fills,
            lifts: self.lifts,
            held_until: self.held_until,
            ..Self::default()
        };
        change
    }
}

impl<B: Backend, K: Clock> Engine<B, K> {
    /// A rolled second while thread C captured moves the cap at the
    /// encoder's throughput; a change reaches every viewer slot and the
    /// trace (`desktop_encoder_cap`). The pace takes it at the next capture
    /// step. Paused, unavailable or without a viewer, nothing is learned.
    pub(super) fn cap_throughput(&mut self, second: &WindowStats, now: Instant) {
        if self.paused || self.unavailable.is_some() {
            return;
        }
        let mut asked: Option<u32> = None;
        let mut floor = 1;
        for slot in self.slots.iter().flatten().filter(|slot| !slot.paused) {
            let targets = &slot.shared.targets;
            let pace = targets.pace_fps.load(Ordering::Acquire).max(1);
            asked = Some(asked.map_or(pace, |least| least.min(pace)));
            floor = floor.max(targets.min_fps.load(Ordering::Acquire));
        }
        let Some(asked) = asked else {
            return;
        };
        let Some(change) = self.throughput.roll(second, asked, floor, now) else {
            return;
        };
        self.publish_throughput(change, Some((second, asked)));
    }

    /// Thread C rebuilt its encoder, for new limits, a new desktop or a
    /// recovery: a cap measured on the old one no longer holds.
    pub(super) fn reset_throughput(&mut self) {
        if let Some(change) = self.throughput.reset() {
            self.publish_throughput(change, None);
        }
    }

    /// Trace a change of the cap, with the rolled second that made it and
    /// the pace asked then, and store it in every viewer slot. A probe's
    /// rise and the fall it may lead to are traced at debug, the rest at
    /// info.
    fn publish_throughput(&self, change: Change, rolled: Option<(&WindowStats, u32)>) {
        let micros = |dwell: Option<Duration>| {
            dwell.map(|dwell| u64::try_from(dwell.as_micros()).unwrap_or(u64::MAX))
        };
        let (full_ticks, units, window_ms, asked_fps) =
            rolled.map_or((None, None, None, None), |(second, asked)| {
                (
                    Some(second.full_ticks),
                    Some(second.units),
                    Some(u64::try_from(second.elapsed.as_millis()).unwrap_or(u64::MAX)),
                    Some(asked),
                )
            });
        macro_rules! trace_change {
            ($level:ident) => {
                tracing::$level!(
                    target: "pb_mapper_desktop::feedback",
                    event = "desktop_encoder_cap",
                    action = change.action.name(),
                    cap = change.cap,
                    asked_fps,
                    full_ticks,
                    units,
                    window_ms,
                    dwell_before_us = micros(change.dwell_before),
                    dwell_after_us = micros(change.dwell_after),
                )
            };
        }
        match change.action {
            Action::Raised | Action::Lowered => trace_change!(debug),
            Action::Capped | Action::Kept | Action::Lifted | Action::Cleared | Action::Reset => {
                trace_change!(info)
            }
        }
        let published = change.cap.unwrap_or(0);
        for slot in self.slots.iter().flatten() {
            slot.shared.encoder_cap.store(published, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn second(full_ticks: u32, units: u32, dwell_ms: u64) -> WindowStats {
        WindowStats {
            elapsed: Duration::from_secs(1),
            full_ticks,
            units,
            encoder_dwell: (units > 0).then(|| Duration::from_millis(dwell_ms)),
            ..WindowStats::default()
        }
    }

    /// Rolls `second` each second from `*at` while `until`, stopping at the
    /// first change, which it returns.
    fn roll_until(
        cap: &mut ThroughputCap,
        at: &mut Instant,
        until: Instant,
        second: &WindowStats,
    ) -> Option<Change> {
        while *at < until {
            *at += Duration::from_secs(1);
            if let Some(change) = cap.roll(second, 120, 10, *at) {
                return Some(change);
            }
        }
        None
    }

    /// A serial encoder at 11 ms a unit, paced at 120. A heavy picture's
    /// skipped ticks are not full; four units inside are, and cap the pace at
    /// 95% of what it delivered, which the guard keeps once the dwell fell.
    /// The cap then holds: through quiet seconds whose dwell shows no more
    /// served, a heavy picture, a still desktop's seconds and a pace asked
    /// below it; and probes only 30 s after it was measured. A probe that filled the
    /// encoder again lowers it, and the next waits twice as long.
    #[test]
    fn a_full_encoder_is_capped_and_holds_what_it_measured() {
        let t0 = Instant::now();
        let mut cap = ThroughputCap::default();
        assert_eq!(cap.roll(&second(3, 118, 8), 120, 10, t0), None);
        let capped = cap.roll(&second(29, 90, 44), 120, 10, t0).unwrap();
        assert_eq!((capped.action, capped.cap), (Action::Capped, Some(85)));
        let mut at = t0;
        assert_eq!(cap.roll(&second(1, 86, 20), 120, 10, at), None);
        let kept = cap.roll(&second(0, 85, 11), 120, 10, at).unwrap();
        assert_eq!((kept.action, kept.cap), (Action::Kept, Some(85)));
        assert_eq!(kept.dwell_before, Some(Duration::from_millis(44)));
        assert_eq!(kept.dwell_after, Some(Duration::from_millis(11)));
        // An 11 ms dwell shows 86 served, less than a step more.
        assert_eq!(
            roll_until(
                &mut cap,
                &mut at,
                t0 + Duration::from_secs(10),
                &second(0, 85, 11)
            ),
            None
        );
        // A heavy picture's skipped ticks, with most units waiting behind
        // none; a still desktop's liveness repeats; a startup hold's pace.
        assert_eq!(cap.roll(&second(2, 83, 12), 120, 10, at), None);
        assert_eq!(cap.roll(&second(0, 4, 2), 120, 10, at), None);
        assert_eq!(cap.roll(&second(0, 15, 11), 15, 10, at), None);
        assert_eq!(cap.cap(), Some(85));
        let probe = t0 + PROBE_HOLD;
        assert_eq!(
            roll_until(
                &mut cap,
                &mut at,
                probe - Duration::from_secs(1),
                &second(0, 85, 11)
            ),
            None
        );
        let raised = roll_until(&mut cap, &mut at, probe, &second(0, 85, 11)).unwrap();
        assert_eq!((raised.action, raised.cap), (Action::Raised, Some(89)));
        at += Duration::from_secs(1);
        let raised = cap.roll(&second(0, 89, 11), 120, 10, at).unwrap();
        assert_eq!(raised.cap, Some(93));
        // Filled again at 93: back to what it delivered, and the next probe
        // 60 s on.
        at += Duration::from_secs(1);
        let lowered = cap.roll(&second(3, 91, 40), 120, 10, at).unwrap();
        assert_eq!((lowered.action, lowered.cap), (Action::Lowered, Some(86)));
        let filled = at;
        let quiet = second(0, 86, 11);
        assert_eq!(
            roll_until(
                &mut cap,
                &mut at,
                filled + 2 * PROBE_HOLD - Duration::from_secs(1),
                &quiet
            ),
            None
        );
        let raised = roll_until(&mut cap, &mut at, filled + 2 * PROBE_HOLD, &quiet).unwrap();
        assert_eq!((raised.action, raised.cap), (Action::Raised, Some(90)));
    }

    /// Once measured, only the dwell raises the cap before a probe: short of
    /// the cap the encoder last filled at, unless it shows the encoder a step
    /// faster than that too, and then to the pace asked for, which clears it.
    #[test]
    fn the_dwell_raises_a_measured_cap_short_of_where_it_filled() {
        let t0 = Instant::now();
        let mut cap = ThroughputCap {
            cap: Some(93),
            fills: 1,
            quiet: QUIET_SECONDS,
            probe_after: Some(t0),
            ..ThroughputCap::default()
        };
        let lowered = cap.roll(&second(3, 91, 40), 120, 10, t0).unwrap();
        assert_eq!((lowered.action, lowered.cap), (Action::Lowered, Some(86)));
        let mut at = t0;
        for _ in 0..QUIET_SECONDS - 1 {
            assert_eq!(cap.roll(&second(0, 86, 10), 120, 10, at), None);
        }
        // 10 ms shows 95 served: a step more than 86, short of 93 + 4.
        at += Duration::from_secs(1);
        let raised = cap.roll(&second(0, 86, 10), 120, 10, at).unwrap();
        assert_eq!((raised.action, raised.cap), (Action::Raised, Some(92)));
        at += Duration::from_secs(1);
        assert_eq!(cap.roll(&second(0, 92, 10), 120, 10, at), None);
        // A faster encoder: 4 ms shows 237 served.
        at += Duration::from_secs(1);
        let cleared = cap.roll(&second(0, 92, 4), 120, 10, at).unwrap();
        assert_eq!((cleared.action, cleared.cap), (Action::Cleared, None));
    }

    /// A pipelined encoder whose latency filled it: the cap leaves the
    /// dwell where it was, so the guard lifts it and holds off for 30 s, then
    /// for twice as long after the next.
    #[test]
    fn a_cap_that_leaves_the_dwell_is_lifted_and_held_off_longer_each_time() {
        let t0 = Instant::now();
        let mut cap = ThroughputCap::default();
        let full = second(31, 88, 45);
        let capped = cap.roll(&full, 120, 10, t0).unwrap();
        assert_eq!(capped.cap, Some(83));
        assert_eq!(cap.roll(&second(0, 83, 45), 120, 10, t0), None);
        let lifted = cap.roll(&second(0, 83, 45), 120, 10, t0).unwrap();
        assert_eq!((lifted.action, lifted.cap), (Action::Lifted, None));
        assert_eq!(lifted.dwell_after, Some(Duration::from_millis(45)));
        let mut at = t0;
        assert_eq!(
            roll_until(
                &mut cap,
                &mut at,
                t0 + LIFTED_HOLD - Duration::from_secs(1),
                &full
            ),
            None
        );
        let again = roll_until(&mut cap, &mut at, t0 + LIFTED_HOLD, &full).unwrap();
        assert_eq!(again.action, Action::Capped);
        assert_eq!(cap.roll(&second(0, 83, 45), 120, 10, at), None);
        let lifted = cap.roll(&second(0, 83, 45), 120, 10, at).unwrap();
        assert_eq!(lifted.action, Action::Lifted);
        let start = at;
        assert_eq!(
            roll_until(
                &mut cap,
                &mut at,
                start + 2 * LIFTED_HOLD - Duration::from_secs(1),
                &full
            ),
            None
        );
        assert!(roll_until(&mut cap, &mut at, start + 2 * LIFTED_HOLD, &full).is_some());
    }

    /// A second that produced nothing, or too little to hold the backlog, is
    /// a stall and caps nothing; an encoder slower than the presets' least
    /// pace is not capped, which would drain nothing. A slow encoder's cap
    /// is judged once its backlog drained at the 1 a second it leaves spare.
    #[test]
    fn a_stall_is_not_full_and_a_slow_encoder_is_judged_once_drained() {
        let t0 = Instant::now();
        let mut cap = ThroughputCap::default();
        assert_eq!(cap.roll(&second(120, 0, 0), 120, 10, t0), None);
        assert_eq!(cap.roll(&second(100, 5, 200), 120, 10, t0), None);
        assert_eq!(cap.roll(&second(100, 9, 450), 120, 10, t0), None);
        let capped = cap.roll(&second(40, 20, 183), 60, 10, t0).unwrap();
        assert_eq!(capped.cap, Some(19));
        for dwell in [160, 120, 80] {
            assert_eq!(cap.roll(&second(0, 20, dwell), 60, 10, t0), None);
        }
        let kept = cap.roll(&second(0, 19, 52), 60, 10, t0).unwrap();
        assert_eq!((kept.action, kept.cap), (Action::Kept, Some(19)));
    }

    /// A rebuilt encoder starts over: a cap measured on the old one goes,
    /// and so does the hold a lift left, which judged the old one.
    #[test]
    fn a_reset_forgets_the_cap_and_the_holds() {
        let t0 = Instant::now();
        let mut cap = ThroughputCap::default();
        assert_eq!(cap.reset(), None);
        cap.roll(&second(31, 88, 45), 120, 10, t0).unwrap();
        assert_eq!(cap.roll(&second(0, 83, 45), 120, 10, t0), None);
        let lifted = cap.roll(&second(0, 83, 45), 120, 10, t0).unwrap();
        assert_eq!(lifted.action, Action::Lifted);
        assert_eq!(cap.roll(&second(29, 90, 44), 120, 10, t0), None);
        assert_eq!(cap.reset(), None);
        cap.roll(&second(29, 90, 44), 120, 10, t0).unwrap();
        let reset = cap.reset().unwrap();
        assert_eq!(
            (reset.action, reset.cap, cap.cap()),
            (Action::Reset, None, None)
        );
        assert!(cap.roll(&second(29, 90, 44), 120, 10, t0).is_some());
    }
}
