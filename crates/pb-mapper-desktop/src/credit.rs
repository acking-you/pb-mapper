//! Send credit for one host session, returned by the viewer's transport
//! receipts. A receipt says a video message was fully read off the network,
//! including one the viewer's sink later drops, so credit never waits for a
//! decoder or a display. Receipts are consumed here and by the QoS controller
//! only: they grant, renew or prove nothing about input, and no authorisation
//! method takes a seq.
//!
//! A viewer that stamps its receipts with arrival times (`arrival_time`)
//! gives each one its unit's queue toward it and the receipt's queue back
//! (`oneway`): the ack queue is then the queue toward the viewer alone, and
//! the window's time grows by the receipts' standing queue on the way back,
//! so a busy uplink at the viewer neither reads as congestion nor starves
//! admission.
use crate::{VideoCodec, oneway::OneWay};
use std::{
    collections::VecDeque,
    fmt,
    time::{Duration, Instant},
};

/// The RTT assumed until the first probe echo.
pub(crate) const INITIAL_RTT: Duration = Duration::from_millis(100);
/// Added to the RTT so a window covers one round trip plus jitter.
pub(crate) const WINDOW_MARGIN: Duration = Duration::from_millis(100);
pub(crate) const FRAME_WINDOW_MIN: usize = 3;
pub(crate) const FRAME_WINDOW_MAX: usize = 64;
pub(crate) const BYTE_WINDOW_MIN: usize = 64 * 1024;
pub(crate) const BYTE_WINDOW_MAX: usize = 4 * 1024 * 1024;
/// The least byte window the delivery-rate clamp can set.
pub(crate) const BYTE_WINDOW_CLAMP_MIN: usize = 32 * 1024;
/// Receipts this much slower than the baseline mean a standing queue.
pub(crate) const ACK_QUEUE_LIMIT: Duration = Duration::from_millis(100);
/// Receipt delays considered for the ack queue.
pub(crate) const ACK_SAMPLE_WINDOW: Duration = Duration::from_millis(250);
/// The ack queue is judged on at least this many receipts.
const ACK_SAMPLES_MIN: usize = 3;
/// Delivered bytes considered for the delivery rate.
pub(crate) const DELIVERY_WINDOW: Duration = Duration::from_millis(500);
pub(crate) const DELIVERY_FACTOR: f64 = 1.25;
/// A JPEG session's fixed window: pictures are independent and few.
pub(crate) const JPEG_WINDOW_FRAMES: usize = 4;
pub(crate) const JPEG_WINDOW_BYTES: usize = 2 * 1024 * 1024;
/// How long B-send drops predicted units for a viewer that asked for an IDR
/// before it asks C for one again, if none was booked meanwhile.
pub(crate) const SKIP_ASK_AGAIN: Duration = Duration::from_secs(2);

const _: () = assert!(
    FRAME_WINDOW_MIN <= FRAME_WINDOW_MAX,
    "window clamps the frame count between them"
);
const _: () = assert!(
    BYTE_WINDOW_MIN <= BYTE_WINDOW_MAX,
    "window clamps the normal byte window between them"
);
const _: () = assert!(
    BYTE_WINDOW_CLAMP_MIN <= BYTE_WINDOW_MIN,
    "the delivery clamp's upper bound is the normal window, which is at least BYTE_WINDOW_MIN"
);
const _: () = assert!(
    ACK_SAMPLES_MIN >= 1,
    "ack_queue takes the median of at least this many delays"
);

/// A receipt for a seq beyond the last one admitted: the viewer receipted
/// video that was never sent, which breaks the protocol.
#[derive(Debug)]
pub(crate) struct ReceiptAhead;
impl fmt::Display for ReceiptAhead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("receipt for video that was never sent")
    }
}
impl std::error::Error for ReceiptAhead {}

/// A keyframe request after a seq beyond the last one admitted: the viewer
/// claims to have read video that was never sent, which breaks the protocol.
#[derive(Debug)]
pub(crate) struct KeyframeAhead;
impl fmt::Display for KeyframeAhead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("keyframe request after video that was never sent")
    }
}
impl std::error::Error for KeyframeAhead {}

/// A video message admitted and written, not yet receipted.
struct Sent {
    seq: u64,
    bytes: u32,
    sent_at: Instant,
    keyframe: bool,
    /// An IDR, or written while one was in flight: its receipt's delay
    /// includes that IDR's transfer.
    behind_idr: bool,
}

/// What one receipt says about the link, for the QoS controller's receipt-rate
/// estimator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Receipt {
    /// From writing the newest unit it covered to the receipt.
    pub delay: Duration,
    /// The payload bytes it newly covered.
    pub bytes: u64,
    /// The newest unit it covered was behind an IDR.
    pub behind_idr: bool,
    /// The newest unit's arrival, from a viewer that stamps them and a stamp
    /// taken as a sample of the way toward it.
    pub arrival: Option<Arrival>,
}

/// When the viewer read the newest unit a receipt covered, and the queue
/// toward it that time shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Arrival {
    /// On the viewer's clock: since its session began.
    pub at: Duration,
    /// Toward the viewer: the unit's wait above the least seen.
    pub forward_queue: Duration,
}

/// What B-send does with a predicted unit at the head of its queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Skip {
    Send,
    /// Drop it: the viewer waits for an IDR and would throw it away.
    Drop,
    /// Drop it and ask C for the IDR again: none was booked for
    /// `SKIP_ASK_AGAIN`.
    DropAndAsk,
}

/// How much may be in flight before the next admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    pub frames: usize,
    pub bytes: usize,
    /// The delivery rate clamped `bytes` below the normal window.
    pub clamped: bool,
}

/// The ledger's state for the QoS tick, the stats and the trace.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CreditView {
    pub outstanding_frames: usize,
    pub outstanding_bytes: usize,
    pub idr_inflight: Option<u64>,
    /// The later of the last receipt and the oldest unreceipted send: how long
    /// the viewer has owed a receipt.
    pub owed_since: Option<Instant>,
}

pub(crate) struct CreditLedger {
    last_admitted: u64,
    acked: u64,
    in_flight: VecDeque<Sent>,
    in_flight_bytes: usize,
    /// The oldest IDR in flight; probes written behind it are tainted.
    idr_inflight: Option<u64>,
    /// When the last receipt that covered an IDR came in.
    idr_received: Option<Instant>,
    /// `(receipt time, ack delay)` of the newest unit each receipt covered:
    /// with arrival times, its queue toward the viewer.
    ack_samples: VecDeque<(Instant, Duration)>,
    /// `(receipt time, bytes)` of every receipted unit.
    delivered: VecDeque<(Instant, u32)>,
    /// When the delivery rate began to be measured.
    delivery_since: Option<Instant>,
    last_receipt: Instant,
    /// Reused by `ack_queue` for the delays it takes the median of.
    delays: Vec<Duration>,
    /// The one-way queues of a viewer that stamps arrival times; `None` for
    /// one that does not.
    one_way: Option<OneWay>,
    /// What the RTT floor hides of a queue on this path (`Qos::floor_gap`),
    /// which the one-way ack queue hides too.
    floor_gap: Duration,
    /// What a receipt rebase added to the QoS controller's baseline
    /// (`Qos::rebase_state`): a longer route, which the window's time
    /// covers and the ack queue does not count; and its generation.
    rebase: Duration,
    rebase_generation: u64,
    /// The newest IDR booked.
    last_keyframe: Option<u64>,
    /// While set, B-send drops predicted units until an IDR is booked, and
    /// asks C for one again at this instant.
    skip_ask_at: Option<Instant>,
}
impl CreditLedger {
    /// `arrival_time`: the viewer stamps its receipts and probe echoes.
    pub fn new(now: Instant, arrival_time: bool) -> Self {
        Self {
            last_admitted: 0,
            acked: 0,
            in_flight: VecDeque::new(),
            in_flight_bytes: 0,
            idr_inflight: None,
            idr_received: None,
            ack_samples: VecDeque::new(),
            delivered: VecDeque::new(),
            delivery_since: None,
            last_receipt: now,
            delays: Vec::new(),
            one_way: arrival_time.then(|| OneWay::new(now)),
            floor_gap: Duration::ZERO,
            rebase: Duration::ZERO,
            rebase_generation: 0,
            last_keyframe: None,
            skip_ask_at: None,
        }
    }

    /// The window for the next admission. It is sized from the baseline RTT,
    /// never a smoothed one, so queueing never enlarges it, plus, with arrival
    /// times, the receipts' standing queue on their way back
    /// (`OneWay::reverse_term`), during which a unit stays unreceipted but in
    /// no queue toward the viewer. While a receipt episode lasts, `clamp` is
    /// the busy rate the QoS controller cut for, and the byte budget follows
    /// it at once. Otherwise, when receipts show a standing queue over a full
    /// delivery window, the byte budget follows the measured delivery rate:
    /// that bounds the queue whoever cuts the rate.
    pub fn window(
        &mut self,
        codec: VideoCodec,
        pace_fps: u32,
        expected_bps: u64,
        base_rtt: Option<Duration>,
        clamp: Option<u64>,
        now: Instant,
    ) -> Window {
        if codec == VideoCodec::Jpeg {
            return Window {
                frames: JPEG_WINDOW_FRAMES,
                bytes: JPEG_WINDOW_BYTES,
                clamped: false,
            };
        }
        let reverse = self
            .one_way
            .as_ref()
            .map_or(Duration::ZERO, OneWay::reverse_term);
        let t = base_rtt.unwrap_or(INITIAL_RTT) + self.rebase + WINDOW_MARGIN + reverse;
        let frames = (f64::from(pace_fps) * t.as_secs_f64()).ceil() as usize;
        let normal = (2.0 * expected_bps as f64 * t.as_secs_f64() / 8.0) as usize;
        let normal = normal.clamp(BYTE_WINDOW_MIN, BYTE_WINDOW_MAX);
        let delivering = self
            .delivery_since
            .is_some_and(|since| now.saturating_duration_since(since) >= DELIVERY_WINDOW);
        let follow = |rate: u64| {
            let bytes = (DELIVERY_FACTOR * rate as f64 * t.as_secs_f64() / 8.0) as usize;
            (bytes.clamp(BYTE_WINDOW_CLAMP_MIN, normal), true)
        };
        let (bytes, clamped) = match (clamp, base_rtt) {
            (Some(rate), Some(_)) => follow(rate),
            (_, Some(base)) if delivering && self.ack_queue(base, now) > ACK_QUEUE_LIMIT => {
                follow(self.delivery_bps(now))
            }
            _ => (normal, false),
        };
        Window {
            frames: frames.clamp(FRAME_WINDOW_MIN, FRAME_WINDOW_MAX),
            bytes,
            clamped,
        }
    }

    /// Whether a unit of `len` bytes may be written now. An empty flight
    /// admits any size, so the session always makes progress; one IDR may go
    /// over the byte window while P-frames are in flight, and a second waits.
    pub fn fits(&self, len: usize, keyframe: bool, window: &Window) -> bool {
        self.in_flight.is_empty()
            || (keyframe
                && self.idr_inflight.is_none()
                && self.in_flight.len() < window.frames + 1
                && self.in_flight_bytes <= window.bytes)
            || (self.in_flight.len() < window.frames && self.in_flight_bytes + len <= window.bytes)
    }

    /// Book a unit `fits` admitted and assign its seq. Booking precedes the
    /// write, so a receipt can never precede its booking and a receipt ahead
    /// of the last admitted seq is a protocol error.
    pub fn book(&mut self, len: usize, keyframe: bool, now: Instant) -> u64 {
        self.last_admitted += 1;
        let seq = self.last_admitted;
        self.in_flight.push_back(Sent {
            seq,
            bytes: len as u32,
            sent_at: now,
            keyframe,
            behind_idr: keyframe || self.idr_inflight.is_some(),
        });
        self.in_flight_bytes += len;
        if keyframe {
            if self.idr_inflight.is_none() {
                self.idr_inflight = Some(seq);
            }
            self.last_keyframe = Some(seq);
            self.skip_ask_at = None;
        }
        seq
    }

    /// The viewer asked for an IDR having read up to `after_seq`, and throws
    /// predicted frames away until the next one. Unless an IDR was booked
    /// after that seq, which is on its way to the viewer, B-send drops
    /// predicted units from now until one is booked: never fewer, since
    /// every one after the first it drops refers to one it dropped. Returns
    /// whether it does. A seq beyond the last admitted one is a protocol
    /// error.
    pub fn skip_to_keyframe(
        &mut self,
        after_seq: u64,
        now: Instant,
    ) -> Result<bool, KeyframeAhead> {
        if after_seq > self.last_admitted {
            return Err(KeyframeAhead);
        }
        if self
            .last_keyframe
            .is_some_and(|keyframe| keyframe > after_seq)
        {
            return Ok(false);
        }
        self.skip_ask_at = Some(now + SKIP_ASK_AGAIN);
        Ok(true)
    }

    /// What B-send does with a predicted unit at the head now. A skip ends
    /// only when an IDR is booked; one that waited `SKIP_ASK_AGAIN` for it
    /// asks C again, and again as long after.
    pub fn skipping(&mut self, now: Instant) -> Skip {
        let Some(ask_at) = &mut self.skip_ask_at else {
            return Skip::Send;
        };
        if now < *ask_at {
            return Skip::Drop;
        }
        *ask_at = now + SKIP_ASK_AGAIN;
        Skip::DropAndAsk
    }

    /// A cumulative receipt: every unit up to `seq` was read off the network,
    /// the newest of them when the viewer's clock said `at_us`, if it said.
    /// Returns what it says about the link; nothing for a duplicate.
    pub fn on_receipt(
        &mut self,
        seq: u64,
        now: Instant,
        at_us: Option<u64>,
    ) -> Result<Option<Receipt>, ReceiptAhead> {
        if seq > self.last_admitted {
            return Err(ReceiptAhead);
        }
        if seq <= self.acked {
            return Ok(None);
        }
        let mut newest = None;
        let mut bytes = 0;
        while let Some(sent) = self.in_flight.front()
            && sent.seq <= seq
        {
            self.in_flight_bytes -= sent.bytes as usize;
            self.delivered.push_back((now, sent.bytes));
            bytes += u64::from(sent.bytes);
            newest = Some((sent.sent_at, sent.behind_idr));
            if sent.keyframe {
                self.idr_received = Some(now);
            }
            self.in_flight.pop_front();
        }
        let receipt = newest.map(|(sent_at, behind_idr)| {
            // The way back is kept in the one-way state for the window; the
            // receipt carries the way toward the viewer.
            let arrival = self
                .one_way
                .as_mut()
                .zip(at_us)
                .and_then(|(one_way, at_us)| {
                    let forward_queue = one_way.receipt(at_us, sent_at, now).forward?;
                    Some(Arrival {
                        at: Duration::from_micros(at_us),
                        forward_queue,
                    })
                });
            Receipt {
                delay: now.saturating_duration_since(sent_at),
                bytes,
                behind_idr,
                arrival,
            }
        });
        // With arrival times, the queue toward the viewer, and a stamp that
        // is no sample adds nothing.
        let sample = match (&self.one_way, receipt) {
            (Some(_), Some(receipt)) => receipt.arrival.map(|arrival| arrival.forward_queue),
            (None, Some(receipt)) => Some(receipt.delay),
            (_, None) => None,
        };
        if let Some(sample) = sample {
            self.ack_samples.push_back((now, sample));
        }
        self.idr_inflight = self
            .in_flight
            .iter()
            .find(|sent| sent.keyframe)
            .map(|sent| sent.seq);
        self.delivery_since.get_or_insert(now);
        self.acked = seq;
        self.last_receipt = now;
        while self.ack_samples.len() > ACK_SAMPLES_MIN
            && self
                .ack_samples
                .front()
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) > ACK_SAMPLE_WINDOW)
        {
            self.ack_samples.pop_front();
        }
        while self
            .delivered
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > DELIVERY_WINDOW)
        {
            self.delivered.pop_front();
        }
        Ok(receipt)
    }

    pub fn idr_inflight(&self) -> Option<u64> {
        self.idr_inflight
    }

    /// When a unit ready at `ready_at` began to age while it waits to be
    /// written: when it was ready, or when the last IDR was receipted if that
    /// came later. `None` while an IDR is in flight: waiting behind it is not
    /// staleness, since that IDR is already the resync.
    pub fn aging_since(&self, ready_at: Instant) -> Option<Instant> {
        if self.idr_inflight.is_some() {
            return None;
        }
        Some(self.idr_received.map_or(ready_at, |at| at.max(ready_at)))
    }

    pub fn view(&self) -> CreditView {
        CreditView {
            outstanding_frames: self.in_flight.len(),
            outstanding_bytes: self.in_flight_bytes,
            idr_inflight: self.idr_inflight,
            owed_since: self
                .in_flight
                .front()
                .map(|oldest| oldest.sent_at.max(self.last_receipt)),
        }
    }

    /// The median receipt delay above the baseline RTT, over the last
    /// `ACK_SAMPLE_WINDOW`, or over the newest three receipts when fewer came
    /// in it: a slow link receipts seldom, and its queue must not vanish
    /// between two receipts. Zero before three receipts. With arrival times,
    /// the median queue toward the viewer, which no baseline offsets, less
    /// what the RTT floor hides of a round trip on this path. Either way less
    /// what a rebase added to the baseline.
    pub fn ack_queue(&mut self, base_rtt: Duration, now: Instant) -> Duration {
        let median = self.ack_median(now);
        if self.one_way.is_some() {
            median.saturating_sub(self.floor_gap + self.rebase)
        } else {
            median.saturating_sub(base_rtt + self.rebase)
        }
    }

    /// What the RTT floor hides of a queue on this path, as the QoS
    /// controller last found it.
    pub fn set_floor_gap(&mut self, gap: Duration) {
        self.floor_gap = gap;
    }

    /// What a receipt rebase added to the QoS controller's baseline, as of
    /// its `generation`: a write of an older generation than the ledger has,
    /// which lost a race with a newer one, is ignored.
    pub fn set_rebase(&mut self, generation: u64, rebase: Duration) {
        if generation > self.rebase_generation {
            self.rebase_generation = generation;
            self.rebase = rebase;
        }
    }

    /// The queues of a viewer that stamps arrival times, for the stats: the
    /// ack queue toward it, and the median of the newest receipts' queues on
    /// their way back.
    pub fn one_way_queues(&mut self, now: Instant) -> Option<(Duration, Option<Duration>)> {
        let reverse = self.one_way.as_ref()?.reverse_queue();
        let forward = self
            .ack_median(now)
            .saturating_sub(self.floor_gap + self.rebase);
        Some((forward, reverse))
    }

    /// A probe's queue toward the viewer: written at `sent`, read when the
    /// viewer's clock said `at_us`, echoed at `now`. `None` from a viewer
    /// that stamps nothing, or for a stamp that is no sample.
    pub fn probe_queue(&mut self, at_us: u64, sent: Instant, now: Instant) -> Option<Duration> {
        self.one_way.as_mut()?.probe(at_us, sent, now)
    }

    /// A new path, or a resume: the one-way minima mean nothing now.
    pub fn reset_one_way(&mut self) {
        if let Some(one_way) = &mut self.one_way {
            one_way.reset();
        }
    }

    /// The median of the ack samples `ack_queue` reads.
    fn ack_median(&mut self, now: Instant) -> Duration {
        if self.ack_samples.len() < ACK_SAMPLES_MIN {
            return Duration::ZERO;
        }
        let recent = self
            .ack_samples
            .iter()
            .filter(|(at, _)| now.saturating_duration_since(*at) <= ACK_SAMPLE_WINDOW)
            .count()
            .max(ACK_SAMPLES_MIN);
        self.delays.clear();
        self.delays.extend(
            self.ack_samples
                .iter()
                .rev()
                .take(recent)
                .map(|(_, delay)| *delay),
        );
        if self.delays.is_empty() {
            return Duration::ZERO;
        }
        // The middle of a non-empty list; the value a sort would put there.
        let middle = self.delays.len() / 2;
        let (_, median, _) = self.delays.select_nth_unstable(middle);
        *median
    }

    /// Bits per second receipted over the last `DELIVERY_WINDOW`.
    fn delivery_bps(&self, now: Instant) -> u64 {
        let bytes: u64 = self
            .delivered
            .iter()
            .filter(|(at, _)| now.saturating_duration_since(*at) <= DELIVERY_WINDOW)
            .map(|(_, bytes)| u64::from(*bytes))
            .sum();
        (bytes as f64 * 8.0 / DELIVERY_WINDOW.as_secs_f64()) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: usize = 1024;

    #[test]
    fn receipts_return_credit_and_idr_uses_headroom() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut credit = CreditLedger::new(t0, false);
        // 60 fps at 4 Mbit/s over a 20 ms baseline: 8 frames and 120 KB.
        let base = Some(Duration::from_millis(20));
        let window = credit.window(VideoCodec::H264, 60, 4_000_000, base, None, t0);
        assert_eq!(
            window,
            Window {
                frames: 8,
                bytes: 120_000,
                clamped: false
            }
        );

        // An empty ledger admits any size; the seq is booked before the write.
        assert!(credit.fits(3 * 1024 * KIB, false, &window));
        assert_eq!(credit.book(3 * 1024 * KIB, false, t0), 1);
        assert!(!credit.fits(KIB, false, &window));
        assert!(credit.on_receipt(2, ms(1), None).is_err());
        credit.on_receipt(1, ms(10), None).unwrap();
        // A repeated receipt is a harmless duplicate.
        credit.on_receipt(1, ms(11), None).unwrap();

        // The window closes at its frame count ...
        for frame in 0..8 {
            assert!(credit.fits(KIB, false, &window));
            assert_eq!(credit.book(KIB, false, ms(20)), 2 + frame);
        }
        assert!(!credit.fits(KIB, false, &window));
        // ... and a receipt reopens it.
        credit.on_receipt(3, ms(40), None).unwrap();
        assert!(credit.fits(KIB, false, &window));

        // ... and at its byte count, for P-frames.
        let mut credit = CreditLedger::new(t0, false);
        credit.book(100 * KIB, false, t0);
        assert!(!credit.fits(20 * KIB, false, &window));
        // One IDR may go over the byte window while P-frames are in flight ...
        assert!(credit.fits(200 * KIB, true, &window));
        let idr = credit.book(200 * KIB, true, t0);
        assert_eq!(credit.idr_inflight(), Some(idr));
        // ... and a second one waits for it.
        assert!(!credit.fits(KIB, true, &window));
        credit.on_receipt(idr, ms(30), None).unwrap();
        assert_eq!(credit.idr_inflight(), None);
        assert!(credit.fits(200 * KIB, true, &window));

        // Receipts 150 ms slower than the baseline for a full delivery window
        // clamp the byte window to the delivery rate, and release it after.
        let mut credit = CreditLedger::new(t0, false);
        let mut seq = 0;
        for n in 0..30 {
            seq = credit.book(5 * KIB, false, ms(n * 20));
            credit.on_receipt(seq, ms(n * 20 + 170), None).unwrap();
        }
        let now = ms(29 * 20 + 170);
        let slow = credit.window(VideoCodec::H264, 60, 4_000_000, base, None, now);
        assert!(slow.clamped);
        // 5 KiB every 20 ms is about 2 Mbit/s: ×1.25 over 120 ms is about 39 KB.
        assert!((36_000..=41_000).contains(&slow.bytes), "{slow:?}");
        assert!(slow.bytes < window.bytes);
        for n in 30..50 {
            seq = credit.book(5 * KIB, false, ms(n * 20));
            credit.on_receipt(seq, ms(n * 20 + 25), None).unwrap();
        }
        let fast = credit.window(
            VideoCodec::H264,
            60,
            4_000_000,
            base,
            None,
            ms(49 * 20 + 25),
        );
        assert_eq!(fast, window);
        assert_eq!(seq, 50);
        // A slow link receipts seldom: between two receipts fewer than three
        // lie in the sample window, and the clamp still holds.
        let mut credit = CreditLedger::new(t0, false);
        for n in 0..10 {
            let seq = credit.book(9 * KIB, false, ms(n * 100));
            credit.on_receipt(seq, ms(n * 100 + 400), None).unwrap();
        }
        let between = credit.window(
            VideoCodec::H264,
            60,
            4_000_000,
            base,
            None,
            ms(9 * 100 + 460),
        );
        assert!(between.clamped, "{between:?}");

        // A JPEG session has a fixed window.
        let jpeg = CreditLedger::new(t0, false).window(VideoCodec::Jpeg, 8, 0, None, None, t0);
        assert_eq!((jpeg.frames, jpeg.bytes), (4, 2 * 1024 * KIB));
    }

    #[test]
    fn an_episode_clamps_the_window_at_once_and_a_rebase_is_no_queue() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let base = Some(Duration::from_millis(20));
        let mut credit = CreditLedger::new(t0, false);
        // Before any receipt the delivery clamp has nothing to go on, but a
        // receipt episode's busy rate clamps at once: 1.25 × 2 Mbit/s over
        // 20 ms + 100 ms is 37 500 bytes ...
        let open = credit.window(VideoCodec::H264, 60, 4_000_000, base, None, t0);
        assert!(!open.clamped);
        let clamped = credit.window(VideoCodec::H264, 60, 4_000_000, base, Some(2_000_000), t0);
        assert_eq!((clamped.bytes, clamped.clamped), (37_500, true));
        assert_eq!(clamped.frames, open.frames);
        // ... never below 32 KiB, never above the normal window ...
        let slow = credit.window(VideoCodec::H264, 60, 4_000_000, base, Some(100_000), t0);
        assert_eq!(slow.bytes, BYTE_WINDOW_CLAMP_MIN);
        let fast = credit.window(VideoCodec::H264, 60, 4_000_000, base, Some(1 << 30), t0);
        assert_eq!(fast.bytes, open.bytes);
        // ... and never for JPEG.
        let jpeg = credit.window(VideoCodec::Jpeg, 8, 0, base, Some(100_000), t0);
        assert!(!jpeg.clamped);

        // Receipts 150 ms above a 20 ms base: a 150 ms ack queue, which
        // clamps the window to the delivery rate ...
        let mut seq = 0;
        for n in 0..30 {
            seq = credit.book(5 * KIB, false, ms(n * 20));
            credit.on_receipt(seq, ms(n * 20 + 170), None).unwrap();
        }
        let now = ms(29 * 20 + 170);
        assert_eq!(
            credit.ack_queue(Duration::from_millis(20), now),
            Duration::from_millis(150)
        );
        assert!(
            credit
                .window(VideoCodec::H264, 60, 4_000_000, base, None, now)
                .clamped
        );
        // ... unless 120 ms of it is a longer route the QoS controller
        // rebased onto: 30 ms of queue, and a window whose time covers the
        // route: 60 FPS over 20 + 120 + 100 ms is 15 frames.
        credit.set_rebase(2, Duration::from_millis(120));
        assert_eq!(
            credit.ack_queue(Duration::from_millis(20), now),
            Duration::from_millis(30)
        );
        let rebased = credit.window(VideoCodec::H264, 60, 4_000_000, base, None, now);
        assert_eq!((rebased.frames, rebased.clamped), (15, false));
        // A write of an older generation, which lost a race, changes nothing.
        credit.set_rebase(1, Duration::ZERO);
        assert_eq!(
            credit.ack_queue(Duration::from_millis(20), now),
            Duration::from_millis(30)
        );
        assert_eq!(seq, 30);
    }
    #[test]
    fn a_receipt_reports_its_newest_unit_and_the_bytes_it_covered() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut credit = CreditLedger::new(t0, false);
        credit.book(4 * KIB, false, ms(0));
        credit.book(6 * KIB, false, ms(10));
        // One receipt for both: the newest unit's delay, both units' bytes.
        let receipt = credit.on_receipt(2, ms(35), None).unwrap();
        let receipt = receipt.unwrap();
        assert_eq!(
            receipt,
            Receipt {
                delay: Duration::from_millis(25),
                bytes: 10 * KIB as u64,
                behind_idr: false,
                arrival: None,
            }
        );
        // A duplicate says nothing.
        assert_eq!(credit.on_receipt(2, ms(40), None).unwrap(), None);

        // An IDR and what is written while it is in flight are behind it;
        // what is written after its receipt is not.
        let idr = credit.book(100 * KIB, true, ms(50));
        let behind = credit.book(4 * KIB, false, ms(60));
        assert!(
            credit
                .on_receipt(idr, ms(150), None)
                .unwrap()
                .unwrap()
                .behind_idr
        );
        let clear = credit.book(4 * KIB, false, ms(160));
        assert!(
            credit
                .on_receipt(behind, ms(170), None)
                .unwrap()
                .unwrap()
                .behind_idr
        );
        assert!(
            !credit
                .on_receipt(clear, ms(180), None)
                .unwrap()
                .unwrap()
                .behind_idr
        );
    }

    #[test]
    fn a_waiting_unit_ages_from_the_last_idr_receipt() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut credit = CreditLedger::new(t0, false);
        let idr = credit.book(160 * KIB, true, ms(10));
        credit.book(8 * KIB, false, ms(20));
        // Waiting behind an IDR in flight is not staleness.
        assert_eq!(credit.aging_since(ms(30)), None);
        // Once it is receipted, with the P-frame still in flight, a unit ready
        // before the receipt ages from it, and one ready after from then.
        credit.on_receipt(idr, ms(1400), None).unwrap();
        assert_eq!(credit.aging_since(ms(30)), Some(ms(1400)));
        assert_eq!(credit.aging_since(ms(1500)), Some(ms(1500)));

        // A JPEG session books no IDR: a unit always ages from when it was ready.
        let mut jpeg = CreditLedger::new(t0, false);
        let seq = jpeg.book(300 * KIB, false, ms(10));
        assert_eq!(jpeg.aging_since(ms(30)), Some(ms(30)));
        jpeg.on_receipt(seq, ms(1400), None).unwrap();
        assert_eq!(jpeg.aging_since(ms(30)), Some(ms(30)));
    }

    /// A clean unit, then 30 every 20 ms, each read 10 ms after it was booked
    /// plus `toward`, and receipted 10 ms after that plus `back`; the window
    /// after the last receipt.
    fn one_way_window(arrival_time: bool, toward: u64, back: u64) -> (CreditLedger, Window) {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        // The viewer's clock, an unknown offset from the host's.
        let stamp = |n: u64| Some(7_000_000 + n * 1_000);
        let mut credit = CreditLedger::new(t0, arrival_time);
        let seq = credit.book(5 * KIB, false, ms(0));
        credit.on_receipt(seq, ms(20), stamp(10)).unwrap();
        let mut last = ms(20);
        for n in 1..=30 {
            let (sent, read) = (n * 20, n * 20 + 10 + toward);
            let seq = credit.book(5 * KIB, false, ms(sent));
            last = ms(read + 10 + back);
            let receipt = credit.on_receipt(seq, last, stamp(read)).unwrap().unwrap();
            let forward = receipt.arrival.map(|arrival| arrival.forward_queue);
            assert_eq!(forward, arrival_time.then(|| Duration::from_millis(toward)));
        }
        let base = Some(Duration::from_millis(20));
        let window = credit.window(VideoCodec::H264, 60, 4_000_000, base, None, last);
        (credit, window)
    }

    #[test]
    fn arrival_times_keep_the_uplinks_queue_out_of_the_window() {
        let base = Duration::from_millis(20);
        // 300 ms on the way back only: the window's time grows by it, so the
        // receipts held up there never starve admission, and nothing clamps.
        let (mut credit, window) = one_way_window(true, 0, 300);
        assert_eq!((window.frames, window.clamped), (26, false), "{window:?}");
        assert!(window.bytes > 400_000, "{window:?}");
        let now = Instant::now() + Duration::from_secs(1);
        assert_eq!(credit.ack_queue(base, now), Duration::ZERO);
        assert_eq!(
            credit.one_way_queues(now),
            Some((Duration::ZERO, Some(Duration::from_millis(300))))
        );
        // The same receipts timed by the host alone: a 300 ms ack queue,
        // the window clamped to the delivery rate.
        let (mut credit, window) = one_way_window(false, 0, 300);
        assert_eq!((window.frames, window.clamped), (8, true), "{window:?}");
        assert!(window.bytes < 64 * KIB, "{window:?}");
        assert_eq!(credit.ack_queue(base, now), Duration::from_millis(300));
        assert_eq!(credit.one_way_queues(now), None);
        assert_eq!(credit.probe_queue(1, now, now), None);
        // 150 ms toward the viewer is congestion: the forward queue clamps,
        // and still does behind a queue on the uplink past `IO_TIMEOUT`,
        // whose receipts then time the way toward the viewer alone.
        let (mut credit, window) = one_way_window(true, 150, 3_500);
        assert_eq!((window.frames, window.clamped), (8, true), "{window:?}");
        assert_eq!(credit.ack_queue(base, now), Duration::from_millis(150));
        let (mut credit, window) = one_way_window(true, 150, 0);
        assert_eq!((window.frames, window.clamped), (8, true), "{window:?}");
        assert_eq!(credit.ack_queue(base, now), Duration::from_millis(150));
        // On a path shorter than the RTT floor, as much of it is hidden as a
        // round trip's.
        credit.set_floor_gap(Duration::from_millis(8));
        assert_eq!(credit.ack_queue(base, now), Duration::from_millis(142));
        credit.set_floor_gap(Duration::ZERO);
        // A new path forgets the minima: the next unit is the new least.
        credit.reset_one_way();
        assert_eq!(
            credit.one_way_queues(now),
            Some((Duration::from_millis(150), None))
        );
    }

    #[test]
    fn a_keyframe_request_skips_predicted_units_until_an_idr() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut credit = CreditLedger::new(t0, false);
        let first = credit.book(100 * KIB, true, ms(0));
        for n in 1..=4 {
            credit.book(KIB, false, ms(n * 10));
        }
        // A seq beyond the last admitted breaks the protocol.
        assert!(credit.skip_to_keyframe(6, ms(50)).is_err());
        // An IDR booked after the seq the viewer read is on its way to it.
        assert!(!credit.skip_to_keyframe(first - 1, ms(50)).unwrap());
        assert_eq!(credit.skipping(ms(50)), Skip::Send);
        // None was: predicted units are dropped until the next IDR is booked.
        assert!(credit.skip_to_keyframe(3, ms(60)).unwrap());
        assert_eq!(credit.skipping(ms(70)), Skip::Drop);
        let idr = credit.book(100 * KIB, true, ms(80));
        assert_eq!(credit.skipping(ms(90)), Skip::Send);
        // A request made before that IDR, arriving after it, skips nothing.
        assert!(!credit.skip_to_keyframe(5, ms(100)).unwrap());
        assert_eq!(credit.skipping(ms(100)), Skip::Send);
        // With no IDR booked, the skip never ends by itself: every predicted
        // unit after a dropped one refers to it. B-send asks for the IDR
        // again every `SKIP_ASK_AGAIN`.
        assert!(credit.skip_to_keyframe(idr, ms(200)).unwrap());
        let due = ms(200) + SKIP_ASK_AGAIN;
        let early = due - Duration::from_millis(1);
        assert_eq!(credit.skipping(early), Skip::Drop);
        assert_eq!(credit.skipping(due), Skip::DropAndAsk);
        assert_eq!(credit.skipping(due + Duration::from_millis(1)), Skip::Drop);
        assert_eq!(credit.skipping(due + SKIP_ASK_AGAIN), Skip::DropAndAsk);
        credit.book(100 * KIB, true, due + SKIP_ASK_AGAIN);
        assert_eq!(credit.skipping(due + 2 * SKIP_ASK_AGAIN), Skip::Send);
    }
}
