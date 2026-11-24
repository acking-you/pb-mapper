//! A receipt-based rate estimator, run in shadow: it reads every transport
//! receipt and decides nothing. Its values are traced once a second, so real
//! links show what a receipt-driven rate control would see before one acts on
//! it.
//!
//! - The standing queue is the least receipt delay above the base RTT over
//!   the newest receipts: at least three, spanning at least two frame
//!   intervals and 50 ms. A burst a few milliseconds long never makes one on
//!   its own.
//! - The busy rate is the median delivery rate over the newest receipt pairs
//!   across which the link stayed busy: the later unit's delay above the base
//!   RTT is at least the pair's gap. Receipts less than a quarter of a frame
//!   interval apart are compression, not delivery: such a pair is no sample,
//!   and its later receipt's bytes count toward the pair before it.
//! - The used rate is what the receipts covered over their newest second.
//! - A receipt would cut when the standing queue, less each unit's own
//!   transfer at the busy rate (at most a frame interval), is above
//!   `QUEUE_HIGH`, with a busy rate known and nothing in the window behind an
//!   IDR, except during a stall: the burst of receipts that crowd in after a
//!   receipt gap of at least three frame intervals.
//!
//! With arrival times (`arrival_time`) each receipt is timed on the viewer's
//! clock, when it read its newest unit, and its delay above the base RTT is
//! that unit's queue toward the viewer: the way back, and whatever bunches
//! receipts on it, are in neither. A receipt whose stamp was no sample is
//! left out. This is what a receipt-driven rate control reads.
//!
//! The history is a ring reserved once, so a receipt allocates nothing.

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
/// which the history always covers.
const STANDING_SPAN_MIN: Duration = Duration::from_millis(50);
const STANDING_SPAN_MAX: Duration = Duration::from_millis(500);
/// A receipt this many frame intervals after the one before follows a gap.
const STALL_FRAMES: u32 = 3;
/// Receipts closer than a frame interval divided by this are compressed.
const COMPRESSED_DIVISOR: u32 = 4;
/// The busy rate is the median of at most this many samples ...
const BUSY_SAMPLES: usize = 8;
/// ... and unknown with fewer than this many.
const BUSY_SAMPLES_MIN: usize = 3;
/// The used rate needs receipts spanning at least this.
const USED_SPAN_MIN: Duration = Duration::from_millis(100);
/// A standing queue above this, beyond the units' own transfer, would cut.
const QUEUE_HIGH: Duration = Duration::from_millis(40);

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
        && STANDING_SPAN_MAX.as_nanos() < HISTORY.as_nanos(),
    "standing_window clamps its span between them, within the history"
);
const _: () = assert!(COMPRESSED_DIVISOR >= 1, "a frame interval is divided by it");

/// One receipt, as the estimator keeps it.
#[derive(Clone, Copy, Debug)]
struct Sample {
    /// On the clock receipts are timed by: since the estimator began on the
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
    /// Compressed after a gap, or after such a receipt: what the link held,
    /// delivered at once.
    Burst,
}

/// The estimator's values for the trace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReceiptRateView {
    pub standing: Option<Duration>,
    pub busy_bps: Option<u64>,
    pub used_bps: Option<u64>,
    /// A receipt since the last view would have cut.
    pub would_cut: bool,
}

pub(crate) struct ReceiptRate {
    /// The newest receipts within `HISTORY` of the newest, oldest first.
    samples: VecDeque<Sample>,
    /// When the last receipt came, on the samples' clock, kept when the
    /// history is trimmed.
    last_at: Option<Duration>,
    /// When the newest sample came, on the host's clock.
    newest: Option<Instant>,
    flow: Flow,
    would_cut: bool,
    /// The host's clock starts at the first receipt.
    epoch: Option<Instant>,
    /// Receipts are timed by the viewer's arrival times.
    one_way: bool,
}
impl ReceiptRate {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(RING),
            last_at: None,
            newest: None,
            flow: Flow::Steady,
            would_cut: false,
            epoch: None,
            one_way: false,
        }
    }

    /// From now on, time receipts by the viewer's arrival times.
    pub fn use_one_way(&mut self) {
        self.clear();
        self.one_way = true;
    }

    /// Forget the receipts: they were measured on another path, or before a
    /// pause. A cut one of them would have made is still reported.
    pub fn clear(&mut self) {
        self.samples.clear();
        self.last_at = None;
        self.newest = None;
        self.flow = Flow::Steady;
    }

    /// With arrival times, delays are queues already: no base to take off.
    fn base(&self, base: Option<Duration>) -> Option<Duration> {
        if self.one_way {
            Some(Duration::ZERO)
        } else {
            base
        }
    }

    /// One receipt at `received`, while the host paces at `pace` fps.
    pub fn on_receipt(
        &mut self,
        receipt: Receipt,
        received: Instant,
        base: Option<Duration>,
        pace: u32,
    ) {
        let (at, delay) = match (self.one_way, receipt.arrival) {
            (true, Some(arrival)) => (arrival.at, arrival.forward_queue),
            (true, None) => return,
            (false, _) => (
                received.saturating_duration_since(*self.epoch.get_or_insert(received)),
                receipt.delay,
            ),
        };
        let base = self.base(base);
        let frame = frame_interval(pace);
        if let Some(last) = self.last_at {
            let gap = at.saturating_sub(last);
            self.flow = if gap >= frame * STALL_FRAMES {
                Flow::Gap
            } else if gap < frame / COMPRESSED_DIVISOR && self.flow != Flow::Steady {
                Flow::Burst
            } else {
                Flow::Steady
            };
        }
        self.last_at = Some(at);
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
        if self.flow != Flow::Burst
            && let Some(base) = base
            && self.cuts(base, frame)
        {
            self.would_cut = true;
        }
    }

    /// The values at `now`, and whether a receipt since the last call would
    /// have cut. Without a receipt in the last second only that flag is set.
    pub fn take_view(
        &mut self,
        now: Instant,
        base: Option<Duration>,
        pace: u32,
    ) -> ReceiptRateView {
        let would_cut = std::mem::take(&mut self.would_cut);
        let fresh = !self.samples.is_empty()
            && self
                .newest
                .is_some_and(|newest| now.saturating_duration_since(newest) <= HISTORY);
        if !fresh {
            return ReceiptRateView {
                would_cut,
                ..ReceiptRateView::default()
            };
        }
        let base = self.base(base);
        let frame = frame_interval(pace);
        ReceiptRateView {
            standing: base.and_then(|base| self.standing(base, frame)),
            busy_bps: base.and_then(|base| self.busy_bps(base, frame)),
            used_bps: self.used_bps(),
            would_cut,
        }
    }

    /// Whether the newest receipts show a queue a rate cut would answer.
    fn cuts(&self, base: Duration, frame: Duration) -> bool {
        let Some(window) = self.standing_window(frame) else {
            return false;
        };
        let newest = || self.samples.iter().rev().take(window);
        // The units' own transfer only lowers the queue, so below the
        // threshold no busy rate is needed.
        if newest().any(|sample| sample.behind_idr)
            || queue(newest(), base, |_| Duration::ZERO).is_none_or(|queue| queue <= QUEUE_HIGH)
        {
            return false;
        }
        let Some(busy) = self.busy_bps(base, frame) else {
            return false;
        };
        queue(newest(), base, |sample| {
            transfer(sample.bytes, busy).min(frame)
        })
        .is_some_and(|queue| queue > QUEUE_HIGH)
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

    fn standing(&self, base: Duration, frame: Duration) -> Option<Duration> {
        let window = self.standing_window(frame)?;
        queue(self.samples.iter().rev().take(window), base, |_| {
            Duration::ZERO
        })
    }

    /// The median rate, lower of two, over the newest samples across which
    /// the link stayed busy.
    fn busy_bps(&self, base: Duration, frame: Duration) -> Option<u64> {
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
            if end.delay.saturating_sub(base) < gap {
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

/// One frame interval at `pace` fps; a second while nothing is paced.
fn frame_interval(pace: u32) -> Duration {
    Duration::from_secs(1) / pace.max(1)
}

/// The least delay above `base` of `samples`, each less its `allowance`.
fn queue<'a>(
    samples: impl Iterator<Item = &'a Sample>,
    base: Duration,
    allowance: impl Fn(&Sample) -> Duration,
) -> Option<Duration> {
    samples
        .map(|sample| {
            sample
                .delay
                .saturating_sub(base)
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

    /// Receipts fed at chosen times, read back at the newest.
    struct Feed {
        t0: Instant,
        rate: ReceiptRate,
        now: Instant,
    }
    impl Feed {
        fn new() -> Self {
            let t0 = Instant::now();
            Self {
                t0,
                rate: ReceiptRate::new(),
                now: t0,
            }
        }

        /// A receipt at `at_ms` of `bytes` whose unit took `queue_ms` more
        /// than the base RTT.
        fn receipt(&mut self, at_ms: u64, queue_ms: u64, bytes: u64) {
            self.receipt_with(at_ms, queue_ms, bytes, false);
        }

        fn receipt_with(&mut self, at_ms: u64, queue_ms: u64, bytes: u64, behind_idr: bool) {
            self.now = self.t0 + Duration::from_millis(at_ms);
            let receipt = Receipt {
                delay: BASE + Duration::from_millis(queue_ms),
                bytes,
                behind_idr,
                arrival: None,
            };
            self.rate.on_receipt(receipt, self.now, Some(BASE), PACE);
        }

        /// `count` receipts every `every_ms` from `from_ms`.
        fn steady(&mut self, from_ms: u64, count: u64, every_ms: u64, queue_ms: u64, bytes: u64) {
            for n in 0..count {
                self.receipt(from_ms + n * every_ms, queue_ms, bytes);
            }
        }

        fn view(&mut self) -> ReceiptRateView {
            self.rate.take_view(self.now, Some(BASE), PACE)
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_standing_queue_would_cut_and_its_rate_is_the_links() {
        // A 2 Mbit/s link with 100 ms of queue: a 4250-byte unit every 17 ms.
        let mut feed = Feed::new();
        feed.steady(0, 80, 17, 100, 4250);
        let view = feed.view();
        assert_eq!(view.standing, Some(ms(100)));
        assert_eq!(view.busy_bps, Some(2_000_000));
        assert_eq!(view.used_bps, Some(2_000_000));
        assert!(view.would_cut);
        // The flag covers the receipts since the last view.
        assert!(!feed.view().would_cut);

        // A unit behind an IDR in the window decides nothing; once the window
        // has passed it, the queue cuts again.
        feed.receipt_with(80 * 17, 100, 4250, true);
        assert!(!feed.view().would_cut);
        feed.steady(81 * 17, 2, 17, 100, 4250);
        assert!(!feed.view().would_cut);
        feed.steady(83 * 17, 2, 17, 100, 4250);
        assert!(feed.view().would_cut);

        // 50 ms of queue is above 40, but not once each unit's own transfer
        // (17 ms at this rate, at most a frame interval) is taken off.
        let mut feed = Feed::new();
        feed.steady(0, 80, 17, 50, 4250);
        let view = feed.view();
        assert_eq!(view.standing, Some(ms(50)));
        assert!(!view.would_cut);
    }

    #[test]
    fn an_idle_link_has_no_busy_rate_and_never_cuts() {
        // A fast link carrying 4 KiB at 60 fps: each unit crosses in 2 ms, so
        // no pair shows the link busy between two receipts.
        let mut feed = Feed::new();
        feed.steady(0, 60, 17, 2, 4096);
        // One receipt a Wi-Fi hold kept 300 ms is no standing queue.
        feed.receipt(60 * 17, 300, 4096);
        feed.steady(61 * 17, 10, 17, 2, 4096);
        let view = feed.view();
        assert_eq!(view.standing, Some(ms(2)));
        assert_eq!(view.busy_bps, None);
        assert!(
            view.used_bps
                .is_some_and(|used| (1_900_000..2_000_000).contains(&used))
        );
        assert!(!view.would_cut);

        // A second without receipts leaves nothing to report ...
        let later = feed.now + HISTORY + ms(1);
        assert_eq!(
            feed.rate.take_view(later, Some(BASE), PACE),
            ReceiptRateView::default()
        );
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
            blind.on_receipt(receipt, t0 + ms(n * 17), None, PACE);
        }
        let view = blind.take_view(t0 + ms(29 * 17), None, PACE);
        assert_eq!(
            (view.standing, view.busy_bps, view.would_cut),
            (None, None, false)
        );
        assert!(view.used_bps.is_some());
    }

    #[test]
    fn a_stall_burst_never_cuts() {
        // A 2 Mbit/s link near capacity: 4250 bytes every 17 ms, 20 ms each.
        let mut feed = Feed::new();
        feed.steady(0, 60, 17, 20, 4250);
        assert_eq!(
            feed.view(),
            ReceiptRateView {
                standing: Some(ms(20)),
                busy_bps: Some(2_000_000),
                used_bps: Some(2_000_000),
                would_cut: false,
            }
        );
        // The link holds everything for 600 ms, then delivers the 36 units
        // sent meanwhile 3 ms apart: their delay falls 14 ms a receipt, and
        // past 50 ms into the burst its own receipts alone stand above 100 ms.
        let held = 59 * 17 + 600;
        let mut late_in_burst = None;
        for n in 0..36 {
            feed.receipt(held + 3 * n, 600 - 14 * n, 4250);
            let view = feed.view();
            assert!(!view.would_cut, "burst receipt {n}: {view:?}");
            if n == 30 {
                late_in_burst = Some(view);
            }
        }
        // The burst alone would have cut: only the stall rule held it.
        let late = late_in_burst.unwrap();
        assert!(
            late.standing.is_some_and(|standing| standing > ms(100)),
            "{late:?}"
        );
        assert!(late.busy_bps.is_some(), "{late:?}");
        // The link carries on as before.
        let after = held + 3 * 35;
        for n in 1..=30 {
            feed.receipt(after + 17 * n, 20, 4250);
            assert!(!feed.view().would_cut, "receipt {n} after the burst");
        }
    }

    #[test]
    fn compressed_receipts_are_no_rate_sample() {
        // A 2 Mbit/s link whose receipts the return path bunches in threes:
        // 3750 bytes each, 1 ms apart, a group every 45 ms, 80 ms of queue.
        let mut feed = Feed::new();
        for group in 0..30 {
            for n in 0..3 {
                feed.receipt(group * 45 + n, 80, 3750);
            }
        }
        let view = feed.view();
        // Each pair 1 ms apart would claim 30 Mbit/s; each group is three
        // units delivered over the 45 ms since the one before.
        assert_eq!(view.busy_bps, Some(2_000_000));
        // Bunching on a steady link is no stall: the queue still cuts.
        assert_eq!(view.standing, Some(ms(80)));
        assert!(view.would_cut);
    }
}
