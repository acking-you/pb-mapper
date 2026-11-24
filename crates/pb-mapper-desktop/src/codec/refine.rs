//! When the last picture goes to the encoder again on a desktop that stopped
//! changing: a short burst of refinement repeats, then the liveness repeat.
//!
//! An encoder that budgets each frame (Media Foundation's on NVIDIA, measured
//! `nominal`) codes a large change, a scroll or a window switch, only as
//! well as one frame's share of the bitrate allows. A repeat of the
//! unchanged picture codes part of what that left out, so each repeat
//! sharpens it by about one more frame's share; the liveness repeat comes
//! only every `LIVENESS_INTERVAL`. So once a picture that its budget limited
//! has stood still `REFINE_AFTER`, [`Repeats`] repeats it at the capture
//! pace, a refinement burst, until a repeat shows that the encoder has
//! little left to code, or the burst's cap; then the liveness repeat takes
//! over, unchanged. A refinement repeat leaves the caller's capture tick
//! armed, so a new picture that comes during a burst goes at once.
//!
//! Neutral, with an injected clock, and tested everywhere. Only the Windows
//! encoder refines, on a GPU whose encoder earned it by a device run. An
//! encoder that budgets by the sample times gives a repeat the bits of the
//! time since the last unit, whatever the spacing: on VideoToolbox (see the
//! still-refinement benchmark record) a burst's first repeats sharpen the
//! picture sooner, its later ones come out starved, and the picture
//! converges no sooner than with the liveness repeat alone, which the Mac
//! host keeps.

use super::pipeline::LIVENESS_INTERVAL;
use std::time::{Duration, Instant};

/// A picture refines once it has stood this long after the last new picture
/// or keyframe went to the encoder: a desktop that changes more often
/// (typing, video, a drag) never waits behind a refinement repeat.
pub(crate) const REFINE_AFTER: Duration = Duration::from_millis(200);
const _: () = assert!(
    REFINE_AFTER.as_millis() < LIVENESS_INTERVAL.as_millis(),
    "a still picture refines before its liveness repeat"
);
/// A repeat smaller than this coded next to nothing: the encoder has little
/// left to add to the picture.
pub(crate) const REFINE_DONE_BYTES: usize = 512;
/// The repeat outputs of a burst before it may stop as converged: right
/// after an IDR or a large unit an encoder's rate control can starve the
/// first repeats, which then come out small, or at a QP that does not drop,
/// before the picture converged.
pub(crate) const REFINE_MIN: u8 = 3;
/// The most refinement repeats one picture gets, however often new pictures
/// interrupt them: a start, which a device bench sets.
pub(crate) const REFINE_MAX: u8 = 15;
/// The longest one burst runs, from its first repeat: a start, which a
/// device bench sets.
pub(crate) const REFINE_MAX_TIME: Duration = Duration::from_secs(1);
/// A repeat the encoder says it coded at this QP or finer ends its burst,
/// where the encoder says (`MFSampleExtension_VideoEncodeQP` on Windows): a
/// start, which a device bench sets.
pub(crate) const REFINE_QP_FLOOR: u32 = 18;
/// A new picture's unit at least this share of one frame's budget was
/// limited by it, `(numerator, denominator)`: a typed character or a caret
/// blink codes far under it, already sharp, and starts no burst.
const LIMITED: (usize, usize) = (3, 4);
const _: () = assert!(
    REFINE_MIN >= 2 && REFINE_MIN <= REFINE_MAX,
    "a burst judges two outputs in a row, within its cap"
);

/// The repeat of the last picture that is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Repeat {
    /// A refinement burst's repeat, at the capture pace: it leaves the
    /// caller's tick armed.
    Refine,
    /// The liveness repeat, `LIVENESS_INTERVAL` after the last submit: it
    /// spends the tick, as before.
    Liveness,
}

/// When an encoder sends its last picture again on its own (a keyframe aside,
/// which the caller asks for): the refinement bursts and the liveness
/// repeat. The encoder tells it each fresh picture (a new one, or a
/// keyframe) and each refinement repeat it submits, and each unit it
/// outputs; it answers when the next repeat is due and which.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Repeats {
    /// Bursts run at all: an encoder whose vendor earned them. Otherwise
    /// only the liveness repeat, as before.
    enabled: bool,
    /// The capture pace's period: refinement repeats come at least this far
    /// apart, and none while it is not shorter than `LIVENESS_INTERVAL`,
    /// which then repeats as often.
    period: Duration,
    /// One frame's share of the bitrate at the nominal rate, in bytes.
    budget: usize,
    /// The last fresh picture submitted: when.
    fresh: Option<Instant>,
    /// A fresh unit since the picture last finished refining filled its
    /// budget: the picture is due a burst once it stands still.
    limited: bool,
    /// The refinement repeats spent since `limited` was set, across the
    /// bursts new pictures interrupted.
    spent: u8,
    /// The burst under way, from its first repeat; a new picture or a
    /// keyframe ends it, and so do its own stops.
    burst: Option<Burst>,
}

/// A refinement burst under way.
#[derive(Clone, Copy, Debug)]
struct Burst {
    /// When its first repeat was submitted.
    started: Instant,
    /// That repeat's sample time: the outputs from it on are the burst's,
    /// and older ones (a new picture's, an earlier burst's) say nothing of
    /// it.
    from_pts: u64,
    /// Its repeats' outputs so far.
    outputs: u8,
    /// Those, in a row up to the last, under `REFINE_DONE_BYTES`.
    small: u8,
    /// The lowest QP an output said, and the outputs since one lowered it.
    qp: Option<u32>,
    flat: u8,
}

impl Repeats {
    /// Bursts when `enabled`, for an encoder at `bitrate` and a nominal
    /// `nominal_fps`, paced at `pace_fps`.
    pub(crate) fn new(enabled: bool, bitrate: u32, nominal_fps: u32, pace_fps: u32) -> Self {
        let mut repeats = Self {
            enabled,
            ..Self::default()
        };
        repeats.rate(bitrate, nominal_fps, pace_fps);
        repeats
    }

    /// The encoder's bitrate, its nominal frame rate (what one frame's
    /// budget is a share of) and the capture pace, as the caller sets them.
    pub(crate) fn rate(&mut self, bitrate: u32, nominal_fps: u32, pace_fps: u32) {
        self.budget = (bitrate / 8 / nominal_fps.max(1)) as usize;
        self.period = Duration::from_secs(1) / pace_fps.max(1);
    }

    /// A fresh picture, a new one or a keyframe, went to the encoder at
    /// `at`: a burst under way ends, and the picture refines once it has
    /// stood `REFINE_AFTER`, if a fresh unit since the last one refined was
    /// limited by its budget. A burst a new picture interrupts did not
    /// finish its picture, which therefore still refines, within what is
    /// left of `REFINE_MAX`.
    pub(crate) fn fresh(&mut self, at: Instant) {
        self.fresh = Some(at);
        self.burst = None;
    }

    /// A refinement repeat, of sample time `pts_us`, went to the encoder at
    /// `at`: the first starts a burst. One that spends `REFINE_MAX`, or
    /// after which no other fits in `REFINE_MAX_TIME`, ends it, and the
    /// picture with it.
    pub(crate) fn refined(&mut self, at: Instant, pts_us: u64) {
        let burst = self.burst.get_or_insert(Burst {
            started: at,
            from_pts: pts_us,
            outputs: 0,
            small: 0,
            qp: None,
            flat: 0,
        });
        let started = burst.started;
        self.spent = self.spent.saturating_add(1);
        let next = at.saturating_duration_since(started) + self.period;
        if self.spent >= REFINE_MAX || next >= REFINE_MAX_TIME {
            self.finished();
        }
    }

    /// The encoder output a unit of sample time `pts_us`, `len` bytes long,
    /// coded at `qp` where it says. A fresh one (not a `duplicate`, or a
    /// `keyframe`) that fills `LIMITED` of a frame's budget makes its
    /// picture due a burst. A repeat output of the burst under way that
    /// shows the encoder has little left to code ends it, once it has had
    /// `REFINE_MIN` outputs (bar a QP at `REFINE_QP_FLOOR`, which ends it at
    /// once): two in a row under `REFINE_DONE_BYTES`, or two that did not
    /// lower the QP. Outputs older than the burst say nothing of it.
    pub(crate) fn output(
        &mut self,
        pts_us: u64,
        duplicate: bool,
        keyframe: bool,
        len: usize,
        qp: Option<u32>,
    ) {
        if !self.enabled {
            return;
        }
        if !duplicate || keyframe {
            if len.saturating_mul(LIMITED.1) >= self.budget.saturating_mul(LIMITED.0) {
                self.limited = true;
                self.spent = 0;
            }
            // An IDR the encoder made of a repeat on its own codes the
            // picture anew: the burst judges from it.
            if let Some(burst) = self.burst.as_mut().filter(|burst| pts_us >= burst.from_pts) {
                burst.small = 0;
                burst.qp = None;
                burst.flat = 0;
            }
            return;
        }
        let Some(burst) = self.burst.as_mut().filter(|burst| pts_us >= burst.from_pts) else {
            return;
        };
        burst.outputs = burst.outputs.saturating_add(1);
        burst.small = if len < REFINE_DONE_BYTES {
            burst.small.saturating_add(1)
        } else {
            0
        };
        if let Some(qp) = qp {
            if burst.qp.is_none_or(|best| qp < best) {
                burst.qp = Some(qp);
                burst.flat = 0;
            } else {
                burst.flat = burst.flat.saturating_add(1);
            }
        }
        let judged = burst.outputs >= REFINE_MIN;
        let fine = qp.is_some_and(|qp| qp <= REFINE_QP_FLOOR);
        if fine || judged && (burst.small >= 2 || burst.flat >= 2) {
            self.finished();
        }
    }

    /// The picture refined as far as it will: no burst until a fresh unit
    /// fills its budget again.
    fn finished(&mut self) {
        self.burst = None;
        self.limited = false;
        self.spent = 0;
    }

    /// When the next refinement repeat is due, given the last submit at
    /// `last_submit`, while the picture is due one: `REFINE_AFTER` after
    /// the last fresh picture, and a period after the last submit.
    fn refine_due(&self, last_submit: Instant) -> Option<Instant> {
        if !self.enabled || !self.limited || self.period >= LIVENESS_INTERVAL {
            return None;
        }
        let still = self.fresh? + REFINE_AFTER;
        Some(still.max(last_submit + self.period))
    }

    /// When the last picture, last submitted at `last_submit`, is next due
    /// again on its own: the next refinement repeat, else the liveness
    /// repeat.
    pub(crate) fn next_due(&self, last_submit: Instant) -> Instant {
        let liveness = last_submit + LIVENESS_INTERVAL;
        self.refine_due(last_submit)
            .map_or(liveness, |due| due.min(liveness))
    }

    /// The repeat due at `now`, the last submit at `last_submit`, if any.
    /// While a picture is due a burst, its repeats come before liveness
    /// would: the refinement is the repeat.
    pub(crate) fn repeat_now(&self, now: Instant, last_submit: Instant) -> Option<Repeat> {
        if self.refine_due(last_submit).is_some_and(|due| now >= due) {
            Some(Repeat::Refine)
        } else if now >= last_submit + LIVENESS_INTERVAL {
            Some(Repeat::Liveness)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4 Mbit/s at a nominal 60: 8 333 bytes a frame, at a pace of 60.
    const BITRATE: u32 = 4_000_000;
    const BUDGET: usize = 8_333;
    const PACE: u32 = 60;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// An encoder's view of the repeats: its clock, its last submit and its
    /// sample times, which only rise.
    struct Rig {
        repeats: Repeats,
        t0: Instant,
        last: Instant,
        pts: u64,
    }
    impl Rig {
        fn new() -> Self {
            let t0 = Instant::now();
            Self {
                repeats: Repeats::new(true, BITRATE, 60, PACE),
                t0,
                last: t0,
                pts: 0,
            }
        }
        fn at(&self, after: Duration) -> Instant {
            self.t0 + after
        }
        /// A fresh picture submitted `after` the start; its sample time.
        fn fresh(&mut self, after: Duration) -> u64 {
            self.last = self.at(after);
            self.pts += 1;
            self.repeats.fresh(self.last);
            self.pts
        }
        /// A fresh picture whose unit came out `len` bytes long.
        fn change(&mut self, after: Duration, len: usize) -> u64 {
            let pts = self.fresh(after);
            self.repeats.output(pts, false, false, len, None);
            pts
        }
        /// The repeat due at `at`, if any.
        fn due(&self, at: Instant) -> Option<Repeat> {
            self.repeats.repeat_now(at, self.last)
        }
        fn next(&self) -> Instant {
            self.repeats.next_due(self.last)
        }
        /// A refinement repeat submitted at `at`, which must be due then;
        /// its sample time.
        fn refine(&mut self, at: Instant) -> u64 {
            assert_eq!(self.due(at), Some(Repeat::Refine), "at {at:?}");
            self.last = at;
            self.pts += 1;
            self.repeats.refined(at, self.pts);
            self.pts
        }
        /// A refinement repeat at each due time, the n-th output
        /// `sizes[n]` bytes long (the last size for the rest) at `qps[n]`,
        /// until no refinement is due: how many went.
        fn burst(&mut self, sizes: &[usize], qps: &[Option<u32>]) -> usize {
            let mut sent = 0;
            loop {
                let due = self.next();
                if self.due(due) != Some(Repeat::Refine) {
                    return sent;
                }
                let pts = self.refine(due);
                let len = sizes.get(sent).or(sizes.last()).copied().unwrap_or(0);
                let qp = qps.get(sent).or(qps.last()).copied().flatten();
                self.repeats.output(pts, true, false, len, qp);
                sent += 1;
            }
        }
    }

    /// A unit that filled its budget starts a burst `REFINE_AFTER` after
    /// its picture, at the pace, before liveness would repeat it; one far
    /// under its budget, as a typed character's, starts none, and its
    /// still picture is repeated at liveness only.
    #[test]
    fn a_burst_starts_only_after_a_budget_sized_unit_has_stood_still() {
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET / 2);
        assert_eq!(rig.next(), rig.at(LIVENESS_INTERVAL));
        assert_eq!(rig.due(rig.at(ms(249))), None);
        assert_eq!(rig.due(rig.at(ms(250))), Some(Repeat::Liveness));

        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 3 / 4 + 1);
        assert_eq!(rig.next(), rig.at(REFINE_AFTER));
        assert_eq!(rig.due(rig.at(ms(199))), None);
        rig.refine(rig.at(ms(200)));
        // At least a period apart, sooner than liveness.
        let period = Duration::from_secs(1) / PACE;
        assert_eq!(rig.next(), rig.at(ms(200) + period));
        assert_eq!(rig.due(rig.at(ms(201))), None);

        // A large unit followed by small ones, as a scroll's last steps: a
        // fresh unit since the picture last refined was limited.
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        rig.change(ms(17), 300);
        rig.change(ms(33), 200);
        assert_eq!(rig.next(), rig.at(ms(233)));
        // An IDR of a still picture fills its budget many times over.
        let mut rig = Rig::new();
        let pts = rig.fresh(ms(0));
        rig.repeats.output(pts, true, true, BUDGET * 20, None);
        assert_eq!(rig.next(), rig.at(REFINE_AFTER));
    }

    /// A burst needs the vendor's leave, and a pace faster than liveness:
    /// at a pace of 4 or slower, liveness repeats as often. The budget
    /// follows the bitrate.
    #[test]
    fn no_burst_while_disabled_or_paced_no_faster_than_liveness() {
        let mut rig = Rig::new();
        rig.repeats = Repeats::new(false, BITRATE, 60, PACE);
        rig.change(ms(0), BUDGET * 4);
        assert_eq!(rig.next(), rig.at(LIVENESS_INTERVAL));
        assert_eq!(rig.due(rig.at(ms(240))), None);

        let mut rig = Rig::new();
        rig.repeats.rate(BITRATE, 60, 4);
        rig.change(ms(0), BUDGET * 4);
        assert_eq!(rig.next(), rig.at(LIVENESS_INTERVAL));
        rig.repeats.rate(BITRATE, 60, 5);
        assert_eq!(rig.next(), rig.at(REFINE_AFTER));

        // At half the bitrate, half the old budget fills the new one.
        let mut rig = Rig::new();
        rig.repeats.rate(BITRATE / 2, 60, PACE);
        rig.change(ms(0), BUDGET / 2);
        assert_eq!(rig.next(), rig.at(REFINE_AFTER));
    }

    /// The first repeats after a large unit can be starved, coming out
    /// small: before `REFINE_MIN` outputs they do not end the burst. After
    /// it, two small outputs in a row do, and a large one between them
    /// starts the count again. Then liveness only, until a unit fills its
    /// budget again.
    #[test]
    fn small_early_repeats_do_not_stop_a_burst_and_two_small_ones_after_do() {
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        let sizes = [99, 231, 9_000, 8_000, 300, 7_000, 200, 100];
        assert_eq!(rig.burst(&sizes, &[None]), sizes.len());
        assert_eq!(rig.next(), rig.last + LIVENESS_INTERVAL);
        rig.change(ms(2_000), 300);
        assert_eq!(rig.next(), rig.at(ms(2_250)));
        rig.change(ms(3_000), BUDGET);
        assert_eq!(rig.next(), rig.at(ms(3_200)));

        // Small from the start: it ends at `REFINE_MIN`.
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        assert_eq!(rig.burst(&[100], &[None]), usize::from(REFINE_MIN));
    }

    /// A QP the encoder reports ends a burst at the floor at once, and
    /// once two outputs in a row did not lower it, after `REFINE_MIN`
    /// outputs: a QP starved at first, for fewer, does not.
    #[test]
    fn the_qp_stops_a_burst_at_its_floor_or_once_it_stops_dropping() {
        let big = [BUDGET];
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        assert_eq!(rig.burst(&big, &[Some(30), Some(REFINE_QP_FLOOR)]), 2);

        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        let dropping = [Some(40), Some(36), Some(33), Some(30), Some(30), Some(31)];
        assert_eq!(rig.burst(&big, &dropping), dropping.len());

        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        let starved = [Some(51), Some(51), Some(44), Some(40), Some(40), Some(40)];
        assert_eq!(rig.burst(&big, &starved), starved.len());
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        assert_eq!(rig.burst(&big, &[Some(51)]), usize::from(REFINE_MIN));
    }

    /// An encoder that never codes a repeat small (one that pads its
    /// constant bitrate, say) and reports no QP is held to `REFINE_MAX`
    /// repeats, and at a slow pace to `REFINE_MAX_TIME` of them.
    #[test]
    fn a_burst_stops_at_its_cap() {
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        assert_eq!(rig.burst(&[BUDGET], &[None]), usize::from(REFINE_MAX));
        assert_eq!(rig.next(), rig.last + LIVENESS_INTERVAL);

        // 100 ms apart from 200 ms on: the tenth goes 900 ms after the
        // first, and an eleventh would go a second after it.
        let mut rig = Rig::new();
        rig.repeats.rate(BITRATE, 60, 10);
        rig.change(ms(0), BUDGET * 4);
        assert_eq!(rig.burst(&[BUDGET], &[None]), 10);
        assert_eq!(rig.last, rig.at(ms(1_100)));
    }

    /// A new picture, or a keyframe (a fresh picture too), ends the burst
    /// under way. The picture refines again once it stands still, with what
    /// is left of `REFINE_MAX` unless a unit filled its budget meanwhile,
    /// and outputs older than the new burst say nothing of it.
    #[test]
    fn a_new_picture_or_keyframe_ends_a_burst_and_stale_outputs_are_ignored() {
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        let first = rig.refine(rig.at(ms(200)));
        let second = rig.refine(rig.at(ms(217)));
        // A typed character, far under its budget, interrupts it.
        let typed = rig.fresh(ms(240));
        assert_eq!(rig.due(rig.at(ms(250))), None);
        assert_eq!(rig.next(), rig.at(ms(440)));
        // A slow encoder holds them all until the next burst's first
        // repeat went: the interrupted burst's outputs, older than it, say
        // nothing of it, though at a QP of 10 they would have ended it.
        let third = rig.refine(rig.at(ms(440)));
        rig.repeats.output(first, true, false, 10, Some(10));
        rig.repeats.output(second, true, false, 10, Some(10));
        rig.repeats.output(typed, false, false, 300, None);
        rig.repeats.output(third, true, false, BUDGET, None);
        let period = Duration::from_secs(1) / PACE;
        assert_eq!(rig.next(), rig.at(ms(440)) + period);
        let left = usize::from(REFINE_MAX) - 3;
        assert_eq!(rig.burst(&[BUDGET], &[None]), left);

        // A keyframe ends the burst, and its IDR, filling its budget, gives
        // the picture a whole burst again.
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        rig.refine(rig.at(ms(200)));
        let idr = rig.fresh(ms(210));
        rig.repeats.output(idr, true, true, BUDGET * 20, None);
        assert_eq!(rig.next(), rig.at(ms(410)));
        assert_eq!(rig.burst(&[BUDGET], &[None]), usize::from(REFINE_MAX));
    }

    /// Once a burst ends, the liveness repeat goes on as before, every
    /// `LIVENESS_INTERVAL`, and its outputs, however large, start no burst.
    #[test]
    fn liveness_resumes_after_a_burst() {
        let mut rig = Rig::new();
        rig.change(ms(0), BUDGET * 4);
        assert_eq!(rig.burst(&[100], &[None]), usize::from(REFINE_MIN));
        let ended = rig.last;
        assert_eq!(rig.next(), ended + LIVENESS_INTERVAL);
        assert_eq!(rig.due(ended + ms(249)), None);
        assert_eq!(rig.due(ended + LIVENESS_INTERVAL), Some(Repeat::Liveness));
        rig.last = ended + LIVENESS_INTERVAL;
        rig.repeats
            .output(rig.pts + 1, true, false, BUDGET * 2, None);
        assert_eq!(rig.next(), rig.last + LIVENESS_INTERVAL);
        // Nothing submitted fresh yet: liveness only.
        let none = Repeats::new(true, BITRATE, 60, PACE);
        assert_eq!(none.next_due(rig.t0), rig.t0 + LIVENESS_INTERVAL);
    }
}
