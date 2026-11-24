//! One-way queues from the viewer's arrival times (`arrival_time`). The
//! viewer stamps each receipt with when it finished reading that unit, and
//! each probe echo with when it read the probe, on its own session clock; the
//! host pairs the stamp with when it booked the unit or wrote the probe, and
//! with when the receipt came back, on its own clock. The clocks share no
//! epoch, so every one-way delay carries one unknown constant offset: only
//! its rise above the least seen over `RTT_WINDOW` means anything, and that
//! rise is a queue.
//!
//! - Forward, toward the viewer: from the host's send to the viewer having
//!   read the unit, or the probe. Receipts and probes share its minimum, as
//!   they share the offset. A unit's own transfer is in it, as it is in
//!   every round trip the host times, so the thresholds those were tuned on
//!   hold; nothing on the way back is.
//! - Reverse, back to the host: from the viewer having read a unit to the
//!   receipt for it at the host: the viewer's writer and its uplink alone.
//!   Only receipts feed it; the credit window reads the newest few.
//!
//! A stamp is an observation, never authority. One that goes backwards for
//! its kind is ignored, and so is each direction whose delay lies more than
//! `IO_TIMEOUT` above its minimum: the two are judged apart, so a queue past
//! that on the viewer's uplink still leaves the way toward it timed. A stamp
//! that puts one direction more than `IO_TIMEOUT` above its minimum and the
//! other more than that below its own is off, not queued, and ignored whole,
//! so it never drags a minimum down. Instants are monotonic on both ends, so
//! no clock ever jumps. Both minima reserve their bounded tables once: a
//! sample allocates nothing.
use crate::{qos::RTT_WINDOW, wire::IO_TIMEOUT};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// A minimum keeps at most one sample per this span.
const BUCKET: Duration = Duration::from_millis(250);
/// The most a minimum keeps: one sample a bucket over `RTT_WINDOW`, with room.
const ENTRIES: usize = 256;
/// The reverse queues the window reads: the least of the newest this many.
pub(crate) const RECENT_REVERSE: usize = 3;
/// The most the reverse queue adds to the window's time.
pub(crate) const REVERSE_TERM_MAX: Duration = Duration::from_secs(1);

const _: () = assert!(
    RTT_WINDOW.as_nanos() / BUCKET.as_nanos() < ENTRIES as u128,
    "one sample a bucket over the window fits the table reserved for it"
);
const _: () = assert!(
    RECENT_REVERSE >= 1,
    "the window's term is the least of at least one queue"
);

/// The least value pushed over the last `RTT_WINDOW`: a deque of
/// `(since, value)`, oldest first and values rising, at least `BUCKET` apart.
pub(crate) struct WindowMin {
    entries: VecDeque<(Instant, i64)>,
}
impl WindowMin {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(ENTRIES),
        }
    }

    /// The least value of the window as of the last push.
    pub fn min(&self) -> Option<i64> {
        self.entries.front().map(|(_, value)| *value)
    }

    /// `value`, seen at `now`; `now` never goes backwards.
    pub fn push(&mut self, value: i64, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|(since, _)| now.saturating_duration_since(*since) > RTT_WINDOW)
        {
            self.entries.pop_front();
        }
        // A value at or above this one can never be the least again.
        while self.entries.back().is_some_and(|(_, back)| *back >= value) {
            self.entries.pop_back();
        }
        // The newest bucket already holds a lower value.
        if self
            .entries
            .back()
            .is_some_and(|(since, _)| now.saturating_duration_since(*since) < BUCKET)
        {
            return;
        }
        if self.entries.len() >= ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back((now, value));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// One receipt's queues: each `None` when the stamp is no sample of that
/// direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Queues {
    pub forward: Option<Duration>,
    pub reverse: Option<Duration>,
}

/// One session's one-way minima and its newest reverse queues.
pub(crate) struct OneWay {
    /// Host times are microseconds since this.
    epoch: Instant,
    forward: WindowMin,
    reverse: WindowMin,
    /// The newest reverse queues, a ring of which `filled` hold one.
    recent: [Duration; RECENT_REVERSE],
    filled: usize,
    next: usize,
    /// The last stamp taken of each kind.
    receipt_at: Option<u64>,
    probe_at: Option<u64>,
}
impl OneWay {
    pub fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            forward: WindowMin::new(),
            reverse: WindowMin::new(),
            recent: [Duration::ZERO; RECENT_REVERSE],
            filled: 0,
            next: 0,
            receipt_at: None,
            probe_at: None,
        }
    }

    /// A receipt the viewer stamped `at_us`, for a unit booked at `sent`,
    /// that came back at `received`: the queue of each direction the stamp
    /// is a sample of.
    pub fn receipt(&mut self, at_us: u64, sent: Instant, received: Instant) -> Queues {
        if self.receipt_at.is_some_and(|last| at_us < last) {
            return Queues::default();
        }
        let Some((forward, reverse)) = self.delays(at_us, sent, received) else {
            return Queues::default();
        };
        let (forward_ok, reverse_ok) = (
            within(&self.forward, forward),
            within(&self.reverse, reverse),
        );
        if !forward_ok && !reverse_ok {
            return Queues::default();
        }
        self.receipt_at = Some(at_us);
        let queues = Queues {
            forward: forward_ok.then(|| queue(&mut self.forward, forward, received)),
            reverse: reverse_ok.then(|| queue(&mut self.reverse, reverse, received)),
        };
        if let Some(reverse) = queues.reverse {
            if let Some(slot) = self.recent.get_mut(self.next) {
                *slot = reverse;
            }
            self.next = (self.next + 1) % RECENT_REVERSE;
            self.filled = (self.filled + 1).min(RECENT_REVERSE);
        }
        queues
    }

    /// A probe the viewer stamped `at_us`, written at `sent`, echoed at
    /// `received`: its forward queue. `None` when the stamp is no sample of
    /// it. Its way back only checks the stamp against the receipts'.
    pub fn probe(&mut self, at_us: u64, sent: Instant, received: Instant) -> Option<Duration> {
        if self.probe_at.is_some_and(|last| at_us < last) {
            return None;
        }
        let (forward, _) = self.delays(at_us, sent, received)?;
        if !within(&self.forward, forward) {
            return None;
        }
        self.probe_at = Some(at_us);
        Some(queue(&mut self.forward, forward, received))
    }

    /// What the receipts' way back adds to the window's time: the least of
    /// the newest `RECENT_REVERSE` reverse queues, at most
    /// `REVERSE_TERM_MAX`, and nothing before there are that many. One
    /// delayed receipt never widens it.
    pub fn reverse_term(&self) -> Duration {
        if self.filled < RECENT_REVERSE {
            return Duration::ZERO;
        }
        self.recent
            .iter()
            .min()
            .map_or(Duration::ZERO, |least| (*least).min(REVERSE_TERM_MAX))
    }

    /// The median of the newest reverse queues, lower of two, for the stats.
    pub fn reverse_queue(&self) -> Option<Duration> {
        let mut recent = self.recent;
        let recent = recent.get_mut(..self.filled)?;
        recent.sort_unstable();
        recent.get(self.filled.checked_sub(1)? / 2).copied()
    }

    /// Forget the minima and the newest queues: a new path, or a resume.
    pub fn reset(&mut self) {
        self.forward.clear();
        self.reverse.clear();
        self.filled = 0;
        self.next = 0;
        self.receipt_at = None;
        self.probe_at = None;
    }

    /// The one-way delays either way a stamp shows, each carrying its
    /// clock's offset; `None` for a stamp that is off rather than queued:
    /// past `i64`, or putting one direction more than `IO_TIMEOUT` above its
    /// least and the other more than that below its own, as a stamp shifted
    /// that far would.
    fn delays(&self, at_us: u64, sent: Instant, received: Instant) -> Option<(i64, i64)> {
        let at = i64::try_from(at_us).ok()?;
        let forward = at.saturating_sub(self.host_us(sent));
        let reverse = self.host_us(received).saturating_sub(at);
        let (up, back) = (rise(&self.forward, forward), rise(&self.reverse, reverse));
        let limit = limit_us();
        let shifted = |above: i64, below: i64| above > limit && below < limit.saturating_neg();
        (!shifted(up, back) && !shifted(back, up)).then_some((forward, reverse))
    }

    fn host_us(&self, at: Instant) -> i64 {
        i64::try_from(at.saturating_duration_since(self.epoch).as_micros()).unwrap_or(i64::MAX)
    }
}

/// `IO_TIMEOUT` in microseconds: the most a delay may lie above its least.
fn limit_us() -> i64 {
    i64::try_from(IO_TIMEOUT.as_micros()).unwrap_or(i64::MAX)
}

/// How far `delay` lies above the least seen, negative below it; zero
/// before any.
fn rise(window: &WindowMin, delay: i64) -> i64 {
    window.min().map_or(0, |least| delay.saturating_sub(least))
}

/// Whether `delay` lies within `IO_TIMEOUT` of the least seen.
fn within(window: &WindowMin, delay: i64) -> bool {
    rise(window, delay) <= limit_us()
}

/// Push `delay` and return how far it lies above the window's least.
fn queue(window: &mut WindowMin, delay: i64, now: Instant) -> Duration {
    window.push(delay, now);
    let above = window
        .min()
        .map_or(0, |least| delay.saturating_sub(least).max(0));
    Duration::from_micros(u64::try_from(above).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_window_keeps_its_least_bounded_and_forgets_it_after_the_window() {
        let t0 = Instant::now();
        let mut window = WindowMin::new();
        assert_eq!(window.min(), None);
        window.push(50, t0);
        window.push(40, t0 + ms(10));
        window.push(70, t0 + ms(20));
        assert_eq!(window.min(), Some(40));
        // A sample a second for two windows: the table stays bounded, and
        // the least is the window's, not the session's.
        for n in 0..2 * RTT_WINDOW.as_secs() {
            window.push(1_000 + n as i64, t0 + ms(1_000 * (n + 1)));
        }
        assert!(window.len() <= ENTRIES);
        // The oldest sample within the window of the newest, at 120 s.
        assert_eq!(window.min(), Some(1_000 + RTT_WINDOW.as_secs() as i64 - 1));
        // Rising samples, each in its own bucket, every 250 ms for longer
        // than the window: at most one a bucket is kept.
        let mut window = WindowMin::new();
        for n in 0..2_000 {
            window.push(n, t0 + BUCKET * n as u32);
        }
        assert!(window.len() <= ENTRIES, "{}", window.len());
        // Many samples within one bucket keep one entry.
        let mut window = WindowMin::new();
        for n in 0..100 {
            window.push(10 + n % 3, t0 + Duration::from_micros(n as u64));
        }
        assert_eq!((window.len(), window.min()), (1, Some(10)));
    }

    /// Both directions' queues of a receipt.
    fn both(forward: u64, reverse: u64) -> Queues {
        Queues {
            forward: Some(ms(forward)),
            reverse: Some(ms(reverse)),
        }
    }

    /// The viewer's clock runs from its own session start: any offset of
    /// either sign, however large, leaves the queues alone.
    #[test]
    fn queues_hold_for_any_clock_offset() {
        let t0 = Instant::now();
        let at = |n: u64| t0 + ms(n);
        for offset in [0i64, 5_000_000_000, -5_000_000_000, -1_000] {
            let mut one_way = OneWay::new(t0);
            // A unit booked at `sent` ms, read `forward` ms later, receipted
            // `reverse` ms after that, all on the host's clock.
            let mut receipt = |sent: u64, forward: u64, reverse: u64| {
                let read_us = (sent + forward) as i64 * 1_000 + 10_000_000_000 + offset;
                one_way.receipt(read_us as u64, at(sent), at(sent + forward + reverse))
            };
            assert_eq!(receipt(0, 20, 20), both(0, 0));
            assert_eq!(receipt(100, 70, 20), both(50, 0));
            assert_eq!(receipt(200, 20, 320), both(0, 300));
            // A probe shares the forward minimum.
            let read_us = (330 + 45) * 1_000 + 10_000_000_000 + offset;
            assert_eq!(
                one_way.probe(read_us as u64, at(330), at(400)),
                Some(ms(25))
            );
        }
    }

    #[test]
    fn a_backwards_shifted_or_far_stamp_is_no_sample_and_reset_forgets() {
        let t0 = Instant::now();
        let at = |n: u64| t0 + ms(n);
        let us = |n: u64| n * 1_000;
        let none = Queues::default();
        let past = IO_TIMEOUT.as_millis() as u64 + 500;
        let mut one_way = OneWay::new(t0);
        assert_eq!(one_way.receipt(us(20), at(0), at(40)), both(0, 0));
        // Backwards for receipts; a probe read earlier, echoed later, is not.
        assert_eq!(one_way.receipt(us(19), at(1), at(41)), none);
        assert!(one_way.probe(us(10), at(0), at(60)).is_some());
        assert_eq!(one_way.probe(us(9), at(0), at(60)), None);
        // A stamp shifted more than `IO_TIMEOUT` puts one direction that far
        // above its least and the other that far below its own: it is off,
        // not queued, and drags no least down.
        let shifted = us(120 + past);
        assert_eq!(one_way.receipt(shifted, at(100), at(140)), none);
        assert_eq!(one_way.probe(shifted, at(100), at(140)), None);
        // A stamp past i64 is none either.
        assert_eq!(one_way.receipt(u64::MAX, at(2_000), at(2_040)), none);
        // What a refused stamp would have moved stayed put: the probe's
        // 10 ms is still the least forward, the first receipt's the least
        // back.
        assert_eq!(
            one_way.receipt(us(2_020), at(2_000), at(2_040)),
            both(10, 0)
        );

        // A queue past `IO_TIMEOUT` one way leaves the other way timed. The
        // uplink holds a receipt that long, of a unit that waited 160 ms
        // toward the viewer ...
        let held = one_way.receipt(us(2_270), at(2_100), at(2_290 + past));
        let forward_only = Queues {
            forward: Some(ms(160)),
            reverse: None,
        };
        assert_eq!(held, forward_only);
        assert_eq!(one_way.reverse_queue(), Some(Duration::ZERO));
        // ... a unit waits that long toward the viewer, and its receipt comes
        // straight back ...
        let queued = one_way.receipt(us(2_320 + past), at(2_300), at(2_340 + past));
        let reverse_only = Queues {
            forward: None,
            reverse: Some(Duration::ZERO),
        };
        assert_eq!(queued, reverse_only);
        // ... and one that waited that long both ways is no sample at all.
        let both_ways = one_way.receipt(us(2_420 + past), at(2_400), at(2_440 + 2 * past));
        assert_eq!(both_ways, none);
        // A probe is judged on its way toward the viewer alone: one that
        // waited too long there is none, and one whose echo the uplink held
        // that long still times that way.
        let late = us(4_000 + past);
        assert_eq!(one_way.probe(late, at(3_980), at(4_020 + past)), None);
        assert_eq!(
            one_way.probe(us(6_020), at(6_000), at(6_000 + past)),
            Some(ms(10))
        );

        // After a reset, the next sample is the new least, whatever it is.
        one_way.reset();
        assert_eq!(one_way.reverse_queue(), None);
        assert_eq!(one_way.receipt(us(10), at(9_000), at(9_500)), both(0, 0));
    }

    #[test]
    fn the_window_term_is_the_least_of_the_newest_three() {
        let t0 = Instant::now();
        let at = |n: u64| t0 + ms(n);
        let mut one_way = OneWay::new(t0);
        // A clean receipt sets both minima.
        one_way.receipt(20_000, at(0), at(40));
        assert_eq!(one_way.reverse_term(), Duration::ZERO);
        // Two receipts 300 ms late: three samples, but the clean one is least.
        for n in 1..=2 {
            let sent = 100 * n;
            one_way.receipt((sent + 20) * 1_000, at(sent), at(sent + 340));
        }
        assert_eq!(one_way.reverse_term(), Duration::ZERO);
        assert_eq!(one_way.reverse_queue(), Some(ms(300)));
        // A third: the uplink's queue stands, and the window takes it in.
        one_way.receipt(320_000, at(300), at(640));
        assert_eq!(one_way.reverse_term(), ms(300));
        // One fast receipt among them brings it down at once ...
        one_way.receipt(420_000, at(400), at(440));
        assert_eq!(one_way.reverse_term(), Duration::ZERO);
        // ... and the term never passes its bound.
        let mut stalled = OneWay::new(t0);
        stalled.receipt(20_000, at(0), at(40));
        for n in 1..=3 {
            let sent = 100 * n;
            stalled.receipt((sent + 20) * 1_000, at(sent), at(sent + 2_540));
        }
        assert_eq!(stalled.reverse_term(), REVERSE_TERM_MAX);
        assert_eq!(stalled.reverse_queue(), Some(ms(2_500)));
    }
}
