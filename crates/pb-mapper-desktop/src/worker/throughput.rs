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
//! Thread C paces within the cap itself, and publishes it to each viewer
//! slot (`SessionShared::encoder_cap`), whose QoS takes it into the pace it
//! expects, cuts from and reports in `Stats`.

use super::{Backend, Clock, Engine, window::WindowStats};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

/// An uncapped encoder is full when it skipped at least this many ticks a
/// second, or a tenth of the pace if more; a capped one at this many.
const FULL_TICKS_MIN: f64 = 2.0;
const FULL_TICKS_SHARE: f64 = 0.1;
/// The cap's share of the units the full encoder produced a second.
const CAP_SHARE: f64 = 0.95;
/// Seconds in a row without a full tick before the cap rises; each such
/// second after that it rises by a twentieth, at least 1, or to `CAP_SHARE`
/// of what the second's dwell shows the encoder serves if that is more, and
/// it clears once it reaches the pace the viewers ask for.
const QUIET_SECONDS: u32 = 3;
const RAISE_DIVISOR: u32 = 20;
/// The rolled second, counted from the one that capped, whose dwell the
/// guard compares with that second's.
const VERIFY_ROLLS: u32 = 2;
/// A cap that did not bring the dwell below this share of what it was lifts
/// again: latency, not throughput, filled the encoder.
const DWELL_SHARE: f64 = 0.6;
/// How long no cap is set after the guard lifted one.
const LIFTED_HOLD: Duration = Duration::from_secs(30);

const _: () = assert!(
    CAP_SHARE < 1.0 && DWELL_SHARE < 1.0,
    "the cap sits below the encoder's throughput, and must shorten its dwell"
);
const _: () = assert!(
    VERIFY_ROLLS < QUIET_SECONDS,
    "the guard judges a cap before it first rises"
);

/// What a rolled second did to the cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    /// The encoder was full: the pace is capped at what it delivered.
    Capped,
    /// The guard found the cap shortened the dwell, and keeps it.
    Kept,
    /// The guard found the dwell no shorter, and lifted the cap for
    /// `LIFTED_HOLD`.
    Lifted,
    /// A capped encoder was full again, at a raised cap or a heavier
    /// picture: the cap falls to what it delivered.
    Lowered,
    /// Quiet seconds raised the cap.
    Raised,
    /// The cap reached the pace the viewers ask for, or that pace fell to
    /// it.
    Cleared,
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
    /// Rolls left until the second it judges by.
    rolls: u32,
}

/// The cap and what moves it, one rolled second at a time.
#[derive(Debug, Default)]
pub(super) struct ThroughputCap {
    cap: Option<u32>,
    /// The dwell p50 of the second that set the cap.
    dwell_before: Option<Duration>,
    /// Rolled seconds in a row without a full tick.
    quiet: u32,
    verify: Option<Verify>,
    /// No cap is set before this: the guard lifted the last one.
    held_until: Option<Instant>,
}

impl ThroughputCap {
    pub(super) fn cap(&self) -> Option<u32> {
        self.cap
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
        let full = f64::from(second.full_ticks) / seconds;
        let units = f64::from(second.units) / seconds;
        self.quiet = if second.full_ticks == 0 {
            self.quiet.saturating_add(1)
        } else {
            0
        };
        let measured = ((CAP_SHARE * units).floor() as u32).max(floor).max(1);
        let Some(cap) = self.cap else {
            let threshold = (f64::from(asked) * FULL_TICKS_SHARE).max(FULL_TICKS_MIN);
            // A second that produced nothing measured no throughput: a
            // stall, which must not leave a cap behind.
            if full < threshold
                || second.units == 0
                || measured >= asked
                || self.held_until.is_some_and(|until| now < until)
            {
                return None;
            }
            self.cap = Some(measured);
            self.dwell_before = second.encoder_dwell;
            self.verify = second.encoder_dwell.map(|before| Verify {
                before,
                rolls: VERIFY_ROLLS,
            });
            return Some(self.change(Action::Capped, None));
        };
        if cap >= asked {
            return Some(self.clear(Action::Cleared, None));
        }
        if let Some(verify) = &mut self.verify {
            verify.rolls = verify.rolls.saturating_sub(1);
            // A second without units has no dwell: the next with some judges.
            let (0, Some(after)) = (verify.rolls, second.encoder_dwell) else {
                return None;
            };
            let before = verify.before;
            self.verify = None;
            if after.as_secs_f64() >= DWELL_SHARE * before.as_secs_f64() {
                self.held_until = Some(now + LIFTED_HOLD);
                return Some(self.clear(Action::Lifted, Some(after)));
            }
            return Some(self.change(Action::Kept, Some(after)));
        }
        if full >= FULL_TICKS_MIN {
            // Full again at the cap: what it delivered now bounds it. At
            // a cap that was raised past the throughput, the queue would
            // otherwise stay four deep with too few full ticks to cap anew.
            if second.units == 0 || measured >= cap {
                return None;
            }
            self.cap = Some(measured);
            return Some(self.change(Action::Lowered, None));
        }
        if self.quiet < QUIET_SECONDS {
            return None;
        }
        // A unit's dwell is at least its service time, a unit queued behind
        // others' or a pipeline's latency only longer, so the encoder serves
        // at least one unit a dwell: a cap a stall left low comes back at
        // once rather than a twentieth a second.
        let served = second.encoder_dwell.map_or(0, |dwell| {
            (CAP_SHARE / dwell.as_secs_f64().max(1e-6)).min(f64::from(u32::MAX)) as u32
        });
        let raised = cap.saturating_add((cap / RAISE_DIVISOR).max(1)).max(served);
        if raised >= asked {
            return Some(self.clear(Action::Cleared, None));
        }
        self.cap = Some(raised);
        Some(self.change(Action::Raised, None))
    }

    fn change(&self, action: Action, dwell_after: Option<Duration>) -> Change {
        Change {
            action,
            cap: self.cap,
            dwell_before: self.dwell_before,
            dwell_after,
        }
    }

    fn clear(&mut self, action: Action, dwell_after: Option<Duration>) -> Change {
        let change = Change {
            cap: None,
            ..self.change(action, dwell_after)
        };
        self.cap = None;
        self.dwell_before = None;
        self.verify = None;
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
        let micros = |dwell: Option<Duration>| {
            dwell.map(|dwell| u64::try_from(dwell.as_micros()).unwrap_or(u64::MAX))
        };
        tracing::info!(
            target: "pb_mapper_desktop::feedback",
            event = "desktop_encoder_cap",
            action = change.action.name(),
            cap = change.cap,
            asked_fps = asked,
            full_ticks = second.full_ticks,
            units = second.units,
            window_ms = u64::try_from(second.elapsed.as_millis()).unwrap_or(u64::MAX),
            dwell_before_us = micros(change.dwell_before),
            dwell_after_us = micros(change.dwell_after),
        );
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

    /// A serial encoder at 11 ms a unit, paced at 120: capped at 95% of
    /// what it delivered, kept once the dwell fell, raised after three
    /// quiet seconds, lowered when the raise filled it again, and cleared
    /// once the cap reaches the pace asked for.
    #[test]
    fn a_full_encoder_is_capped_kept_raised_lowered_and_cleared() {
        let now = Instant::now();
        let mut cap = ThroughputCap::default();
        // Below a tenth of the pace, an uncapped encoder keeps up.
        assert_eq!(cap.roll(&second(11, 90, 44), 120, 10, now), None);
        let capped = cap.roll(&second(29, 90, 44), 120, 10, now).unwrap();
        assert_eq!((capped.action, capped.cap), (Action::Capped, Some(85)));
        assert_eq!(cap.roll(&second(1, 86, 20), 120, 10, now), None);
        let kept = cap.roll(&second(0, 85, 11), 120, 10, now).unwrap();
        assert_eq!((kept.action, kept.cap), (Action::Kept, Some(85)));
        assert_eq!(kept.dwell_before, Some(Duration::from_millis(44)));
        assert_eq!(kept.dwell_after, Some(Duration::from_millis(11)));
        // Two more quiet seconds make three: it rises by a twentieth, more
        // than the 86 an 11 ms dwell shows served.
        assert_eq!(cap.roll(&second(0, 85, 11), 120, 10, now), None);
        let raised = cap.roll(&second(0, 85, 11), 120, 10, now).unwrap();
        assert_eq!((raised.action, raised.cap), (Action::Raised, Some(89)));
        let raised = cap.roll(&second(0, 89, 11), 120, 10, now).unwrap();
        assert_eq!(raised.cap, Some(93));
        // Two full ticks at the raised cap: back to what it delivered.
        let lowered = cap.roll(&second(3, 90, 40), 120, 10, now).unwrap();
        assert_eq!((lowered.action, lowered.cap), (Action::Lowered, Some(85)));
        // The pace asked for fell to the cap: nothing left to hold back.
        let cleared = cap.roll(&second(0, 80, 11), 80, 10, now).unwrap();
        assert_eq!((cleared.action, cleared.cap), (Action::Cleared, None));
        assert_eq!(cap.cap(), None);
    }

    /// A pipelined encoder whose latency filled it: the cap leaves the
    /// dwell where it was, so the guard lifts it and holds off for 30 s.
    #[test]
    fn a_cap_that_leaves_the_dwell_is_lifted_and_held_off() {
        let now = Instant::now();
        let mut cap = ThroughputCap::default();
        let capped = cap.roll(&second(31, 88, 45), 120, 10, now).unwrap();
        assert_eq!(capped.cap, Some(83));
        assert_eq!(cap.roll(&second(0, 83, 45), 120, 10, now), None);
        let lifted = cap.roll(&second(0, 83, 45), 120, 10, now).unwrap();
        assert_eq!((lifted.action, lifted.cap), (Action::Lifted, None));
        assert_eq!(lifted.dwell_after, Some(Duration::from_millis(45)));
        let later = now + LIFTED_HOLD - Duration::from_millis(1);
        assert_eq!(cap.roll(&second(31, 88, 45), 120, 10, later), None);
        let again = cap.roll(&second(31, 88, 45), 120, 10, now + LIFTED_HOLD);
        assert_eq!(again.map(|change| change.action), Some(Action::Capped));
    }

    /// A second that produced nothing is a stall, not a throughput, and
    /// leaves no cap; one that produced little caps no lower than the
    /// presets' least pace, and once the encoder serves again its dwell
    /// raises the cap at once rather than a twentieth a second.
    #[test]
    fn a_stall_sets_no_cap_and_a_low_cap_comes_back_by_the_dwell() {
        let now = Instant::now();
        let mut cap = ThroughputCap::default();
        assert_eq!(cap.roll(&second(120, 0, 0), 120, 10, now), None);
        let capped = cap.roll(&second(100, 5, 44), 120, 12, now).unwrap();
        assert_eq!(capped.cap, Some(12));
        assert_eq!(cap.roll(&second(0, 12, 11), 120, 12, now), None);
        let kept = cap.roll(&second(0, 12, 11), 120, 12, now).unwrap();
        assert_eq!((kept.action, kept.cap), (Action::Kept, Some(12)));
        // The third quiet second: an 11 ms dwell shows 86 served.
        let raised = cap.roll(&second(0, 12, 11), 120, 12, now).unwrap();
        assert_eq!((raised.action, raised.cap), (Action::Raised, Some(86)));
        // A dwell that shows the encoder serving the pace clears it.
        let mut fast = ThroughputCap {
            cap: Some(86),
            quiet: QUIET_SECONDS,
            ..ThroughputCap::default()
        };
        let cleared = fast.roll(&second(0, 86, 4), 120, 12, now).unwrap();
        assert_eq!((cleared.action, cleared.cap), (Action::Cleared, None));
    }
}
