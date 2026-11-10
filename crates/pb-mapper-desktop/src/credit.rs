//! Send credit for one host session, returned by the viewer's transport
//! receipts. A receipt says a video message was fully read off the network,
//! including one the viewer's sink later drops, so credit never waits for a
//! decoder or a display. Receipts are consumed here and by the QoS controller
//! only: they grant, renew or prove nothing about input, and no authorisation
//! method takes a seq.
use crate::VideoCodec;
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

/// A video message admitted and written, not yet receipted.
struct Sent {
    seq: u64,
    bytes: u32,
    sent_at: Instant,
    keyframe: bool,
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
    /// `(receipt time, ack delay)` of the newest unit each receipt covered.
    ack_samples: VecDeque<(Instant, Duration)>,
    /// `(receipt time, bytes)` of every receipted unit.
    delivered: VecDeque<(Instant, u32)>,
    /// When the delivery rate began to be measured.
    delivery_since: Option<Instant>,
    last_receipt: Instant,
}
impl CreditLedger {
    pub fn new(now: Instant) -> Self {
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
        }
    }

    /// The window for the next admission. It is sized from the baseline RTT,
    /// never a smoothed one, so queueing never enlarges it. When receipts show
    /// a standing queue over a full delivery window, the byte budget follows
    /// the measured delivery rate instead: that bounds the queue between
    /// probes, while fps and bitrate decisions stay on the probe.
    pub fn window(
        &self,
        codec: VideoCodec,
        pace_fps: u32,
        expected_bps: u64,
        base_rtt: Option<Duration>,
        now: Instant,
    ) -> Window {
        if codec == VideoCodec::Jpeg {
            return Window {
                frames: JPEG_WINDOW_FRAMES,
                bytes: JPEG_WINDOW_BYTES,
                clamped: false,
            };
        }
        let t = base_rtt.unwrap_or(INITIAL_RTT) + WINDOW_MARGIN;
        let frames = (f64::from(pace_fps) * t.as_secs_f64()).ceil() as usize;
        let normal = (2.0 * expected_bps as f64 * t.as_secs_f64() / 8.0) as usize;
        let normal = normal.clamp(BYTE_WINDOW_MIN, BYTE_WINDOW_MAX);
        let delivering = self
            .delivery_since
            .is_some_and(|since| now.saturating_duration_since(since) >= DELIVERY_WINDOW);
        let (bytes, clamped) = match base_rtt {
            Some(base) if delivering && self.ack_queue(base, now) > ACK_QUEUE_LIMIT => {
                let rate = self.delivery_bps(now) as f64;
                let bytes = (DELIVERY_FACTOR * rate * t.as_secs_f64() / 8.0) as usize;
                (bytes.clamp(BYTE_WINDOW_CLAMP_MIN, normal), true)
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
        });
        self.in_flight_bytes += len;
        if keyframe && self.idr_inflight.is_none() {
            self.idr_inflight = Some(seq);
        }
        seq
    }

    /// A cumulative receipt: every unit up to `seq` was read off the network.
    pub fn on_receipt(&mut self, seq: u64, now: Instant) -> Result<(), ReceiptAhead> {
        if seq > self.last_admitted {
            return Err(ReceiptAhead);
        }
        if seq <= self.acked {
            return Ok(());
        }
        let mut newest = None;
        while let Some(sent) = self.in_flight.front()
            && sent.seq <= seq
        {
            self.in_flight_bytes -= sent.bytes as usize;
            self.delivered.push_back((now, sent.bytes));
            newest = Some(sent.sent_at);
            if sent.keyframe {
                self.idr_received = Some(now);
            }
            self.in_flight.pop_front();
        }
        if let Some(sent_at) = newest {
            self.ack_samples
                .push_back((now, now.saturating_duration_since(sent_at)));
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
        Ok(())
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
    /// between two receipts. Zero before three receipts.
    pub fn ack_queue(&self, base_rtt: Duration, now: Instant) -> Duration {
        if self.ack_samples.len() < ACK_SAMPLES_MIN {
            return Duration::ZERO;
        }
        let recent = self
            .ack_samples
            .iter()
            .filter(|(at, _)| now.saturating_duration_since(*at) <= ACK_SAMPLE_WINDOW)
            .count()
            .max(ACK_SAMPLES_MIN);
        let mut delays: Vec<Duration> = self
            .ack_samples
            .iter()
            .rev()
            .take(recent)
            .map(|(_, delay)| *delay)
            .collect();
        delays.sort_unstable();
        delays[delays.len() / 2].saturating_sub(base_rtt)
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
        let mut credit = CreditLedger::new(t0);
        // 60 fps at 4 Mbit/s over a 20 ms baseline: 8 frames and 120 KB.
        let base = Some(Duration::from_millis(20));
        let window = credit.window(VideoCodec::H264, 60, 4_000_000, base, t0);
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
        assert!(credit.on_receipt(2, ms(1)).is_err());
        credit.on_receipt(1, ms(10)).unwrap();
        // A repeated receipt is a harmless duplicate.
        credit.on_receipt(1, ms(11)).unwrap();

        // The window closes at its frame count ...
        for frame in 0..8 {
            assert!(credit.fits(KIB, false, &window));
            assert_eq!(credit.book(KIB, false, ms(20)), 2 + frame);
        }
        assert!(!credit.fits(KIB, false, &window));
        // ... and a receipt reopens it.
        credit.on_receipt(3, ms(40)).unwrap();
        assert!(credit.fits(KIB, false, &window));

        // ... and at its byte count, for P-frames.
        let mut credit = CreditLedger::new(t0);
        credit.book(100 * KIB, false, t0);
        assert!(!credit.fits(20 * KIB, false, &window));
        // One IDR may go over the byte window while P-frames are in flight ...
        assert!(credit.fits(200 * KIB, true, &window));
        let idr = credit.book(200 * KIB, true, t0);
        assert_eq!(credit.idr_inflight(), Some(idr));
        // ... and a second one waits for it.
        assert!(!credit.fits(KIB, true, &window));
        credit.on_receipt(idr, ms(30)).unwrap();
        assert_eq!(credit.idr_inflight(), None);
        assert!(credit.fits(200 * KIB, true, &window));

        // Receipts 150 ms slower than the baseline for a full delivery window
        // clamp the byte window to the delivery rate, and release it after.
        let mut credit = CreditLedger::new(t0);
        let mut seq = 0;
        for n in 0..30 {
            seq = credit.book(5 * KIB, false, ms(n * 20));
            credit.on_receipt(seq, ms(n * 20 + 170)).unwrap();
        }
        let now = ms(29 * 20 + 170);
        let slow = credit.window(VideoCodec::H264, 60, 4_000_000, base, now);
        assert!(slow.clamped);
        // 5 KiB every 20 ms is about 2 Mbit/s: ×1.25 over 120 ms is about 39 KB.
        assert!((36_000..=41_000).contains(&slow.bytes), "{slow:?}");
        assert!(slow.bytes < window.bytes);
        for n in 30..50 {
            seq = credit.book(5 * KIB, false, ms(n * 20));
            credit.on_receipt(seq, ms(n * 20 + 25)).unwrap();
        }
        let fast = credit.window(VideoCodec::H264, 60, 4_000_000, base, ms(49 * 20 + 25));
        assert_eq!(fast, window);
        assert_eq!(seq, 50);
        // A slow link receipts seldom: between two receipts fewer than three
        // lie in the sample window, and the clamp still holds.
        let mut credit = CreditLedger::new(t0);
        for n in 0..10 {
            let seq = credit.book(9 * KIB, false, ms(n * 100));
            credit.on_receipt(seq, ms(n * 100 + 400)).unwrap();
        }
        let between = credit.window(VideoCodec::H264, 60, 4_000_000, base, ms(9 * 100 + 460));
        assert!(between.clamped, "{between:?}");

        // A JPEG session has a fixed window.
        let jpeg = CreditLedger::new(t0).window(VideoCodec::Jpeg, 8, 0, None, t0);
        assert_eq!((jpeg.frames, jpeg.bytes), (4, 2 * 1024 * KIB));
    }

    #[test]
    fn a_waiting_unit_ages_from_the_last_idr_receipt() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut credit = CreditLedger::new(t0);
        let idr = credit.book(160 * KIB, true, ms(10));
        credit.book(8 * KIB, false, ms(20));
        // Waiting behind an IDR in flight is not staleness.
        assert_eq!(credit.aging_since(ms(30)), None);
        // Once it is receipted, with the P-frame still in flight, a unit ready
        // before the receipt ages from it, and one ready after from then.
        credit.on_receipt(idr, ms(1400)).unwrap();
        assert_eq!(credit.aging_since(ms(30)), Some(ms(1400)));
        assert_eq!(credit.aging_since(ms(1500)), Some(ms(1500)));

        // A JPEG session books no IDR: a unit always ages from when it was ready.
        let mut jpeg = CreditLedger::new(t0);
        let seq = jpeg.book(300 * KIB, false, ms(10));
        assert_eq!(jpeg.aging_since(ms(30)), Some(ms(30)));
        jpeg.on_receipt(seq, ms(1400)).unwrap();
        assert_eq!(jpeg.aging_since(ms(30)), Some(ms(30)));
    }
}
