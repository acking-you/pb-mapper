//! Receipt-driven rate control, after airhand's `flow.rs`: every transport
//! receipt updates an estimate of the link, a receipt that shows a standing
//! queue cuts the rate the controller hands the encoder within a round trip,
//! and the QoS tick raises it again while the receipts show none.
//!
//! - A receipt's queue is its delay above the baseline, less the unit's own
//!   transfer: at the busy rate, and at most a frame interval (a frame
//!   interval without a busy rate, when the link carried each unit before
//!   the next). The baseline is the probes' base RTT or, once a rebase set
//!   one, the receipts' own when higher: the least queue the rebase found,
//!   which every lower receipt lowers further. The probes' base is never
//!   touched.
//! - The standing queue is the least queue over the newest receipts: at least
//!   three, spanning at least two frame intervals and 50 ms. A burst a few
//!   milliseconds long never makes one on its own. The queue before it is
//!   the least over as many receipts before those.
//! - The busy rate is the median delivery rate over the newest receipt pairs
//!   across which the link stayed busy: the later unit's delay above the
//!   baseline is at least the pair's gap. Receipts less than a quarter of a
//!   frame interval apart are compression, not delivery: such a pair is no
//!   sample, and its later receipt's bytes count toward the pair before it.
//! - The used rate is what the receipts covered over their newest second.
//! - A stall is a receipt gap of at least three frame intervals; the receipts
//!   that come after it compressed, or each with less delay than the one
//!   before, are what it held, and decide nothing. Stalls recur when three
//!   end within 2 s, or two that cover 15% of it.
//!
//! The queue is congested above `QUEUE_HIGH`, or above `QUEUE_LOW` while a
//! clamped window refused a unit, and stalls that recur are congestion too.
//! Congested, a receipt inside a hold decides nothing; a queue that drains
//! holds; two cuts that left it where it was rebase the baseline onto it (a
//! longer route, not a rate the host sends); and otherwise the rate is cut to
//! `min(0.85 × busy, 0.9 × used)`, or `0.85 × used` when stalls recur. Without
//! a busy rate the probe controller remains: there is no blind cut. Two stall
//! cuts that left the stalls as they were excuse them until they stop.
//!
//! With arrival times (`arrival_time`) each receipt is timed on the viewer's
//! clock, when it read its newest unit, and its delay is that unit's queue
//! toward the viewer: the way back, and whatever bunches receipts on it, are
//! in neither. A receipt whose stamp was no sample is left out.
//!
//! The history is a ring reserved once: a receipt allocates nothing.

use crate::{credit::Receipt, video::MAX_FPS};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// Receipts kept at most: two seconds' worth at the highest pace, so a burst
/// never pushes out the last second's.
const RING: usize = 2 * MAX_FPS as usize;
/// How far back from the newest receipt anything is judged.
const HISTORY: Duration = Duration::from_secs(1);
/// The standing queue is judged on at least this many receipts ...
const STANDING_RECEIPTS: usize = 3;
/// ... spanning at least this many frame intervals ...
const STANDING_FRAMES: u32 = 2;
/// ... and at least this long, but never longer than `STANDING_SPAN_MAX`,
/// which the history always covers twice.
const STANDING_SPAN_MIN: Duration = Duration::from_millis(50);
const STANDING_SPAN_MAX: Duration = Duration::from_millis(500);
/// A receipt this many frame intervals after the one before follows a stall.
const STALL_FRAMES: u32 = 3;
/// Receipts closer than a frame interval divided by this are compressed.
const COMPRESSED_DIVISOR: u32 = 4;
/// The busy rate is the median of at most this many samples ...
const BUSY_SAMPLES: usize = 8;
/// ... and unknown with fewer than this many.
const BUSY_SAMPLES_MIN: usize = 3;
/// The used rate needs receipts spanning at least this.
const USED_SPAN_MIN: Duration = Duration::from_millis(100);
/// A standing queue above this is congestion ...
pub(crate) const QUEUE_HIGH: Duration = Duration::from_millis(40);
/// ... and above this while a clamped window refused a unit; below it the
/// link is clear.
pub(crate) const QUEUE_LOW: Duration = Duration::from_millis(10);
/// A standing queue this much below the one before is draining ...
const DRAIN_MARGIN: Duration = Duration::from_millis(5);
/// ... and so is one below this share of the episode's last.
const DRAINING: f64 = 0.8;
/// After this many cuts, a queue within this multiple of the episode's last
/// is not the host's: the baseline moves onto it.
const NO_EFFECT_CUTS: u32 = 2;
const NO_EFFECT: f64 = 1.1;
/// A cut: this share of the busy rate, at most this share of the used rate,
/// ...
const CUT_OF_BUSY: f64 = 0.85;
const CUT_OF_USED: f64 = 0.9;
/// ... or this share of the used rate when stalls recur.
const STALL_CUT_OF_USED: f64 = 0.85;
/// A cut holds for a round trip and the queue, within these, ...
const HOLD_MIN: Duration = Duration::from_millis(100);
const HOLD_MAX: Duration = Duration::from_secs(1);
/// ... and until the used rate is within this multiple of the cut's target,
/// for at most `HOLD_MAX`.
const HOLD_USED: f64 = 1.1;
/// A rebase holds raises this long: the queue it took for the route's is
/// read against it first.
const RAISE_AFTER_REBASE: Duration = Duration::from_secs(1);
/// Raises: this share of the expected rate a second, or this within
/// `NEAR_CAPACITY` of the last busy rate cut for, plus `RAISE_ADD_BPS` a
/// second; that capacity is forgotten above `CAPACITY_CLEAR` times it.
const RAISE_PER_S: f64 = 0.15;
const RAISE_NEAR_PER_S: f64 = 0.04;
const NEAR_CAPACITY: f64 = 0.9;
const CAPACITY_CLEAR: f64 = 1.2;
const RAISE_ADD_BPS: f64 = 100_000.0;
/// A raise is applied once it adds this share of the expected rate.
const RAISE_APPLY: f64 = 0.05;
/// The most time one tick's raise covers.
const RAISE_DT_MAX: Duration = Duration::from_millis(500);
/// Raises need the used rate between these multiples of the expected one:
/// below, the picture is still or the encoder lags; above, the encoder
/// spends more than it is asked for.
const RAISE_USED_MIN: f64 = 0.5;
const RAISE_USED_MAX: f64 = 1.3;
/// Once an episode has ended and the queue stayed under `QUEUE_LOW` for
/// `RECOVER_QUIET`, raises run at this share a second toward `RECOVER_SHARE`
/// of the expected rate the episode began at, once an episode.
const RECOVER_PER_S: f64 = 0.5;
const RECOVER_SHARE: f64 = 0.9;
const RECOVER_QUIET: Duration = Duration::from_millis(500);
/// Stalls recur when `STALLS_RECURRING` end within `STALL_SPAN`, or two or
/// more that cover `STALL_SHARE` of it.
const STALL_SPAN: Duration = Duration::from_secs(2);
const STALLS_RECURRING: usize = 3;
const STALL_SHARE: f64 = 0.15;
/// The stalls kept: enough to count `STALLS_RECURRING`.
const STALLS_KEPT: usize = 4;
/// The probe controller yields to the receipts for this long after a cut or
/// a rebase, and while an episode lasts.
pub(crate) const OWNED_AFTER: Duration = Duration::from_secs(2);

const _: () = assert!(
    RING >= 2 && STANDING_RECEIPTS >= 1,
    "the ring holds a pair, and the standing queue a minimum"
);
const _: () = assert!(
    BUSY_SAMPLES_MIN >= 1 && BUSY_SAMPLES_MIN <= BUSY_SAMPLES,
    "busy_bps takes the median of BUSY_SAMPLES_MIN to BUSY_SAMPLES samples"
);
const _: () = assert!(
    STANDING_SPAN_MIN.as_nanos() <= STANDING_SPAN_MAX.as_nanos()
        && 2 * STANDING_SPAN_MAX.as_nanos() <= HISTORY.as_nanos(),
    "standing_window clamps its span between them, and the history holds a window before it"
);
const _: () = assert!(COMPRESSED_DIVISOR >= 1, "a frame interval is divided by it");
const _: () = assert!(
    QUEUE_LOW.as_nanos() < QUEUE_HIGH.as_nanos(),
    "a clear link is below a congested one"
);
const _: () = assert!(
    HOLD_MIN.as_nanos() <= HOLD_MAX.as_nanos(),
    "a hold is clamped between them"
);
const _: () = assert!(
    DRAINING < 1.0 && NO_EFFECT > 1.0 && NO_EFFECT_CUTS >= 1,
    "a drain lowers the queue, and no effect leaves it about where it was"
);
const _: () = assert!(
    CUT_OF_BUSY < 1.0 && CUT_OF_USED < 1.0 && STALL_CUT_OF_USED < 1.0,
    "a cut lowers the rate below what the link carried"
);
const _: () = assert!(
    RAISE_NEAR_PER_S < RAISE_PER_S && RAISE_PER_S < RECOVER_PER_S,
    "raises slow near capacity and speed up to recover"
);
const _: () = assert!(
    NEAR_CAPACITY < 1.0 && CAPACITY_CLEAR > 1.0 && RECOVER_SHARE <= 1.0,
    "the capacity band surrounds the capacity"
);
const _: () = assert!(
    RAISE_USED_MIN < 1.0 && RAISE_USED_MAX > 1.0,
    "an encoder that spends what it is asked for may be raised"
);
const _: () = assert!(
    STALLS_RECURRING >= 2 && STALLS_KEPT >= STALLS_RECURRING,
    "recurring stalls are counted from the ones kept"
);

/// One receipt, as the controller keeps it.
#[derive(Clone, Copy, Debug)]
struct Sample {
    /// On the clock receipts are timed by: since the controller began on the
    /// host's, or the viewer's arrival time.
    at: Duration,
    /// From the host's write to the receipt, or the queue toward the viewer.
    delay: Duration,
    bytes: u64,
    behind_idr: bool,
}

/// How the newest receipt arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Steady,
    /// At least `STALL_FRAMES` frame intervals after the one before.
    Gap,
    /// After a gap, or after such a receipt, compressed or with less delay
    /// than the one before: what the link held, delivered.
    Burst,
}

/// What a receipt decided.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Verdict {
    None,
    /// The queue is draining: nothing more until the hold passes.
    Hold,
    /// Cut the expected rate to this many bit/s.
    Cut(f64),
    /// Two cuts left the queue where it was: the receipts' baseline is now
    /// this delay, on the receipts' clock (a round trip, or a queue toward
    /// the viewer with arrival times).
    Rebased(Duration),
}

/// What the controller is told with each receipt and tick.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Inputs {
    /// The probes' base RTT; with arrival times, unused.
    pub base: Option<Duration>,
    /// The capture pace now, for the frame interval.
    pub pace: u32,
    /// Bits per frame × the pace: what the viewer is expected to receive.
    pub expected_bps: f64,
    /// The last probe round trip, for the hold.
    pub rtt: Option<Duration>,
    /// A clamped window refused a unit since the last receipt.
    pub blocked: bool,
}

/// The controller's values for the trace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReceiptRateView {
    pub standing: Option<Duration>,
    pub busy_bps: Option<u64>,
    pub used_bps: Option<u64>,
    /// Cuts since the session began.
    pub cuts: u64,
    /// An episode is under way.
    pub episode: bool,
}

/// The standing queue, and what the receipts it is judged on show.
#[derive(Clone, Copy, Debug)]
struct Measure {
    standing: Duration,
    /// The standing queue over the receipts before those.
    before: Option<Duration>,
    busy: Option<u64>,
    /// A receipt the standing queue is judged on was behind an IDR.
    behind_idr: bool,
    /// The least delay of those receipts less each unit's transfer, which a
    /// rebase takes ...
    least: Duration,
    /// ... and the newest receipt's, which lowers it.
    newest: Duration,
}

/// A cut, or a drain, holding the next decision.
#[derive(Clone, Copy, Debug)]
struct Hold {
    since: Instant,
    /// The least time it holds.
    until: Instant,
    /// After `until`, it holds while the used rate is above `HOLD_USED` ×
    /// this, up to `HOLD_MAX` after `since`.
    target: Option<f64>,
}

/// From the first cut until the queue is clear again.
#[derive(Clone, Copy, Debug)]
struct Episode {
    cuts: u32,
    /// The standing queue at the last cut or drain.
    last: Duration,
    /// The busy rate of the last cut.
    busy: f64,
}

/// Where a recovery stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Climb {
    #[default]
    Idle,
    /// Due once the queue has been clear for `RECOVER_QUIET`.
    Armed,
    Climbing,
}

/// The climb back after congestion.
#[derive(Clone, Copy, Debug, Default)]
struct Recovery {
    /// `RECOVER_SHARE` × the expected rate before the congestion began,
    /// until a raise reaches it.
    to: Option<f64>,
    climb: Climb,
    /// A climb was cut short below `to`: none again until a raise passes
    /// `CAPACITY_CLEAR` × the capacity, which shows the link carries more.
    cut_short: bool,
}

/// One stall: where it began on the receipts' clock, and how much longer
/// than a frame interval it lasted.
#[derive(Clone, Copy, Debug, Default)]
struct Stall {
    began: Duration,
    length: Duration,
}

/// Cuts for recurring stalls, while they recur.
#[derive(Clone, Copy, Debug, Default)]
struct StallCuts {
    cuts: u32,
    /// The stalled share of `STALL_SPAN` at the last cut.
    share: f64,
    /// Two cuts left the stalls as they were: they are not the host's.
    excused: bool,
}

/// What the raises have built up since they last applied one.
#[derive(Clone, Copy, Debug, Default)]
struct Raise {
    since: Option<Instant>,
    share: f64,
    added: f64,
}

pub(crate) struct ReceiptRate {
    /// The newest receipts within `HISTORY` of the newest, oldest first.
    samples: VecDeque<Sample>,
    /// When the last receipt came, on the samples' clock, and its delay,
    /// kept when the history is trimmed.
    last: Option<(Duration, Duration)>,
    /// When the newest sample came, on the host's clock.
    newest: Option<Instant>,
    flow: Flow,
    /// The host's clock starts at the first receipt.
    epoch: Option<Instant>,
    /// Receipts are timed by the viewer's arrival times.
    one_way: bool,
    /// The receipts' own baseline, on their clock, once a rebase set it: the
    /// least delay, less the unit's transfer, of the receipts it found the
    /// queue standing on, lowered by every receipt below it.
    rebase: Option<Duration>,
    /// When the last rebase came.
    rebased_at: Option<Instant>,
    /// The busy rate the last receipt found.
    busy: Option<u64>,
    /// Since when the standing queue has been under `QUEUE_LOW`.
    quiet_since: Option<Instant>,
    hold: Option<Hold>,
    episode: Option<Episode>,
    /// The busy rate of the last cut, until a raise passes it.
    capacity: Option<f64>,
    raise: Raise,
    recovery: Recovery,
    /// The newest stalls, a ring.
    stalls: [Stall; STALLS_KEPT],
    next_stall: usize,
    stall_cuts: StallCuts,
    /// Cuts since the session began.
    cuts: u64,
    /// The last cut or rebase.
    acted: Option<Instant>,
}
impl ReceiptRate {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(RING),
            last: None,
            newest: None,
            flow: Flow::Steady,
            epoch: None,
            one_way: false,
            rebase: None,
            rebased_at: None,
            busy: None,
            quiet_since: None,
            hold: None,
            episode: None,
            capacity: None,
            raise: Raise::default(),
            recovery: Recovery::default(),
            stalls: [Stall::default(); STALLS_KEPT],
            next_stall: 0,
            stall_cuts: StallCuts::default(),
            cuts: 0,
            acted: None,
        }
    }

    /// From now on, time receipts by the viewer's arrival times.
    pub fn use_one_way(&mut self) {
        self.reset();
        self.one_way = true;
    }

    /// Start again after a pause: the receipts, the episode, the hold, the
    /// raises, the stalls and the rebase, as the one-way minima start again
    /// too. The capacity is kept.
    pub fn restart(&mut self) {
        self.samples.clear();
        self.last = None;
        self.newest = None;
        self.flow = Flow::Steady;
        self.rebase = None;
        self.rebased_at = None;
        self.busy = None;
        self.quiet_since = None;
        self.hold = None;
        self.episode = None;
        self.raise = Raise::default();
        self.recovery = Recovery::default();
        self.stalls = [Stall::default(); STALLS_KEPT];
        self.next_stall = 0;
        self.stall_cuts = StallCuts::default();
        self.acted = None;
    }

    /// Forget everything the link showed, the capacity too: a new path or a
    /// new preset. The cut count is the session's.
    pub fn reset(&mut self) {
        self.restart();
        self.capacity = None;
    }

    /// With arrival times, delays are queues already: no base to take off.
    fn base(&self, base: Option<Duration>) -> Option<Duration> {
        if self.one_way {
            Some(Duration::ZERO)
        } else {
            base
        }
    }

    /// The baseline: the probes' base, or the receipts' own when higher.
    fn baseline(&self, base: Duration) -> Duration {
        base.max(self.rebase.unwrap_or(Duration::ZERO))
    }

    /// One receipt at `received`.
    pub fn on_receipt(&mut self, receipt: Receipt, received: Instant, inputs: Inputs) -> Verdict {
        let (at, delay) = match (self.one_way, receipt.arrival) {
            (true, Some(arrival)) => (arrival.at, arrival.forward_queue),
            (true, None) => return Verdict::None,
            (false, _) => (
                received.saturating_duration_since(*self.epoch.get_or_insert(received)),
                receipt.delay,
            ),
        };
        let frame = frame_interval(inputs.pace);
        let base = self.base(inputs.base);
        let above = base.map_or(Duration::ZERO, |base| {
            delay.saturating_sub(self.baseline(base))
        });
        self.follow(at, delay, above, frame);
        self.newest = Some(received);
        // Popping first keeps the ring within the capacity reserved for it.
        while self.samples.len() >= RING
            || self
                .samples
                .front()
                .is_some_and(|oldest| at.saturating_sub(oldest.at) > HISTORY)
        {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            at,
            delay,
            bytes: receipt.bytes,
            behind_idr: receipt.behind_idr,
        });
        let Some(base) = base else {
            return Verdict::None;
        };
        let Some(measure) = self.measure(base, frame) else {
            return Verdict::None;
        };
        // The receipts' own baseline only ever falls between rebases, and
        // goes once it is no higher than the probes'.
        if let Some(rebase) = self.rebase
            && measure.newest < rebase
        {
            self.rebase = (measure.newest > base).then_some(measure.newest);
        }
        self.busy = measure.busy;
        if self.flow == Flow::Burst {
            self.quiet_since = None;
            return Verdict::None;
        }
        if measure.standing < QUEUE_LOW {
            self.quiet_since.get_or_insert(received);
        } else {
            self.quiet_since = None;
        }
        if measure.behind_idr {
            return Verdict::None;
        }
        self.decide(measure, at, received, inputs)
    }

    /// Record how the receipt at `at` arrived, and a stall it ends. `above` is
    /// its delay above the receipts' least: a stall's first receipt waited
    /// through it, so at least half the gap beyond a frame interval; a unit
    /// sent after a still picture's idle gap waited for nothing.
    fn follow(&mut self, at: Duration, delay: Duration, above: Duration, frame: Duration) {
        if let Some((last_at, last_delay)) = self.last {
            let gap = at.saturating_sub(last_at);
            let held = self.flow != Flow::Steady
                && (gap < frame / COMPRESSED_DIVISOR || delay < last_delay);
            self.flow = if gap >= frame * STALL_FRAMES && above >= gap.saturating_sub(frame) / 2 {
                if let Some(slot) = self.stalls.get_mut(self.next_stall) {
                    *slot = Stall {
                        began: last_at,
                        length: gap.saturating_sub(frame),
                    };
                }
                self.next_stall = (self.next_stall + 1) % STALLS_KEPT;
                Flow::Gap
            } else if held {
                Flow::Burst
            } else {
                Flow::Steady
            };
        }
        self.last = Some((at, delay));
    }

    /// Whether stalls recur as of `at`, and the share of `STALL_SPAN` they
    /// covered.
    fn stalled(&self, at: Duration) -> (bool, f64) {
        let from = at.saturating_sub(STALL_SPAN);
        let (count, length) = self
            .stalls
            .iter()
            .filter(|stall| stall.length > Duration::ZERO && stall.began + stall.length >= from)
            .fold((0, Duration::ZERO), |(count, length), stall| {
                let began = stall.began.max(from);
                (count + 1, length + (stall.began + stall.length - began))
            });
        let share = length.as_secs_f64() / STALL_SPAN.as_secs_f64();
        (
            count >= STALLS_RECURRING || (count >= 2 && share >= STALL_SHARE),
            share,
        )
    }

    /// The decision on a receipt the stall rule and the IDR rule let through.
    fn decide(&mut self, measure: Measure, at: Duration, now: Instant, inputs: Inputs) -> Verdict {
        let standing = measure.standing;
        let (recurring, share) = self.stalled(at);
        if !recurring {
            self.stall_cuts = StallCuts::default();
        }
        let queued = standing > QUEUE_HIGH || (inputs.blocked && standing > QUEUE_LOW);
        let stalled = recurring && !self.stall_cuts.excused;
        if !queued {
            if standing < QUEUE_LOW && self.episode.take().is_some() {
                self.arm_recovery();
            }
            if !stalled {
                return Verdict::None;
            }
        }
        let used = self.used_bps().map(|used| used as f64);
        if self.held(now, used) {
            return Verdict::None;
        }
        if queued {
            let draining = measure
                .before
                .is_some_and(|before| standing + DRAIN_MARGIN < before)
                || self.episode.is_some_and(|episode| {
                    standing.as_secs_f64() < DRAINING * episode.last.as_secs_f64()
                });
            if draining {
                if let Some(episode) = &mut self.episode {
                    episode.last = standing;
                }
                self.hold = Some(Hold {
                    since: now,
                    until: now + hold_time(inputs, standing),
                    target: None,
                });
                return Verdict::Hold;
            }
            if self.episode.is_some_and(|episode| {
                episode.cuts >= NO_EFFECT_CUTS
                    && standing.as_secs_f64() <= NO_EFFECT * episode.last.as_secs_f64()
            }) {
                return self.rebase(measure.least, now);
            }
        } else if self.stall_cuts.cuts >= NO_EFFECT_CUTS
            && share <= NO_EFFECT * self.stall_cuts.share
        {
            self.stall_cuts.excused = true;
            return Verdict::None;
        }
        let busy = measure.busy.map(|busy| busy as f64);
        let target = match (queued, stalled, busy, used) {
            (_, true, _, Some(used)) => {
                let target = STALL_CUT_OF_USED * used;
                match busy.filter(|_| queued) {
                    Some(busy) => target.min(CUT_OF_BUSY * busy),
                    None => target,
                }
            }
            (true, _, Some(busy), used) => {
                (CUT_OF_BUSY * busy).min(used.map_or(f64::INFINITY, |used| CUT_OF_USED * used))
            }
            // No busy rate: no blind cut; the probe controller remains.
            _ => return Verdict::None,
        };
        // A cut to about the rate already asked for changes nothing, so it is
        // no evidence for a rebase either: the encoder has yet to follow the
        // last one.
        if target >= (1.0 - RAISE_APPLY) * inputs.expected_bps {
            self.hold = Some(Hold {
                since: now,
                until: now + hold_time(inputs, standing),
                target: Some(target),
            });
            return Verdict::Hold;
        }
        if queued {
            let episode = self.episode.get_or_insert(Episode {
                cuts: 0,
                last: standing,
                busy: busy.unwrap_or(target),
            });
            episode.cuts += 1;
            episode.last = standing;
            if let Some(busy) = busy {
                episode.busy = busy;
                self.capacity = Some(busy);
            }
        }
        if stalled {
            self.stall_cuts.cuts += 1;
            self.stall_cuts.share = share;
        }
        // Congestion began at the rate the session ran at: a recovery climbs
        // back toward it. A climb that this cut stops is cut short.
        let recovery = &mut self.recovery;
        recovery
            .to
            .get_or_insert(RECOVER_SHARE * inputs.expected_bps);
        if recovery.climb != Climb::Idle {
            recovery.cut_short = true;
            recovery.climb = Climb::Idle;
        }
        self.raise = Raise::default();
        self.hold = Some(Hold {
            since: now,
            until: now + hold_time(inputs, standing),
            target: Some(target),
        });
        self.cuts += 1;
        self.acted = Some(now);
        Verdict::Cut(target)
    }

    /// The baseline moves onto `least`: the queue the cuts left is the route,
    /// and the rate they took is recovered.
    fn rebase(&mut self, least: Duration, now: Instant) -> Verdict {
        self.rebase = Some(least);
        self.rebased_at = Some(now);
        self.episode = None;
        self.hold = None;
        self.recovery.cut_short = false;
        self.arm_recovery();
        self.acted = Some(now);
        Verdict::Rebased(least)
    }

    /// An episode ended: climb back, unless the last climb was cut short.
    fn arm_recovery(&mut self) {
        let recovery = &mut self.recovery;
        if recovery.to.is_some() && !recovery.cut_short {
            recovery.climb = Climb::Armed;
        }
    }

    /// Whether a hold still stands at `now`; one that passed is dropped.
    fn held(&mut self, now: Instant, used: Option<f64>) -> bool {
        let Some(hold) = self.hold else {
            return false;
        };
        let spending = hold
            .target
            .zip(used)
            .is_some_and(|(target, used)| used > HOLD_USED * target);
        let held = now < hold.until || (spending && now < hold.since + HOLD_MAX);
        if !held {
            self.hold = None;
        }
        held
    }

    /// The QoS tick: the expected rate to raise to, if a raise is due. Raises
    /// need fresh receipts, no hold, episode or recurring stalls, a standing
    /// queue under `QUEUE_LOW`, a baseline that has not just risen, and an
    /// encoder that spends about what it is asked for.
    pub fn raise(&mut self, now: Instant, inputs: Inputs) -> Option<f64> {
        let expected = inputs.expected_bps;
        let used = self.used_bps().map(|used| used as f64);
        let clear = self.fresh(now)
            && self
                .base(inputs.base)
                .and_then(|base| self.measure(base, frame_interval(inputs.pace)))
                .is_some_and(|measure| measure.standing < QUEUE_LOW)
            && self.episode.is_none()
            && !self.held(now, used)
            && self
                .last
                .is_none_or(|(at, _)| !self.stalled(at).0 || self.stall_cuts.excused)
            && self
                .rebased_at
                .is_none_or(|at| now.saturating_duration_since(at) >= RAISE_AFTER_REBASE)
            && expected > 0.0
            && used.is_some_and(|used| {
                (RAISE_USED_MIN * expected..=RAISE_USED_MAX * expected).contains(&used)
            });
        if !clear {
            self.raise = Raise::default();
            return None;
        }
        let quiet = self
            .quiet_since
            .is_some_and(|quiet| now.saturating_duration_since(quiet) >= RECOVER_QUIET);
        let recovery = &mut self.recovery;
        if recovery.to.is_some_and(|to| expected >= to) {
            *recovery = Recovery::default();
        } else if recovery.climb == Climb::Armed && quiet {
            recovery.climb = Climb::Climbing;
        }
        let climbing = recovery.to.filter(|_| recovery.climb == Climb::Climbing);
        let per_s = if climbing.is_some() {
            RECOVER_PER_S
        } else if self
            .capacity
            .is_some_and(|capacity| expected >= NEAR_CAPACITY * capacity)
        {
            RAISE_NEAR_PER_S
        } else {
            RAISE_PER_S
        };
        let since = self.raise.since.replace(now)?;
        let dt = now
            .saturating_duration_since(since)
            .min(RAISE_DT_MAX)
            .as_secs_f64();
        self.raise.share += per_s * dt;
        self.raise.added += RAISE_ADD_BPS * dt;
        if self.raise.share * expected + self.raise.added < RAISE_APPLY * expected {
            return None;
        }
        let mut next = expected * (1.0 + self.raise.share) + self.raise.added;
        self.raise.share = 0.0;
        self.raise.added = 0.0;
        if let Some(to) = climbing
            && next >= to
        {
            next = to.max(expected);
            self.recovery = Recovery::default();
        }
        // Well past the last capacity with no queue: the link carries more
        // now, as when a limit lifts, and a climb cut short may climb again.
        if self
            .capacity
            .is_some_and(|capacity| next > CAPACITY_CLEAR * capacity)
        {
            self.capacity = None;
            self.recovery.cut_short = false;
            self.arm_recovery();
        }
        Some(next)
    }

    /// A receipt came within `HISTORY` of `now`.
    fn fresh(&self, now: Instant) -> bool {
        !self.samples.is_empty()
            && self
                .newest
                .is_some_and(|newest| now.saturating_duration_since(newest) <= HISTORY)
    }

    /// Whether the probe controller yields frame rate cuts to the receipts:
    /// while an episode lasts, and for `OWNED_AFTER` after a cut or a rebase,
    /// as long as receipts come.
    pub fn owns(&self, now: Instant) -> bool {
        self.fresh(now)
            && (self.episode.is_some()
                || self
                    .acted
                    .is_some_and(|acted| now.saturating_duration_since(acted) <= OWNED_AFTER))
    }

    /// The rate the byte window follows while an episode lasts: the newest
    /// busy rate, or the last cut's.
    pub fn clamp_rate(&self, now: Instant) -> Option<u64> {
        let episode = self.episode.filter(|_| self.fresh(now))?;
        Some(self.busy.unwrap_or(episode.busy as u64))
    }

    /// Whether a receipt came at or after `since`.
    pub fn sampled_since(&self, since: Instant) -> bool {
        self.newest.is_some_and(|newest| newest >= since)
    }

    /// The values at `now` for the trace. Without a receipt in the last
    /// second only the counts are set.
    pub fn view(&self, now: Instant, base: Option<Duration>, pace: u32) -> ReceiptRateView {
        let counts = ReceiptRateView {
            cuts: self.cuts,
            episode: self.episode.is_some(),
            ..ReceiptRateView::default()
        };
        if !self.fresh(now) {
            return counts;
        }
        let measure = self
            .base(base)
            .and_then(|base| self.measure(base, frame_interval(pace)));
        ReceiptRateView {
            standing: measure.map(|measure| measure.standing),
            busy_bps: measure.and_then(|measure| measure.busy),
            used_bps: self.used_bps(),
            ..counts
        }
    }

    /// The standing queue and what it is judged on; `None` while the history
    /// is shorter than its window.
    fn measure(&self, base: Duration, frame: Duration) -> Option<Measure> {
        let window = self.standing_window(frame)?;
        let baseline = self.baseline(base);
        let busy = self.busy_bps(baseline, frame);
        let allowance =
            |sample: &Sample| busy.map_or(frame, |busy| transfer(sample.bytes, busy).min(frame));
        let newest = || self.samples.iter().rev().take(window);
        let standing = queue(newest(), baseline, allowance)?;
        let before = (self.samples.len() >= 2 * window)
            .then(|| {
                queue(
                    self.samples.iter().rev().skip(window).take(window),
                    baseline,
                    allowance,
                )
            })
            .flatten();
        let excess = |sample: &Sample| sample.delay.saturating_sub(allowance(sample));
        Some(Measure {
            standing,
            before,
            busy,
            behind_idr: newest().any(|sample| sample.behind_idr),
            least: newest().map(excess).min()?,
            newest: self.samples.back().map(excess)?,
        })
    }

    /// How many of the newest receipts a standing queue is judged on; `None`
    /// while the history is shorter.
    fn standing_window(&self, frame: Duration) -> Option<usize> {
        let span = (frame * STANDING_FRAMES).clamp(STANDING_SPAN_MIN, STANDING_SPAN_MAX);
        let newest = self.samples.back()?.at;
        self.samples
            .iter()
            .rev()
            .enumerate()
            .find(|(index, sample)| {
                index + 1 >= STANDING_RECEIPTS && newest.saturating_sub(sample.at) >= span
            })
            .map(|(index, _)| index + 1)
    }

    /// The median rate, lower of two, over the newest samples across which
    /// the link stayed busy.
    fn busy_bps(&self, baseline: Duration, frame: Duration) -> Option<u64> {
        let compressed = frame / COMPRESSED_DIVISOR;
        let mut rates = [0u64; BUSY_SAMPLES];
        let mut found = 0;
        // Receipts compressed onto the newest of their group: its bytes, and
        // its delay and time, which end the sample.
        let mut group: Option<(u64, &Sample)> = None;
        for (later, earlier) in self
            .samples
            .iter()
            .rev()
            .zip(self.samples.iter().rev().skip(1))
        {
            let (bytes, end) = group.map_or((later.bytes, later), |(bytes, end)| {
                (bytes.saturating_add(later.bytes), end)
            });
            if later.at.saturating_sub(earlier.at) < compressed {
                group = Some((bytes, end));
                continue;
            }
            group = None;
            let gap = end.at.saturating_sub(earlier.at);
            if end.delay.saturating_sub(baseline) < gap {
                continue;
            }
            let Some(slot) = rates.get_mut(found) else {
                break;
            };
            *slot = bits_per_second(bytes, gap);
            found += 1;
        }
        if found < BUSY_SAMPLES_MIN {
            return None;
        }
        let rates = rates.get_mut(..found)?;
        rates.sort_unstable();
        rates.get((found - 1) / 2).copied()
    }

    /// Bits per second over the history, from its first receipt to its last.
    fn used_bps(&self) -> Option<u64> {
        let first = self.samples.front()?.at;
        let span = self.samples.back()?.at.saturating_sub(first);
        if span < USED_SPAN_MIN {
            return None;
        }
        // The first receipt's bytes crossed before the span began.
        let bytes = self
            .samples
            .iter()
            .skip(1)
            .fold(0u64, |bytes, sample| bytes.saturating_add(sample.bytes));
        Some(bits_per_second(bytes, span))
    }
}

/// How long a cut or a drain holds: a round trip and the queue, within
/// `HOLD_MIN` and `HOLD_MAX`.
fn hold_time(inputs: Inputs, standing: Duration) -> Duration {
    let rtt = inputs.rtt.or(inputs.base).unwrap_or(HOLD_MIN);
    (rtt + standing).clamp(HOLD_MIN, HOLD_MAX)
}

/// One frame interval at `pace` fps; a second while nothing is paced.
fn frame_interval(pace: u32) -> Duration {
    Duration::from_secs(1) / pace.max(1)
}

/// The least delay above `baseline` of `samples`, each less its
/// `allowance`.
fn queue<'a>(
    samples: impl Iterator<Item = &'a Sample>,
    baseline: Duration,
    allowance: impl Fn(&Sample) -> Duration,
) -> Option<Duration> {
    samples
        .map(|sample| {
            sample
                .delay
                .saturating_sub(baseline)
                .saturating_sub(allowance(sample))
        })
        .min()
}

/// How long `bytes` take at `bps`.
fn transfer(bytes: u64, bps: u64) -> Duration {
    let nanos = (u128::from(bytes) * 8_000_000_000)
        .checked_div(u128::from(bps))
        .unwrap_or(u128::MAX);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// `bytes` over `span`, in bits per second.
fn bits_per_second(bytes: u64, span: Duration) -> u64 {
    let bps = (u128::from(bytes) * 8_000_000_000)
        .checked_div(span.as_nanos())
        .unwrap_or(u128::MAX);
    u64::try_from(bps).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_millis(20);
    /// 60 fps: a frame interval of 16.7 ms.
    const PACE: u32 = 60;

    /// Receipts fed at chosen times, timed by the host, as the QoS controller
    /// feeds them.
    struct Feed {
        t0: Instant,
        rate: ReceiptRate,
        now: Instant,
        /// What the controller expects the viewer to receive.
        expected: f64,
        blocked: bool,
        verdicts: Vec<Verdict>,
    }
    impl Feed {
        fn new(expected: f64) -> Self {
            let t0 = Instant::now();
            Self {
                t0,
                rate: ReceiptRate::new(),
                now: t0,
                expected,
                blocked: false,
                verdicts: Vec::new(),
            }
        }

        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }

        fn inputs(&self) -> Inputs {
            Inputs {
                base: Some(BASE),
                pace: PACE,
                expected_bps: self.expected,
                rtt: Some(BASE),
                blocked: self.blocked,
            }
        }

        /// A receipt at `at_ms` of `bytes` whose unit took `queue_ms` more
        /// than the base RTT. A cut is applied to `expected`.
        fn receipt(&mut self, at_ms: u64, queue_ms: u64, bytes: u64) -> Verdict {
            self.receipt_with(at_ms, queue_ms, bytes, false)
        }

        fn receipt_with(
            &mut self,
            at_ms: u64,
            queue_ms: u64,
            bytes: u64,
            behind_idr: bool,
        ) -> Verdict {
            self.now = self.at(at_ms);
            let receipt = Receipt {
                delay: BASE + Duration::from_millis(queue_ms),
                bytes,
                behind_idr,
                arrival: None,
            };
            let verdict = self.rate.on_receipt(receipt, self.now, self.inputs());
            if let Verdict::Cut(target) = verdict {
                self.expected = self.expected.min(target);
            }
            self.verdicts.push(verdict);
            verdict
        }

        /// `count` receipts every `every_ms` from `from_ms`.
        fn steady(&mut self, from_ms: u64, count: u64, every_ms: u64, queue_ms: u64, bytes: u64) {
            for n in 0..count {
                self.receipt(from_ms + n * every_ms, queue_ms, bytes);
            }
        }

        /// The QoS tick at `at_ms`: a raise is applied to `expected`.
        fn tick(&mut self, at_ms: u64) -> Option<f64> {
            self.now = self.at(at_ms);
            let next = self.rate.raise(self.now, self.inputs());
            if let Some(next) = next {
                self.expected = next;
            }
            next
        }

        fn cuts(&self) -> Vec<f64> {
            self.verdicts
                .iter()
                .filter_map(|verdict| match verdict {
                    Verdict::Cut(target) => Some(*target),
                    _ => None,
                })
                .collect()
        }

        fn view(&self) -> ReceiptRateView {
            self.rate.view(self.now, Some(BASE), PACE)
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_standing_queue_cuts_to_85_percent_of_the_busy_rate_within_a_round_trip() {
        // A 2 Mbit/s link with 100 ms of queue: a 4250-byte unit every 17 ms.
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 5, 17, 100, 4250);
        // The fourth receipt shows three busy pairs: 85% of 2 Mbit/s.
        assert_eq!(feed.cuts(), [1_700_000.0]);
        assert!(
            matches!(feed.verdicts[3], Verdict::Cut(_)),
            "{:?}",
            feed.verdicts
        );
        let view = feed.view();
        assert_eq!(view.busy_bps, Some(2_000_000));
        // Its own transfer, a frame interval at most, is no queue.
        assert!(
            view.standing
                .is_some_and(|standing| standing > ms(80) && standing < ms(85)),
            "{view:?}"
        );
        assert_eq!((view.cuts, view.episode), (1, true));
        // The byte window follows the busy rate, and the probes yield.
        assert_eq!(feed.rate.clamp_rate(feed.now), Some(2_000_000));
        assert!(feed.rate.owns(feed.now));

        // Units behind an IDR in the window decide nothing.
        let mut feed = Feed::new(4_000_000.0);
        for n in 0..20 {
            feed.receipt_with(17 * n, 100, 4250, true);
        }
        assert!(feed.cuts().is_empty());

        // 50 ms of queue is above 40, but not once each unit's own transfer
        // (17 ms at this rate, at most a frame interval) is taken off.
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 80, 17, 50, 4250);
        assert!(feed.cuts().is_empty());
        // ... unless a clamped window refused a unit meanwhile: above 10 ms,
        // that is congestion too.
        feed.blocked = true;
        feed.receipt(80 * 17, 50, 4250);
        assert_eq!(feed.cuts(), [1_700_000.0]);
        // A queue under 10 ms is none, blocked or not.
        let mut feed = Feed::new(4_000_000.0);
        feed.blocked = true;
        feed.steady(0, 80, 17, 20, 4250);
        assert!(feed.cuts().is_empty());
    }

    #[test]
    fn an_idle_link_has_no_busy_rate_and_never_cuts() {
        // A fast link carrying 4 KiB at 60 fps: each unit crosses in 2 ms, so
        // no pair shows the link busy between two receipts.
        let mut feed = Feed::new(2_000_000.0);
        feed.steady(0, 60, 17, 2, 4096);
        // One receipt a Wi-Fi hold kept 300 ms is no standing queue.
        feed.receipt(60 * 17, 300, 4096);
        feed.steady(61 * 17, 10, 17, 2, 4096);
        let view = feed.view();
        assert_eq!((view.standing, view.busy_bps), (Some(Duration::ZERO), None));
        assert!(
            view.used_bps
                .is_some_and(|used| (1_900_000..2_000_000).contains(&used))
        );
        assert!(
            feed.verdicts
                .iter()
                .all(|verdict| *verdict == Verdict::None)
        );

        // A second without receipts leaves only the counts ...
        let later = feed.now + HISTORY + ms(1);
        assert_eq!(
            feed.rate.view(later, Some(BASE), PACE),
            ReceiptRateView::default()
        );
        assert!(!feed.rate.owns(later));
        // ... and without a base RTT there is no queue to judge.
        let mut blind = ReceiptRate::new();
        let t0 = Instant::now();
        for n in 0..30 {
            let receipt = Receipt {
                delay: ms(400),
                bytes: 4250,
                behind_idr: false,
                arrival: None,
            };
            let inputs = Inputs {
                base: None,
                ..feed.inputs()
            };
            let verdict = blind.on_receipt(receipt, t0 + ms(n * 17), inputs);
            assert_eq!(verdict, Verdict::None);
        }
        let view = blind.view(t0 + ms(29 * 17), None, PACE);
        assert_eq!((view.standing, view.busy_bps), (None, None));
        assert!(view.used_bps.is_some());
    }

    #[test]
    fn a_stall_burst_never_cuts() {
        // A 2 Mbit/s link near capacity: 4250 bytes every 17 ms, 20 ms each.
        let mut feed = Feed::new(2_000_000.0);
        feed.steady(0, 60, 17, 20, 4250);
        assert_eq!(feed.view().busy_bps, Some(2_000_000));
        // The link holds everything for 600 ms, then delivers the 36 units
        // sent meanwhile 3 ms apart: their delay falls 14 ms a receipt, and
        // past 50 ms into the burst its own receipts alone stand above 100 ms.
        let held = 59 * 17 + 600;
        for n in 0..36 {
            feed.receipt(held + 3 * n, 600 - 14 * n, 4250);
        }
        // The link carries on as before.
        let after = held + 3 * 35;
        for n in 1..=30 {
            feed.receipt(after + 17 * n, 20, 4250);
        }
        assert!(feed.cuts().is_empty(), "{:?}", feed.verdicts);
        // A backlog the link drains at its own pace, each receipt with less
        // delay than the one before, is the stall's too.
        let mut feed = Feed::new(2_000_000.0);
        feed.steady(0, 60, 17, 2, 4096);
        let held = 59 * 17 + 600;
        for n in 0..40 {
            feed.receipt(held + 8 * n, 600 - 15 * n, 4096);
        }
        feed.steady(held + 8 * 40, 30, 17, 2, 4096);
        assert!(feed.cuts().is_empty(), "{:?}", feed.verdicts);
        assert_eq!(feed.view().cuts, 0);
    }

    #[test]
    fn stalls_that_recur_cut_twice_then_are_excused_and_idle_gaps_never_count() {
        // 300 ms stalls every second on a fast link: each burst is excused,
        // but stalls that recur are congestion.
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 60, 17, 2, 8333);
        let mut at = 60 * 17;
        for _ in 0..8 {
            // The units sent during the stall arrive at once after it; each
            // is as large as the rate the host asks for.
            let bytes = (feed.expected / 60.0 / 8.0) as u64;
            at += 300;
            for n in 0..18 {
                feed.receipt(at + n, 300 - 16 * n, bytes);
            }
            at += 18;
            for _ in 0..40 {
                at += 17;
                feed.receipt(at, 2, bytes);
            }
        }
        let cuts = feed.cuts();
        assert_eq!(cuts.len(), 2, "{:?}", feed.verdicts);
        // Each to 85% of what the receipts delivered, at most.
        assert!(cuts[0] <= 0.85 * 4_000_000.0 + 1.0, "{cuts:?}");
        assert!(feed.rate.stall_cuts.excused);

        // A still picture's idle gaps carry no delay: no stall at all.
        let mut feed = Feed::new(4_000_000.0);
        let mut at = 0;
        for _ in 0..20 {
            feed.steady(at, 5, 17, 2, 8333);
            at += 5 * 17 + 500;
        }
        assert!(feed.cuts().is_empty());
        assert!(
            feed.rate
                .stalls
                .iter()
                .all(|stall| stall.length == Duration::ZERO)
        );
    }

    #[test]
    fn compressed_receipts_are_no_rate_sample() {
        // A 2 Mbit/s link whose receipts the return path bunches in threes:
        // 3750 bytes each, 1 ms apart, a group every 45 ms, 80 ms of queue.
        let mut feed = Feed::new(4_000_000.0);
        for group in 0..30 {
            for n in 0..3 {
                feed.receipt(group * 45 + n, 80, 3750);
            }
        }
        // Each pair 1 ms apart would claim 30 Mbit/s; each group is three
        // units delivered over the 45 ms since the one before.
        assert_eq!(feed.view().busy_bps, Some(2_000_000));
        // Bunching on a steady link is no stall: the queue still cuts.
        assert_eq!(feed.cuts().first(), Some(&1_700_000.0));
    }

    #[test]
    fn a_cut_holds_then_a_draining_queue_holds_and_no_cut_repeats_the_last() {
        // 2 Mbit/s with 100 ms of queue, until the cut.
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 5, 17, 100, 4250);
        assert_eq!(feed.cuts(), [1_700_000.0]);
        // The link still delivers at 2 Mbit/s with the same queue: the cut
        // holds for up to a second while what it delivers is above 1.1 × the
        // target ...
        feed.steady(85, 55, 17, 100, 4250);
        assert_eq!(feed.cuts().len(), 1, "{:?}", feed.verdicts);
        // ... and after it, a cut to the rate already asked for only holds
        // again: it is no evidence that the host's rate is not the cause.
        feed.steady(85 + 55 * 17, 10, 17, 100, 4250);
        assert_eq!(feed.cuts().len(), 1, "{:?}", feed.verdicts);
        assert!(feed.verdicts.contains(&Verdict::Hold));
        // A queue that drains holds, whatever the busy rate.
        let mut feed = Feed::new(8_000_000.0);
        feed.steady(0, 10, 17, 200, 4250);
        let cuts = feed.cuts().len();
        let from = 10 * 17 + 1100;
        for n in 0..12 {
            feed.receipt(from + 17 * n, 190 - 12 * n, 4250);
        }
        assert_eq!(feed.cuts().len(), cuts, "{:?}", feed.verdicts);
        assert!(feed.verdicts.contains(&Verdict::Hold));
    }

    #[test]
    fn two_cuts_that_leave_the_queue_rebase_onto_it() {
        // A longer route: every unit 120 ms later than the base, on a link
        // that is not otherwise full. The busy rate is what the host sends,
        // so each cut takes 15%, and the queue stays.
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 60, 17, 2, 8333);
        let mut at = 60 * 17;
        let mut rebased = None;
        for _ in 0..240 {
            at += 17;
            let bytes = (feed.expected / 60.0 / 8.0) as u64;
            if let Verdict::Rebased(level) = feed.receipt(at, 122, bytes) {
                rebased = Some(level);
            }
        }
        assert_eq!(feed.cuts().len(), 2, "{:?}", feed.verdicts);
        // The new baseline is the queue's least, less the units' transfer.
        let level = rebased.unwrap();
        assert!(
            level > BASE + ms(100) && level < BASE + ms(122),
            "{level:?}"
        );
        // The longer route then cuts nothing more.
        assert_eq!(feed.view().cuts, 2);
        assert!(
            feed.view()
                .standing
                .is_some_and(|standing| standing < QUEUE_LOW)
        );
        // A receipt below it lowers it: the route got shorter again.
        feed.receipt(at + 17, 2, 8333);
        assert_eq!(feed.rate.rebase, None);
    }

    #[test]
    fn raises_follow_use_in_5_percent_steps_slower_near_capacity() {
        // A clear 4 Mbit/s session: used is what is expected.
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 60, 17, 2, 8333);
        // The first tick only starts the raise; then 15% a second plus
        // 100 kbit/s a second, applied once it adds 5%.
        assert_eq!(feed.tick(1000), None);
        assert_eq!(feed.tick(1250), None);
        let next = feed.tick(1500).unwrap();
        assert!(
            (next - (4_000_000.0 * 1.075 + 50_000.0)).abs() < 1.0,
            "{next}"
        );
        // A still picture uses less than half of what is expected: no raise.
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 60, 17, 2, 2000);
        assert!((0..8).all(|n| feed.tick(1000 + 250 * n).is_none()));
        // Near the capacity of the last cut, 4% a second, so the 5% takes a
        // second where it took half of one.
        let mut feed = Feed::new(4_000_000.0);
        feed.rate.capacity = Some(4_200_000.0);
        feed.steady(0, 60, 17, 2, 8333);
        let raises: Vec<(u64, f64)> = (0..5)
            .filter_map(|n| feed.tick(1000 + 250 * n).map(|next| (n, next)))
            .collect();
        assert_eq!(raises, [(4, 4_000_000.0 * 1.04 + 100_000.0)]);
        // No raise inside an episode or a hold.
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 5, 17, 100, 4250);
        assert!((0..4).all(|n| feed.tick(100 + 250 * n).is_none()));
    }

    #[test]
    fn a_recovery_climbs_at_half_a_second_toward_where_congestion_began() {
        // 4 Mbit/s, then a 2 Mbit/s queue: a cut to 1.7 Mbit/s.
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 5, 17, 100, 4250);
        assert_eq!(feed.cuts(), [1_700_000.0]);
        // The queue clears and the link carries 1.7 Mbit/s with nothing
        // queued: the episode ends, and after 500 ms of a clear queue raises
        // climb at 50% a second toward 90% of the 4 Mbit/s it began at.
        let bytes = (1_700_000.0_f64 / 60.0 / 8.0) as u64;
        feed.steady(1200, 120, 17, 2, bytes);
        assert!(!feed.view().episode);
        let start = feed.expected;
        let mut at = 1200 + 17 * 120;
        let mut raises = Vec::new();
        for _ in 0..8 {
            let bytes = (feed.expected / 60.0 / 8.0) as u64;
            feed.steady(at, 15, 17, 2, bytes);
            at += 250;
            raises.extend(feed.tick(at));
        }
        assert!(feed.expected > 2.0 * start, "{raises:?}");
        assert!(feed.expected <= 3_600_000.0 + 1.0, "{raises:?}");
    }

    #[test]
    fn the_receipts_own_the_queue_for_two_seconds_after_a_cut() {
        let mut feed = Feed::new(4_000_000.0);
        feed.steady(0, 5, 17, 100, 4250);
        let cut = feed.now;
        // The queue clears: the episode ends, but the cut is recent.
        feed.steady(1200, 30, 17, 2, 4250);
        assert!(!feed.view().episode);
        assert!(feed.rate.owns(feed.now));
        assert_eq!(feed.rate.clamp_rate(feed.now), None);
        feed.steady(1200 + 30 * 17, 70, 17, 2, 4250);
        assert!(feed.now > cut + OWNED_AFTER);
        assert!(!feed.rate.owns(feed.now));
    }
}
