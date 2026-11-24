//! Delay-based QoS for one host session. Transport receipts drive the rate
//! (`receipt_rate`, after airhand's `flow.rs`): a standing queue cuts bits
//! per frame within a round trip and keeps the frame rate, which falls only
//! once bits per frame reach the preset's floor, and then to what the
//! delivery rate carries at what a frame costs there: the floor, or what the
//! host measured the encoder spending on its frames, which is less for a
//! light picture and more at an encoder's content floor; the QoS tick raises
//! them again while the receipts show no queue. A probe written in stream
//! order and echoed by the viewer's network task measures downstream
//! queueing, after RustDesk's `video_qos.rs`: fps follows each sample,
//! yielding to the receipts below `SEVERE_DELAY` while they own the queue,
//! and bits per frame follow each 3 s window in which no receipt came (a
//! JPEG session, a still picture). Resolution never adapts, and no fps,
//! bitrate or preset change rebuilds the encoder: the encoder runs at the
//! session's nominal fps, and capture skips ticks to the pace published here.
//! An encoder found to budget per second over the last 2 s of ticks latches
//! the session to the timestamp rate model.
//!
//! This reverses decision D5 and CONTRACT-perf R13 of the overhaul plan, in
//! which the probe owned fps and bitrate and receipts only clamped the byte
//! window (`docs/remote-desktop-overhaul/plan/DECISIONS.md`).
//!
//! Unlike RustDesk, one stalled probe moves nothing: Wi-Fi holds a packet for
//! hundreds of milliseconds now and then without any queue behind it. The
//! delay is the median of the last three samples, a cut needs the receipts to
//! show a standing queue too unless two probes in a row saw one, and a cut
//! the receipts alone confirm at most halves fps.
//!
//! A viewer that stamps its echoes and receipts with arrival times
//! (`arrival_time`) turns the delay one-way: each probe's queue toward the
//! viewer (`oneway`) takes the place of its round trip above the baseline,
//! and the receipts' witness is their own queue toward it. A queue on the
//! viewer's uplink then moves nothing, while one toward it cuts as before.
//! The round trip still sets the baseline the credit window is sized from
//! and the short path.
//!
//! A rebase (two receipt cuts that left the queue where it was: a longer
//! route) moves the probes' baseline up by as much, at most `CUT_DELAY`,
//! until their own 60 s minimum catches up, so both controllers read one
//! baseline.

mod receipt_rate;
#[cfg(test)]
mod sim;

use self::receipt_rate::{Inputs, Lift, ReceiptRate, ReceiptRateView, Verdict};
use crate::{
    QualityPreset, VideoCodec, VideoLimits,
    credit::{ACK_QUEUE_LIMIT, CreditView, Receipt},
    outgoing::ProducedSnapshot,
    protocol::{Adaptation, RateModel},
    wire::IO_TIMEOUT,
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub(crate) const PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// The probe interval on a short path, where recovery may be quick.
pub(crate) const FAST_PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// A base RTT below this is a short path.
pub(crate) const FAST_PATH_RTT: Duration = Duration::from_millis(100);
/// A delay from which fps falls ...
const CUT_DELAY: u32 = 150;
/// ... and from which it falls even while the receipts own the queue.
const SEVERE_DELAY: u32 = 600;
/// The most a rebase adds to the probes' baseline.
const REBASE_MAX: Duration = Duration::from_millis(CUT_DELAY as u64);
/// A rebase smaller than this moves nothing.
const REBASE_MIN: Duration = Duration::from_millis(10);
/// A change the controller made is reported this long before a probe that
/// changes nothing may report `Steady` over it.
const ADAPTATION_HOLD: Duration = Duration::from_secs(1);
/// A probe or receipt owed this long marks the viewer unresponsive.
pub(crate) const UNRESPONSIVE_AFTER: Duration = Duration::from_secs(2);
/// The output pace while the viewer is unresponsive.
pub(crate) const UNRESPONSIVE_FPS: u32 = 2;
pub(crate) const DELAY_FLOOR: Duration = Duration::from_millis(10);
/// Untainted samples whose median is the delay.
pub(crate) const DELAY_HISTORY: usize = 3;
/// How long a sample counts toward the RTT baseline, its minimum.
pub(crate) const RTT_WINDOW: Duration = Duration::from_secs(60);
pub(crate) const QOS_TICK: Duration = Duration::from_millis(250);
/// The bits-per-frame window.
pub(crate) const RATIO_INTERVAL: Duration = Duration::from_secs(3);
/// Unique frames a window needs before bits per frame may rise, and the
/// latch's span before it may latch.
pub(crate) const DYNAMIC_UNIQUE_FRAMES: u64 = 6;
/// The span the rate-model latch reads: the last ticks covering it.
pub(crate) const LATCH_SPAN: Duration = Duration::from_secs(2);
const LATCH_TICKS: usize = (LATCH_SPAN.as_nanos() / QOS_TICK.as_nanos()) as usize;
/// Non-IDR bits above this multiple of the bits expected for them are an
/// overshoot ...
pub(crate) const LATCH_OVERSHOOT: f64 = 1.3;
/// ... and, at a reduced pace, above this share of `nominal / mean pace` too,
/// up to `LATCH_FLOOR_MAX`: an encoder that budgets per second overshoots by
/// that ratio when its pictures fill the budget, while one that budgets per
/// frame overshoots only at its content floor, where the moving picture alone
/// sets the size of each frame.
pub(crate) const LATCH_PACE_SHARE: f64 = 0.6;
/// The most the pace term asks for: at a reduced pace, a session whose encoder
/// sends more than this multiple of the bits expected for its frames latches
/// whatever the pace, so a per-second encoder whose pictures fill only part of
/// its budget is still found.
pub(crate) const LATCH_FLOOR_MAX: f64 = 2.0;
/// The highest measured content floor of a per-frame encoder, as a multiple
/// of its budget: NVIDIA's H.264 MFT at pace 5 of a nominal 120 and 4 Mbit/s
/// (`docs/remote-desktop.md`, "Encoder rate model per GPU").
pub(crate) const MEASURED_CONTENT_FLOOR: f64 = 1.79;
/// The latch looks only at ticks paced at most this share of nominal.
pub(crate) const LATCH_PACE_RATIO: f64 = 0.75;
/// What the encoder spends per frame is read from the newest this many
/// ticks at the budget in force, each with at least `SPENDING_FRAMES`
/// non-IDR frames ...
const SPENDING_TICKS: usize = 2;
const SPENDING_FRAMES: u64 = 2;
/// ... and it outspends its budget above this multiple of it in each, unless
/// its excess fell to this share of the tick before's or less: a VBV that
/// still follows a cut.
const OUTSPEND: f64 = 1.25;
const CONVERGING: f64 = 0.8;
/// The startup pace cap after `Accepted` and after `Resume`.
pub(crate) const STARTUP_HOLD: Duration = Duration::from_secs(1);
pub(crate) const JPEG_MAX_FPS: u32 = 8;
/// The encoder bitrate's bounds, which the encoder and the helper check too.
pub(crate) const ENCODER_BITRATE_MIN: u32 = 256_000;
pub(crate) const ENCODER_BITRATE_MAX: u32 = 80_000_000;
/// Preset bits per frame are given at 1080p. Another encoded size scales them
/// by its pixels relative to 1080p to this power, within these bounds: a
/// picture's bits grow slower than its pixels.
const FULL_HD_PIXELS: f64 = 1920.0 * 1080.0;
const PIXEL_SCALE_EXPONENT: f64 = 0.75;
const PIXEL_SCALE_MIN: f64 = 0.4;
const PIXEL_SCALE_MAX: f64 = 3.0;
/// The most one window may raise the expected bitrate, in bit/s.
const BPF_RAISE_MAX_BPS: f64 = 150_000.0;
/// How long after a session its transport ended the bits per frame it ended
/// at may start another (`ClosingRate`).
pub(crate) const START_MEMORY: Duration = Duration::from_secs(120);

const _: () = assert!(
    ENCODER_BITRATE_MIN <= ENCODER_BITRATE_MAX,
    "targets clamps the encoder bitrate between them"
);
const _: () = assert!(
    PIXEL_SCALE_MIN <= PIXEL_SCALE_MAX,
    "pixel_scale clamps between them"
);
const _: () = assert!(
    FAST_PROBE_INTERVAL.as_nanos() < PROBE_INTERVAL.as_nanos(),
    "a short path is probed more often"
);
const _: () = assert!(DELAY_HISTORY >= 1, "the delay is the median of the history");
const _: () = assert!(
    SEVERE_DELAY > CUT_DELAY,
    "the receipts own a band of delays above the cut"
);
const _: () = assert!(
    REBASE_MIN.as_nanos() < REBASE_MAX.as_nanos(),
    "a rebase between them offsets the probes' baseline"
);
const _: () = assert!(
    LATCH_TICKS >= 1 && LATCH_TICKS as u128 * QOS_TICK.as_nanos() == LATCH_SPAN.as_nanos(),
    "the latch's span is whole ticks"
);
const _: () = assert!(
    SPENDING_TICKS == 2 && SPENDING_TICKS <= LATCH_TICKS && SPENDING_FRAMES >= 1,
    "spending compares the newest tick with the one before, both in the span"
);
const _: () = assert!(
    OUTSPEND > 1.0 && CONVERGING < 1.0,
    "an encoder outspends above its budget, and a falling excess converges"
);
const _: () = assert!(
    LATCH_PACE_SHARE < 1.0,
    "a per-second budget, which overshoots by nominal / pace, must pass the latch"
);
const _: () = assert!(
    LATCH_OVERSHOOT <= LATCH_FLOOR_MAX,
    "the pace term only ever raises the latch's threshold"
);
const _: () = assert!(
    MEASURED_CONTENT_FLOOR < LATCH_FLOOR_MAX,
    "a per-frame encoder at its measured content floor must not latch"
);

/// A preset's fps and bits per frame (at 1080p).
struct Preset {
    min_fps: u32,
    normal_fps: u32,
    ceiling_fps: u32,
    bpf_start: f64,
    bpf_max: f64,
    bpf_floor: f64,
    /// Where a receipt cut stops lowering bits per frame and lowers the pace
    /// instead, down to `min_fps`; only below that do bits per frame fall to
    /// `bpf_floor`. Quality keeps half its start, for sharpness.
    cut_floor: f64,
}
const QUALITY: Preset = Preset {
    min_fps: 8,
    normal_fps: 24,
    ceiling_fps: 30,
    bpf_start: 200_000.0,
    bpf_max: 400_000.0,
    bpf_floor: 50_000.0,
    cut_floor: 100_000.0,
};
const BALANCED: Preset = Preset {
    min_fps: 10,
    normal_fps: 45,
    ceiling_fps: 60,
    bpf_start: 66_667.0,
    bpf_max: 133_333.0,
    bpf_floor: 16_667.0,
    cut_floor: 16_667.0,
};
const SMOOTH: Preset = Preset {
    min_fps: 12,
    normal_fps: 50,
    ceiling_fps: 120,
    bpf_start: 50_000.0,
    bpf_max: 100_000.0,
    bpf_floor: 12_500.0,
    cut_floor: 12_500.0,
};
/// A preset's fps and bits per frame are each in order, floor to max, and a
/// receipt cut turns to the pace between its floor and its start.
const fn ordered(p: &Preset) -> bool {
    p.min_fps <= p.normal_fps
        && p.normal_fps <= p.ceiling_fps
        && p.bpf_floor <= p.bpf_start
        && p.bpf_start <= p.bpf_max
        && p.bpf_floor <= p.cut_floor
        && p.cut_floor <= p.bpf_start
}
const _: () = assert!(
    ordered(&QUALITY) && ordered(&BALANCED) && ordered(&SMOOTH),
    "clamp_bpf clamps between a preset's floor and max"
);
fn preset(quality: QualityPreset) -> &'static Preset {
    match quality {
        QualityPreset::Quality => &QUALITY,
        QualityPreset::Balanced => &BALANCED,
        QualityPreset::Smooth => &SMOOTH,
    }
}

/// A preset's fps ceiling, before the viewer's `limits.max_fps`, which
/// replaces it when the viewer lists `fps_override`. The viewer's fps cap is
/// bounded by it too.
pub(crate) fn ceiling_fps(quality: QualityPreset) -> u32 {
    preset(quality).ceiling_fps
}

/// Bits per frame at `width` × `height` relative to 1080p.
fn pixel_scale(width: u32, height: u32) -> f64 {
    (f64::from(width) * f64::from(height) / FULL_HD_PIXELS)
        .powf(PIXEL_SCALE_EXPONENT)
        .clamp(PIXEL_SCALE_MIN, PIXEL_SCALE_MAX)
}

/// The size the hardware encoder makes of a `width` × `height` display for a
/// viewer's bounds: fitted inside them, keeping its aspect ratio, never
/// upscaled, and even, as NV12 needs. A session's bits per frame start from it.
pub(crate) fn scaled_size(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    // Integer arithmetic avoids rounding beyond the renderer's bounds; at most
    // one source edge pixel is lost to rounding. A degenerate display a
    // backend reports must not divide by zero.
    let (width, height) = (width.max(1), height.max(1));
    let target_width = width.min(max_width);
    let target_height = height.min(max_height);
    let (width, height) = if u64::from(target_width) * u64::from(height)
        <= u64::from(target_height) * u64::from(width)
    {
        (
            target_width,
            (u64::from(height) * u64::from(target_width) / u64::from(width)) as u32,
        )
    } else {
        (
            (u64::from(width) * u64::from(target_height) / u64::from(height)) as u32,
            target_height,
        )
    };
    ((width & !1).max(2), (height & !1).max(2))
}

/// Where a session that its transport ended left its rate: its bits per
/// frame relative to 1080p, under its preset, and when it ended. A session
/// of the same preset accepted within `START_MEMORY` of that starts no
/// higher (`Qos::new`): a viewer reconnecting over the link that just ended
/// its session meets it at the rate the link last carried.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClosingRate {
    pub preset: QualityPreset,
    /// Bits per frame at 1080p: the encoded size's divided by its pixel
    /// scale, so a session of another size starts from its own share.
    pub bpf: f64,
    pub ended: Instant,
}
impl ClosingRate {
    /// The bits per frame at 1080p this record starts a session of `preset`
    /// accepted at `now` from, if it applies: the same preset, within
    /// `START_MEMORY` of the end.
    fn start(&self, preset: QualityPreset, now: Instant) -> Option<f64> {
        (self.preset == preset && now.saturating_duration_since(self.ended) <= START_MEMORY)
            .then_some(self.bpf)
    }
}

/// What thread C is asked for, published after every change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TargetValues {
    pub pace_fps: u32,
    pub encoder_bps: u32,
    pub paused: bool,
}

/// The controller's state for the stats and the trace.
#[derive(Clone, Copy, Debug)]
pub(crate) struct QosView {
    pub fps: u32,
    pub pace_fps: u32,
    pub viewer_cap: Option<u32>,
    pub paused: bool,
    pub unresponsive: bool,
    pub bpf: u32,
    pub expected_bps: u64,
    pub encoder_bps: u32,
    pub rate_model: RateModel,
    pub rtt: Option<Duration>,
    pub base_rtt: Option<Duration>,
    pub delay_ms: Option<u32>,
    /// The last probe was written behind an IDR in flight.
    pub tainted: bool,
    pub adaptation: Adaptation,
    /// The pace cap the receipts set from the delivery rate.
    pub rate_fps: Option<u32>,
    /// What a rebase added to the probes' baseline.
    pub rebase: Option<Duration>,
}

/// A rebase of the probes' baseline: `offset` above it, until their own base
/// reaches `level` (round trips) or their samples fall below half the offset.
#[derive(Clone, Copy, Debug)]
struct Rebase {
    offset: Duration,
    level: Duration,
}

struct Probe {
    nonce: u64,
    sent: Instant,
    tainted: bool,
}

/// One bits-per-frame window: the worst untainted delay, the worst tainted
/// one, and the production counters it started from.
struct RatioWindow {
    started: Instant,
    max_delay: Option<u32>,
    max_tainted: Option<u32>,
    produced: ProducedSnapshot,
}
impl RatioWindow {
    fn new(started: Instant, produced: ProducedSnapshot) -> Self {
        Self {
            started,
            max_delay: None,
            max_tainted: None,
            produced,
        }
    }
}

/// One QoS tick's production, as the rate-model latch reads it.
#[derive(Clone, Copy, Debug, Default)]
struct TickBucket {
    elapsed: Duration,
    frames: u64,
    unique: u64,
    non_idr_frames: u64,
    non_idr_bits: u64,
    /// The tick's non-IDR frames × the highest per-frame budget the encoder
    /// was given during the tick: a cut never makes the frames encoded before
    /// it look like an overshoot.
    expected_bits: f64,
    /// The tick's time × the encoder bitrate at its end, × the non-IDR share
    /// of its frames: what an encoder that budgets per second spends on them.
    rate_bits: f64,
    /// The pace at both ends of the tick was at most `LATCH_PACE_RATIO` ×
    /// nominal. A slot no tick has filled yet is not.
    paced_low: bool,
}
impl TickBucket {
    fn add(mut self, other: &Self) -> Self {
        let Self {
            elapsed,
            frames,
            unique,
            non_idr_frames,
            non_idr_bits,
            expected_bits,
            rate_bits,
            paced_low,
        } = *other;
        self.elapsed += elapsed;
        self.frames += frames;
        self.unique += unique;
        self.non_idr_frames += non_idr_frames;
        self.non_idr_bits = self.non_idr_bits.saturating_add(non_idr_bits);
        self.expected_bits += expected_bits;
        self.rate_bits += rate_bits;
        self.paced_low &= paced_low;
        self
    }
}

/// The last `LATCH_TICKS` ticks' production, in a fixed ring, and where the
/// tick under way began.
struct LatchSpan {
    buckets: [TickBucket; LATCH_TICKS],
    /// The slot the next tick's bucket replaces.
    next: usize,
    since: Instant,
    produced: ProducedSnapshot,
    pace: u32,
    /// The highest per-frame budget in force since the tick under way began:
    /// every change of bits per frame raises it (`Qos::set_bpf`).
    peak_budget: f64,
}
impl LatchSpan {
    fn new(since: Instant, pace: u32, budget: f64) -> Self {
        Self {
            buckets: [TickBucket::default(); LATCH_TICKS],
            next: 0,
            since,
            produced: ProducedSnapshot::default(),
            pace,
            peak_budget: budget,
        }
    }

    fn push(&mut self, bucket: TickBucket) {
        if let Some(slot) = self.buckets.get_mut(self.next) {
            *slot = bucket;
        }
        self.next = (self.next + 1) % LATCH_TICKS;
    }

    /// The buckets, newest first.
    fn newest(&self) -> impl Iterator<Item = &TickBucket> {
        // The slots before `next` were filled last, those from it before them.
        let (recent, earlier) = self.buckets.split_at(self.next.min(LATCH_TICKS));
        earlier.iter().chain(recent).rev()
    }

    /// The span's totals.
    fn total(&self) -> TickBucket {
        let start = TickBucket {
            paced_low: true,
            ..TickBucket::default()
        };
        self.buckets.iter().fold(start, TickBucket::add)
    }
}

/// What the encoder spent per non-IDR frame over the newest ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Spending {
    bits: f64,
    /// Above its budget and not falling toward it: an encoder at its content
    /// floor, or one that budgets per second at a reduced pace.
    outspends: bool,
}

/// What a frame costs on the wire at a budget: the budget, unless the
/// encoder was measured spending less (a light picture), or outspending it,
/// which a lower budget does not change.
#[derive(Clone, Copy, Debug)]
struct FrameCost(Option<Spending>);
impl FrameCost {
    fn at(self, budget: f64) -> f64 {
        let cost = match self.0 {
            Some(Spending {
                bits,
                outspends: true,
            }) => bits.max(budget),
            Some(Spending {
                bits,
                outspends: false,
            }) => bits.min(budget),
            None => budget,
        };
        cost.max(1.0)
    }
}

pub(crate) struct Qos {
    limits: VideoLimits,
    codec: VideoCodec,
    /// `limits.max_fps` is every preset's ceiling: the viewer's user chose it.
    fps_override: bool,
    preset: QualityPreset,
    min_fps: u32,
    normal_fps: u32,
    ceiling_fps: u32,
    /// `ceiling / 30`, at least 1: the fps steps grow with the ceiling.
    scale: f64,
    pixel_scale: f64,
    fps: u32,
    quick: u32,
    increase: u32,
    last_delay: Option<u32>,
    /// The last untainted sample's own delay, before the median.
    last_raw: Option<u32>,
    /// Balanced may jump to its ceiling at the next untainted sample.
    start_high: bool,
    /// Bits per frame at the encoded size.
    bpf: f64,
    rate_model: RateModel,
    /// The newest ticks' production: what the rate-model latch reads while
    /// the model is `Nominal`, and what the encoder spends (`spending`).
    span: LatchSpan,
    window: RatioWindow,
    /// An untainted sample was seen this session.
    sampled: bool,
    next_nonce: u64,
    outstanding: Option<Probe>,
    last_probe: Option<Instant>,
    /// `(echoed at, rtt)` over the last `RTT_WINDOW`.
    rtt: VecDeque<(Instant, Duration)>,
    last_rtt: Option<Duration>,
    /// The newest untainted samples: round trips, or with arrival times,
    /// queues toward the viewer.
    history: VecDeque<Duration>,
    /// The viewer stamps arrival times: the delay is one-way.
    one_way: bool,
    tainted: bool,
    viewer_cap: Option<u32>,
    paused: bool,
    unresponsive: bool,
    startup_until: Instant,
    adaptation: Adaptation,
    /// Until when `adaptation` is held against `Steady`.
    adaptation_until: Option<Instant>,
    /// The receipt-driven controller.
    receipts: ReceiptRate,
    /// The pace cap a receipt cut set below the preset's cut floor, from
    /// the delivery rate.
    rate_fps: Option<u32>,
    /// What a receipt rebase added to the probes' baseline.
    rebase: Option<Rebase>,
    /// Counts the changes of `rebase`'s offset, so the credit ledger takes
    /// only the newest (`CreditLedger::set_rebase`).
    rebase_generation: u64,
}
impl Qos {
    /// Bits per frame start at the viewer's bounds, until `set_video_size`
    /// says what the encoder produces: the host's expectation before the first
    /// unit, then each unit's own size. They start at the preset's start, or
    /// lower where the last session its transport ended (`previous`) ended
    /// lower, of the same preset and within `START_MEMORY`; the frame rate
    /// starts as the preset's always does. The rate model starts at the
    /// backend's (`BackendCaps::rate_model`).
    pub fn new(
        limits: VideoLimits,
        codec: VideoCodec,
        quality: QualityPreset,
        fps_override: bool,
        rate_model: RateModel,
        previous: Option<ClosingRate>,
        now: Instant,
    ) -> Self {
        let mut qos = Self {
            limits,
            codec,
            fps_override,
            preset: quality,
            min_fps: 1,
            normal_fps: 1,
            ceiling_fps: 1,
            scale: 1.0,
            pixel_scale: pixel_scale(limits.max_width, limits.max_height),
            fps: 1,
            quick: 0,
            increase: 0,
            last_delay: None,
            last_raw: None,
            start_high: false,
            bpf: 0.0,
            rate_model,
            span: LatchSpan::new(now, 0, 0.0),
            window: RatioWindow::new(now, ProducedSnapshot::default()),
            sampled: false,
            next_nonce: 1,
            outstanding: None,
            last_probe: None,
            rtt: VecDeque::new(),
            last_rtt: None,
            history: VecDeque::with_capacity(DELAY_HISTORY),
            one_way: false,
            tainted: false,
            viewer_cap: None,
            paused: false,
            unresponsive: false,
            startup_until: now + STARTUP_HOLD,
            adaptation: Adaptation::Steady,
            adaptation_until: None,
            receipts: ReceiptRate::new(),
            rate_fps: None,
            rebase: None,
            rebase_generation: 0,
        };
        qos.apply_preset(quality);
        if let Some(previous) = previous
            && let Some(start) = previous.start(quality, now)
        {
            let bpf = qos.clamp_bpf(start * qos.pixel_scale);
            if bpf < qos.bpf {
                qos.set_bpf(bpf);
                tracing::info!(
                    target: "pb_mapper_desktop::feedback",
                    event = "desktop_start_rate_remembered",
                    preset = ?quality,
                    bpf_1080p = start as u32,
                    preset_start_1080p = preset(quality).bpf_start as u32,
                    ended_ms_ago = millis(now.saturating_duration_since(previous.ended)),
                );
            }
        }
        // Built after bits per frame are set, so the first tick's budget is
        // the one the session starts at.
        qos.span = LatchSpan::new(now, qos.pace(now), qos.nominal_budget());
        qos
    }

    /// Where this session's rate stands at `now`, as a record for the next
    /// session should its transport end it now.
    pub fn closing_rate(&self, now: Instant) -> ClosingRate {
        ClosingRate {
            preset: self.preset,
            bpf: self.bpf / self.pixel_scale,
            ended: now,
        }
    }

    /// The fps and bits per frame a preset starts from. Balanced H.264 and
    /// H.265 start at the ceiling instead at the next untainted probe, on any
    /// path: the receipts cut within a round trip if it cannot carry that.
    fn apply_preset(&mut self, quality: QualityPreset) {
        let p = preset(quality);
        self.preset = quality;
        self.ceiling_fps = if self.fps_override {
            self.limits.max_fps
        } else {
            p.ceiling_fps.min(self.limits.max_fps)
        };
        self.normal_fps = p.normal_fps.min(self.ceiling_fps);
        self.min_fps = p.min_fps;
        self.scale = (f64::from(self.ceiling_fps) / 30.0).max(1.0);
        self.fps = self.normal_fps;
        self.start_high = quality == QualityPreset::Balanced && self.codec != VideoCodec::Jpeg;
        self.set_bpf(p.bpf_start * self.pixel_scale);
    }

    /// A live preset change: fps and bits per frame restart from the preset,
    /// the receipts' controller and the rebase start again, the rate model is
    /// kept, and nothing rebuilds the encoder.
    pub fn set_preset(&mut self, quality: QualityPreset, now: Instant, produced: ProducedSnapshot) {
        self.apply_preset(quality);
        self.quick = 0;
        self.increase = 0;
        self.window = RatioWindow::new(now, produced);
        self.receipts.reset();
        self.rate_fps = None;
        self.set_rebase(None);
    }

    /// The viewer's runtime fps cap; a cap at or above `limits.max_fps` clears it.
    pub fn set_viewer_cap(&mut self, fps: u32) {
        self.viewer_cap = (fps < self.limits.max_fps).then_some(fps);
    }

    pub fn set_paused(&mut self) {
        self.paused = true;
        self.unresponsive = false;
    }

    /// Back through the startup cap, with a fresh delay and receipt history,
    /// no episode or hold, and no rebase.
    pub fn resume(&mut self, now: Instant) {
        self.paused = false;
        self.unresponsive = false;
        self.startup_until = now + STARTUP_HOLD;
        self.history.clear();
        self.last_raw = None;
        self.receipts.restart();
        self.set_rebase(None);
    }

    /// A new viewer path: the old baseline, the receipts, their capacity and
    /// the rebase mean nothing. fps and bits per frame are kept.
    pub fn reset_baseline(&mut self) {
        self.rtt.clear();
        self.history.clear();
        self.last_raw = None;
        self.receipts.reset();
        self.set_rebase(None);
    }

    /// The viewer stamps arrival times: from now on the delay, the witness
    /// and the receipts' controller read queues toward it, which the credit
    /// ledger's one-way minima give (`CreditLedger::probe_queue`).
    pub fn use_one_way(&mut self) {
        self.one_way = true;
        self.history.clear();
        self.last_raw = None;
        self.receipts.use_one_way();
        self.set_rebase(None);
    }

    /// A new encoded size: bits per frame keep their 1080p-relative level.
    pub fn set_video_size(&mut self, width: u32, height: u32) {
        let scale = pixel_scale(width, height);
        if scale != self.pixel_scale {
            let bpf = self.bpf * (scale / self.pixel_scale);
            self.pixel_scale = scale;
            self.set_bpf(self.clamp_bpf(bpf));
        }
    }

    /// A transport receipt arrived: the viewer is alive. A new one, not a
    /// duplicate, of H.264 or H.265 video that is not paused also feeds the
    /// receipts' controller, which with arrival times takes only receipts
    /// whose stamp was a sample, and may cut the rate or rebase. `blocked`:
    /// a clamped window refused a unit since the last receipt.
    pub fn on_receipt(&mut self, receipt: Option<Receipt>, blocked: bool, now: Instant) {
        self.unresponsive = false;
        if let Some(receipt) = receipt
            && self.codec != VideoCodec::Jpeg
            && !self.paused
        {
            let inputs = self.receipt_inputs(blocked, now);
            match self.receipts.on_receipt(receipt, now, inputs) {
                Verdict::None | Verdict::Hold => {}
                Verdict::Cut(target) => self.cut_to(target, now),
                Verdict::Rebased(least) => self.rebase_to(least),
            }
        }
    }

    /// What the receipts' controller is told: the pace and expected rate,
    /// and what the encoder sends at that pace and at the preset's floor, as
    /// the host measured its frames (`spending`).
    fn receipt_inputs(&self, blocked: bool, now: Instant) -> Inputs {
        let pace = f64::from(self.pace(now));
        let cost = FrameCost(self.spending());
        let floor = preset(self.preset).bpf_floor * self.pixel_scale;
        let least_pace = self.min_fps.min(self.steady_pace());
        Inputs {
            base: self.base_rtt(),
            pace: self.pace(now),
            expected_bps: self.bpf * pace,
            sent_bps: cost.at(self.bpf) * pace,
            floor_bps: cost.at(floor) * f64::from(least_pace),
            rtt: self.last_rtt,
            blocked,
            capped: self.rate_fps.is_some(),
        }
    }

    /// What the encoder spent per non-IDR frame over the newest
    /// `SPENDING_TICKS` ticks, each with `SPENDING_FRAMES` or more, and
    /// whether it outspends its budget; `None` when one of them had too few
    /// frames, or was encoded at a higher budget than now and spent more than
    /// the budget now: the frames from before a cut, which the encoder no
    /// longer makes. A light picture's frames, under either budget, stand.
    ///
    /// An encoder outspends when, in both ticks, its frames cost more than
    /// `OUTSPEND` × their budget, and its excess is not falling as a VBV's
    /// does. An encoder that budgets per second makes larger frames whenever
    /// fewer are captured than the pace (the window holding captures back),
    /// though its bits per second are what it is asked for: under the
    /// timestamp model, which the latch chose for such an encoder, one
    /// outspends only if its bits also exceed `OUTSPEND` × what its bitrate
    /// gives the ticks' time. The nominal model takes the encoder to budget
    /// per frame; one that budgets per second instead sheds none of its bits
    /// through the pace until the latch finds it.
    fn spending(&self) -> Option<Spending> {
        let budget = self.nominal_budget();
        let mut excess = [0.0; SPENDING_TICKS];
        let mut per_second = false;
        let (mut bits, mut frames) = (0u64, 0u64);
        let mut ticks = 0;
        for (slot, bucket) in excess.iter_mut().zip(self.span.newest()) {
            let at_budget = budget * bucket.non_idr_frames as f64;
            let stale =
                bucket.expected_bits > at_budget * 1.001 && bucket.non_idr_bits as f64 > at_budget;
            if bucket.non_idr_frames < SPENDING_FRAMES || stale {
                return None;
            }
            *slot = bucket.non_idr_bits as f64 / at_budget - 1.0;
            per_second |= (bucket.non_idr_bits as f64) < OUTSPEND * bucket.rate_bits;
            bits = bits.saturating_add(bucket.non_idr_bits);
            frames += bucket.non_idr_frames;
            ticks += 1;
        }
        if ticks < SPENDING_TICKS {
            return None;
        }
        let [newest, before] = excess;
        let outspends = newest >= OUTSPEND - 1.0
            && before >= OUTSPEND - 1.0
            && newest > CONVERGING * before
            && !(per_second && self.rate_model == RateModel::Timestamp);
        Some(Spending {
            bits: bits as f64 / frames as f64,
            outspends,
        })
    }

    /// The receipts' controller's values for the once-a-second trace.
    pub fn receipt_view(&self, now: Instant) -> ReceiptRateView {
        self.receipts.view(now, self.base_rtt(), self.pace(now))
    }

    /// The delivery rate the byte window follows while a receipt episode
    /// lasts; `None` otherwise, for JPEG, and while paused.
    pub fn clamp_rate(&self, now: Instant) -> Option<u64> {
        (self.codec != VideoCodec::Jpeg && !self.paused)
            .then(|| self.receipts.clamp_rate(now))
            .flatten()
    }

    /// A receipt cut to `target` bit/s at the pace before the startup cap:
    /// bits per frame first, down to the preset's cut floor; then the pace,
    /// from the delivery rate at what a frame then costs, down to the
    /// preset's minimum; below that bits per frame again, down to the
    /// preset's floor. For every preset but Quality the two floors are one.
    /// A frame costs its budget, unless the host measured the encoder
    /// spending less (a light picture) or outspending it (a content floor,
    /// where a lower budget sheds nothing): the pace then carries what it
    /// costs. A cut never raises either.
    fn cut_to(&mut self, target: f64, now: Instant) {
        let pace = self.steady_pace();
        let cut_floor = preset(self.preset).cut_floor * self.pixel_scale;
        let floor = preset(self.preset).bpf_floor * self.pixel_scale;
        let min_fps = self.min_fps.min(pace);
        let cost = FrameCost(self.spending());
        let mut bpf = self.bpf.min((target / f64::from(pace)).max(cut_floor));
        let mut fps = pace;
        // A hair over, so a budget of exactly `target / pace` carries the pace.
        let mut carried = (target / cost.at(bpf) * (1.0 + 1e-9)) as u32;
        // Frames that cost more than their budget at most halve the pace a
        // cut: one that budgets per second makes them too while the window
        // holds captures back, and a lower pace sheds none of its bits.
        if cost.at(bpf) > bpf {
            carried = carried.max(pace.div_ceil(2));
        }
        if carried < pace {
            if carried >= min_fps {
                fps = carried;
            } else {
                fps = min_fps;
                bpf = bpf.min((target / f64::from(min_fps)).max(floor));
            }
        }
        let lowered_fps = fps < pace;
        if lowered_fps {
            self.rate_fps = Some(fps);
        }
        let lowered_bpf = bpf < self.bpf;
        if lowered_bpf {
            self.set_bpf(bpf);
        }
        if lowered_fps {
            self.adapt(Adaptation::LoweringFps, now);
        } else if lowered_bpf {
            self.adapt(Adaptation::LoweringBitrate, now);
        }
    }

    /// A receipt raise from `expected`: the receipts' pace cap first, so the
    /// frame rate comes back before bits per frame (for Quality, bits per
    /// frame below its cut floor before that), then bits per frame, up to the
    /// preset's maximum. A light picture's raise lifts the pace cap alone.
    fn raise_to(&mut self, lift: Lift, expected: f64, now: Instant) {
        let factor = lift.to / expected.max(1.0);
        let (bpf, rate_fps) = (self.bpf, self.rate_fps);
        let cut_floor = preset(self.preset).cut_floor * self.pixel_scale;
        match self.rate_fps {
            Some(_) if self.bpf < cut_floor && !lift.pace_only => {
                self.set_bpf((self.bpf * factor).min(cut_floor));
            }
            Some(cap) => {
                let fps = (f64::from(cap) * factor).ceil() as u32;
                self.rate_fps = (fps < self.natural_pace()).then_some(fps.max(cap));
            }
            None if lift.pace_only => {}
            None => self.set_bpf(self.clamp_bpf(self.bpf * factor).max(self.bpf)),
        }
        if self.bpf > bpf || self.rate_fps != rate_fps {
            self.adapt(Adaptation::Raising, now);
        }
    }

    /// Two receipt cuts left the queue where it was: the route is longer. The
    /// probes' baseline moves up by as much as the receipts' did, at most
    /// `REBASE_MAX`.
    fn rebase_to(&mut self, least: Duration) {
        let base = if self.one_way {
            self.floor_gap()
        } else {
            self.base_rtt().unwrap_or(Duration::ZERO)
        };
        let offset = least.saturating_sub(base).min(REBASE_MAX);
        self.set_rebase((offset >= REBASE_MIN).then_some(Rebase {
            offset,
            level: base + offset,
        }));
    }

    /// Every change of the rebase goes through here, so a change of its
    /// offset counts a generation.
    fn set_rebase(&mut self, rebase: Option<Rebase>) {
        let offset = |rebase: Option<Rebase>| rebase.map(|rebase| rebase.offset);
        if offset(rebase) != offset(self.rebase) {
            self.rebase_generation += 1;
        }
        self.rebase = rebase;
    }

    /// What a rebase adds to the probes' baseline.
    pub fn rebase_offset(&self) -> Duration {
        self.rebase.map_or(Duration::ZERO, |rebase| rebase.offset)
    }

    /// The rebase's offset and the count of its changes, which the credit
    /// ledger reads too (`CreditLedger::set_rebase`): of two writes that race
    /// there, the older generation is ignored.
    pub fn rebase_state(&self) -> (u64, Duration) {
        (self.rebase_generation, self.rebase_offset())
    }

    /// Report `adaptation`; a probe or window that changes nothing reports
    /// `Steady` only once the last change has been reported for
    /// `ADAPTATION_HOLD`.
    fn adapt(&mut self, adaptation: Adaptation, now: Instant) {
        if adaptation == Adaptation::Steady {
            if self.adaptation_until.is_some_and(|until| now < until) {
                return;
            }
            self.adaptation_until = None;
        } else {
            self.adaptation_until = Some(now + ADAPTATION_HOLD);
        }
        self.adaptation = adaptation;
    }

    /// The nonce of the probe to write now, if one is due.
    pub fn probe_due(&self, now: Instant) -> Option<u64> {
        let interval = self.probe_interval();
        (self.outstanding.is_none()
            && self
                .last_probe
                .is_none_or(|last| now.saturating_duration_since(last) >= interval))
        .then_some(self.next_nonce)
    }

    /// When the next probe is due; `None` while one is outstanding.
    pub fn probe_due_at(&self, now: Instant) -> Option<Instant> {
        self.outstanding.is_none().then(|| {
            self.last_probe
                .map_or(now, |last| last + self.probe_interval())
        })
    }

    /// A short path is probed four times as often, so fps recovers in about
    /// a second after a cut rather than in ten.
    fn probe_interval(&self) -> Duration {
        if self.short_path() {
            FAST_PROBE_INTERVAL
        } else {
            PROBE_INTERVAL
        }
    }

    fn short_path(&self) -> bool {
        self.base_rtt().is_some_and(|base| base < FAST_PATH_RTT)
    }

    /// `tainted`: the probe went out behind an IDR in flight, so its RTT
    /// includes that IDR's transfer and must not drive the fps controller.
    pub fn probe_sent(&mut self, nonce: u64, now: Instant, tainted: bool) {
        self.outstanding = Some(Probe {
            nonce,
            sent: now,
            tainted,
        });
        self.last_probe = Some(now);
        self.next_nonce = nonce + 1;
    }

    /// When the outstanding probe went out, if `nonce` is its.
    pub fn probe_sent_at(&self, nonce: u64) -> Option<Instant> {
        self.outstanding
            .as_ref()
            .filter(|probe| probe.nonce == nonce)
            .map(|probe| probe.sent)
    }

    /// An echo of the outstanding probe; any other nonce is ignored.
    /// `forward` is the probe's queue toward a viewer that stamps arrival
    /// times, `None` when it stamped none or the stamp was no sample, in
    /// which case its round trip still counts toward the baseline but moves
    /// no decision. A round trip over `IO_TIMEOUT` is no sample of the path,
    /// but its queue toward such a viewer still is: a queue on the viewer's
    /// uplink lengthens the one and not the other. `ack_queue` is the
    /// receipts' queue delay at the echo, the witness a cut needs unless the
    /// probes' own delay persists.
    pub fn on_probe_echo(
        &mut self,
        nonce: u64,
        now: Instant,
        forward: Option<Duration>,
        ack_queue: Option<Duration>,
    ) {
        let Some(probe) = self.outstanding.take_if(|probe| probe.nonce == nonce) else {
            return;
        };
        self.unresponsive = false;
        let elapsed = now.saturating_duration_since(probe.sent);
        let timely = elapsed <= IO_TIMEOUT;
        if !timely && !(self.one_way && forward.is_some()) {
            return;
        }
        let rtt = elapsed.max(DELAY_FLOOR);
        if timely {
            self.last_rtt = Some(rtt);
            while self
                .rtt
                .front()
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) > RTT_WINDOW)
            {
                self.rtt.pop_front();
            }
            // Kept unfloored, so the one-way delay can hide what the floor
            // hides.
            self.rtt.push_back((now, elapsed));
        }
        self.tainted = probe.tainted;
        let Some(base) = self.base_rtt() else {
            return;
        };
        // This probe's own sample: its queue toward the viewer, less what the
        // floor hides of a round trip on a path shorter than it, or its round
        // trip, whose excess over the baseline is its delay.
        let (sample, above) = match (self.one_way, forward) {
            (true, Some(forward)) => {
                let forward = forward.saturating_sub(self.floor_gap());
                (forward, forward)
            }
            (true, None) => return,
            (false, _) => (rtt, rtt.saturating_sub(base)),
        };
        // A rebase ends once the probes' own base has risen to it, or their
        // samples show the longer route gone.
        if self.rebase.is_some_and(|rebase| {
            above < rebase.offset / 2 || (!self.one_way && base >= rebase.level)
        }) {
            self.set_rebase(None);
        }
        let raw = millis(above.saturating_sub(self.rebase_offset()));
        if probe.tainted {
            // Kept apart, for a window that has nothing else to go on.
            let max = &mut self.window.max_tainted;
            *max = Some(max.map_or(raw, |max| max.max(raw)));
            return;
        }
        if self.history.len() == DELAY_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(sample);
        let Some(delay) = self.delay_ms() else {
            return;
        };
        if std::mem::take(&mut self.start_high) {
            self.fps = self.ceiling_fps;
        }
        let witnessed = ack_queue.is_some_and(|queue| queue >= ACK_QUEUE_LIMIT);
        let owned = self.receipts.owns(now);
        self.control_fps(delay, raw, witnessed, owned, now);
        if !self.sampled {
            // As RustDesk: the first delay also sets bits per frame at once,
            // which can only lower them.
            self.sampled = true;
            self.step_bpf(delay, false, now);
        }
    }

    /// The rate-model latch, unresponsiveness, the bits-per-frame window when
    /// it is due, and the receipts' raise.
    pub fn tick(&mut self, now: Instant, credit: &CreditView, produced: ProducedSnapshot) {
        if self.record_tick(now, produced) && self.rate_model == RateModel::Nominal {
            self.latch();
        }
        if !self.paused {
            let probe_late = self.outstanding.as_ref().is_some_and(|probe| {
                now.saturating_duration_since(probe.sent) > UNRESPONSIVE_AFTER
            });
            let receipt_late = credit
                .owed_since
                .is_some_and(|since| now.saturating_duration_since(since) > UNRESPONSIVE_AFTER);
            if probe_late || receipt_late {
                self.unresponsive = true;
            }
        }
        if now.saturating_duration_since(self.window.started) >= RATIO_INTERVAL {
            self.close_window(now, produced);
        }
        if !self.paused && self.codec != VideoCodec::Jpeg {
            let inputs = self.receipt_inputs(false, now);
            if let Some(lift) = self.receipts.raise(now, inputs) {
                self.raise_to(lift, inputs.expected_bps, now);
            }
        }
    }

    pub fn targets(&self, now: Instant) -> TargetValues {
        let pace_fps = self.pace(now);
        let bps = match self.rate_model {
            RateModel::Nominal => self.bpf * f64::from(self.limits.max_fps),
            RateModel::Timestamp => self.bpf * f64::from(pace_fps),
        };
        TargetValues {
            pace_fps,
            encoder_bps: encoder_bitrate(bps),
            paused: self.paused,
        }
    }

    /// The per-frame budget the nominal model hands the encoder: its bitrate
    /// over the nominal rate, which is bits per frame unless the encoder
    /// bitrate's bounds clamp it.
    fn nominal_budget(&self) -> f64 {
        let nominal = f64::from(self.limits.max_fps.max(1));
        f64::from(encoder_bitrate(self.bpf * nominal)) / nominal
    }

    /// What the viewer is expected to receive: bits per frame at the pace.
    pub fn expected_bps(&self, now: Instant) -> u64 {
        (self.bpf * f64::from(self.pace(now))) as u64
    }

    /// The least round trip over the last `RTT_WINDOW`, never below
    /// `DELAY_FLOOR`.
    pub fn base_rtt(&self) -> Option<Duration> {
        self.rtt
            .iter()
            .map(|(_, rtt)| *rtt)
            .min()
            .map(|least| least.max(DELAY_FLOOR))
    }

    /// How much of a queue the floor hides from a round trip: a path whose
    /// least round trip is under `DELAY_FLOOR` shows only the part of a queue
    /// above the difference. The one-way delay hides as much, so a queue
    /// toward the viewer cuts exactly as the round trip's did.
    pub fn floor_gap(&self) -> Duration {
        self.rtt
            .iter()
            .map(|(_, rtt)| *rtt)
            .min()
            .map_or(Duration::ZERO, |least| DELAY_FLOOR.saturating_sub(least))
    }

    pub fn preset(&self) -> QualityPreset {
        self.preset
    }

    pub fn view(&self, now: Instant) -> QosView {
        let targets = self.targets(now);
        QosView {
            fps: self.fps,
            pace_fps: targets.pace_fps,
            viewer_cap: self.viewer_cap,
            paused: self.paused,
            unresponsive: self.unresponsive,
            bpf: self.bpf as u32,
            expected_bps: self.expected_bps(now),
            encoder_bps: targets.encoder_bps,
            rate_model: self.rate_model,
            rtt: self.last_rtt,
            base_rtt: self.base_rtt(),
            delay_ms: self.delay_ms(),
            tainted: self.tainted,
            adaptation: if self.paused {
                Adaptation::Paused
            } else if self.unresponsive {
                Adaptation::Unresponsive
            } else if now < self.startup_until {
                Adaptation::Startup
            } else {
                self.adaptation
            },
            rate_fps: self.rate_fps,
            rebase: self.rebase.map(|rebase| rebase.offset),
        }
    }

    /// The pace before the receipts' cap and the output clamps: the probes'
    /// fps within the ceiling and the viewer's cap.
    fn natural_pace(&self) -> u32 {
        self.fps
            .min(self.ceiling_fps)
            .min(self.viewer_cap.unwrap_or(u32::MAX))
            .min(self.limits.max_fps)
            .max(1)
    }

    /// The pace before the startup, unresponsive and JPEG clamps: the natural
    /// pace within the receipts' cap.
    fn steady_pace(&self) -> u32 {
        self.natural_pace()
            .min(self.rate_fps.unwrap_or(u32::MAX))
            .max(1)
    }

    /// The capture pace. The fps state is never changed by the startup cap,
    /// the viewer cap, the receipts' cap, pause or unresponsiveness: those
    /// clamp only the output.
    fn pace(&self, now: Instant) -> u32 {
        if self.paused {
            return 0;
        }
        let mut pace = self.steady_pace();
        if now < self.startup_until {
            pace = pace.min(self.normal_fps.min((15.0 * self.scale).round() as u32));
        }
        if self.unresponsive {
            pace = pace.min(UNRESPONSIVE_FPS);
        }
        if self.codec == VideoCodec::Jpeg {
            pace = pace.min(JPEG_MAX_FPS);
        }
        pace.max(1)
    }

    /// The median of the recent untainted RTTs above the baseline, or with
    /// arrival times of their queues toward the viewer; of two, the lower.
    /// One stalled probe among them moves it not at all. A rebase raises the
    /// baseline.
    fn delay_ms(&self) -> Option<u32> {
        let base = if self.one_way {
            Duration::ZERO
        } else {
            self.base_rtt()?
        } + self.rebase_offset();
        let mut recent = [Duration::ZERO; DELAY_HISTORY];
        for (slot, rtt) in recent.iter_mut().zip(&self.history) {
            *slot = *rtt;
        }
        let recent = recent.get_mut(..self.history.len())?;
        recent.sort_unstable();
        let median = recent.get(recent.len().checked_sub(1)? / 2)?;
        Some(millis(median.saturating_sub(base)))
    }

    /// RustDesk's `user_network_delay` on the median delay, with its steps
    /// scaled to the ceiling. A cut needs the receipts to witness a standing
    /// queue, or this probe and the one before to have seen one (`raw` is
    /// this probe's own delay); until the probes persist it at most halves
    /// fps. While the receipts own the queue (`owned`), they cut, and fps
    /// holds below `SEVERE_DELAY`. A short path takes the quick step at every
    /// sample.
    fn control_fps(&mut self, delay: u32, raw: u32, witnessed: bool, owned: bool, now: Instant) {
        let small = (self.scale.round() as u32).max(1);
        let quick_step = (5.0 * self.scale).round() as u32;
        let persisted = raw >= CUT_DELAY && self.last_raw.is_some_and(|last| last >= CUT_DELAY);
        let (min_fps, before) = (self.min_fps, self.fps);
        let mut fps = self.fps;
        match delay {
            0..50 => {
                self.quick += 1;
                let mut step = if fps < self.normal_fps { small } else { 0 };
                if self.quick >= 3 || self.short_path() {
                    self.quick = 0;
                    step = quick_step;
                }
                fps = min_fps.max(fps + step);
            }
            50..100 => {
                if self.last_delay.is_some_and(|last| delay < last) && fps < self.normal_fps {
                    fps += small;
                }
            }
            100..CUT_DELAY => fps = min_fps.max(fps),
            // The receipts cut bits per frame, and the pace from the
            // delivery rate, for this queue.
            _ if owned && delay < SEVERE_DELAY => {}
            // Only the probes saw it, and not twice in a row: a stall on
            // their way, not a queue.
            _ if !persisted && !witnessed => {}
            _ => {
                let divided = (fps * 150).div_ceil(delay);
                let target = match delay {
                    150..200 => min_fps.max(divided),
                    200..300 => min_fps.min(divided),
                    300..600 => 150 * min_fps / delay,
                    _ => (150 * min_fps / delay).min(divided),
                };
                fps = if persisted {
                    target
                } else {
                    target.max(fps.div_ceil(2))
                };
            }
        }
        if delay < CUT_DELAY {
            self.increase += 1;
            if self.increase >= 3 {
                self.increase = 0;
                fps += small;
            }
        } else {
            self.increase = 0;
        }
        if delay > 50 {
            self.quick = 0;
        }
        self.fps = fps.clamp(1, self.ceiling_fps);
        self.last_delay = Some(delay);
        self.last_raw = Some(raw);
        self.window.max_delay = Some(self.window.max_delay.map_or(delay, |max| max.max(delay)));
        let adaptation = match self.fps.cmp(&before) {
            std::cmp::Ordering::Greater => Adaptation::Raising,
            std::cmp::Ordering::Less => Adaptation::LoweringFps,
            std::cmp::Ordering::Equal => Adaptation::Steady,
        };
        self.adapt(adaptation, now);
    }

    /// Close the tick's bucket, whatever the rate model; `false` when the
    /// tick was too short to read a pace from, which the next takes in. The
    /// expected bits are integrated tick by tick at the highest per-frame
    /// budget the encoder was given during each, so a cut never compares the
    /// frames encoded before it with the budget after it.
    fn record_tick(&mut self, now: Instant, produced: ProducedSnapshot) -> bool {
        let elapsed = now.saturating_duration_since(self.span.since);
        if elapsed < QOS_TICK / 2 {
            return false;
        }
        let nominal = f64::from(self.limits.max_fps);
        let low = |pace: u32| f64::from(pace) <= LATCH_PACE_RATIO * nominal;
        let (pace, start) = (self.pace(now), self.span.produced);
        let non_idr_frames = produced.non_idr_frames.saturating_sub(start.non_idr_frames);
        let frames = produced.frames.saturating_sub(start.frames);
        let non_idr_share = non_idr_frames as f64 / frames.max(1) as f64;
        self.span.push(TickBucket {
            elapsed,
            frames,
            unique: produced.unique.saturating_sub(start.unique),
            non_idr_frames,
            non_idr_bits: produced
                .non_idr_bytes
                .saturating_sub(start.non_idr_bytes)
                .saturating_mul(8),
            expected_bits: self.span.peak_budget * non_idr_frames as f64,
            rate_bits: elapsed.as_secs_f64()
                * f64::from(self.targets(now).encoder_bps)
                * non_idr_share,
            paced_low: low(self.span.pace) && low(pace),
        });
        self.span.since = now;
        self.span.produced = produced;
        self.span.pace = pace;
        // The next tick starts from the budget now, before the 3 s window's
        // step later in this tick: a raise there lifts it through `set_bpf`,
        // and after a cut it keeps the budget frames still in the encoder were
        // given.
        self.span.peak_budget = self.nominal_budget();
        true
    }

    /// Latch the timestamp model for good when the last `LATCH_SPAN` shows
    /// an encoder that budgets per second: every tick paced at most
    /// `LATCH_PACE_RATIO` × nominal at both its ends, moving content, and
    /// non-IDR bits above `max(LATCH_OVERSHOOT, min(LATCH_PACE_SHARE ×
    /// nominal / mean pace, LATCH_FLOOR_MAX))` × the bits expected for them,
    /// where the mean pace is the frames produced over the span's time.
    fn latch(&mut self) {
        let nominal = f64::from(self.limits.max_fps);
        let span = self.span.total();
        if !span.paced_low || span.unique < DYNAMIC_UNIQUE_FRAMES || span.frames == 0 {
            return;
        }
        // At least half a tick a bucket, so never zero.
        let mean_pace = span.frames as f64 / span.elapsed.as_secs_f64();
        let threshold =
            LATCH_OVERSHOOT.max((LATCH_PACE_SHARE * nominal / mean_pace).min(LATCH_FLOOR_MAX));
        if span.non_idr_bits as f64 > threshold * span.expected_bits {
            self.rate_model = RateModel::Timestamp;
            tracing::info!(
                target: "pb_mapper_desktop::feedback",
                event = "desktop_rate_model_latched",
                frames = span.non_idr_frames,
                bits = span.non_idr_bits,
                expected_bits = span.expected_bits as u64,
                mean_pace_fps = mean_pace,
                threshold,
                bpf = self.bpf as u32,
            );
        }
    }

    /// Close a 3 s window: the bits-per-frame step, unless receipts came in
    /// it, which then own bits per frame. A JPEG session, and a window in
    /// which a still picture or a pause sent nothing, keep the step.
    fn close_window(&mut self, now: Instant, produced: ProducedSnapshot) {
        let start = self.window.produced;
        let dynamic = produced.unique.saturating_sub(start.unique) >= DYNAMIC_UNIQUE_FRAMES;
        let receipted =
            self.codec != VideoCodec::Jpeg && self.receipts.sampled_since(self.window.started);
        match (receipted, self.window.max_delay, self.window.max_tainted) {
            (true, _, _) => {}
            (false, Some(max_delay), _) => self.step_bpf(max_delay, dynamic, now),
            // Only probes behind IDRs: their delay is mostly the IDRs' own
            // transfer. It may lower bits per frame, which shrinks the IDRs,
            // but never raise them; otherwise a link where every IDR takes
            // seconds would freeze the controller.
            (false, None, Some(tainted)) => self.step_bpf(tainted, false, now),
            // No sample in the window: hold.
            (false, None, None) => {}
        }
        self.window = RatioWindow::new(now, produced);
    }

    /// One bits-per-frame step from the window's worst delay. Rises need moving
    /// content and are capped at +150 kbit/s.
    fn step_bpf(&mut self, max_delay: u32, dynamic: bool, now: Instant) {
        let factor = match max_delay {
            0..50 => dynamic.then_some(1.15),
            50..100 => dynamic.then_some(1.10),
            100..150 => dynamic.then_some(1.05),
            150..200 => Some(0.95),
            200..300 => Some(0.90),
            300..500 => Some(0.85),
            _ => Some(0.80),
        };
        let Some(factor) = factor else {
            return;
        };
        let mut bpf = self.bpf * factor;
        if factor > 1.0 {
            bpf = bpf.min(self.bpf + BPF_RAISE_MAX_BPS / f64::from(self.pace(now).max(1)));
        }
        let bpf = self.clamp_bpf(bpf);
        let adaptation = if bpf > self.bpf {
            Adaptation::Raising
        } else if bpf < self.bpf {
            Adaptation::LoweringBitrate
        } else {
            Adaptation::Steady
        };
        self.adapt(adaptation, now);
        self.set_bpf(bpf);
    }

    fn clamp_bpf(&self, bpf: f64) -> f64 {
        let p = preset(self.preset);
        bpf.clamp(p.bpf_floor * self.pixel_scale, p.bpf_max * self.pixel_scale)
    }

    /// Every change of bits per frame goes through here, so the latch knows
    /// the highest budget the encoder was given during each tick.
    fn set_bpf(&mut self, bpf: f64) {
        self.bpf = bpf;
        self.span.peak_budget = self.span.peak_budget.max(self.nominal_budget());
    }
}

/// The encoder bitrate for `bps`, within its bounds.
fn encoder_bitrate(bps: f64) -> u32 {
    (bps as u32).clamp(ENCODER_BITRATE_MIN, ENCODER_BITRATE_MAX)
}

fn millis(duration: Duration) -> u32 {
    duration.as_millis().min(u128::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_fps: u32) -> VideoLimits {
        VideoLimits {
            max_width: 1920,
            max_height: 1080,
            max_fps,
        }
    }

    /// An H.264 session on Balanced at 1080p60, its preset's own ceiling.
    fn balanced(now: Instant) -> Qos {
        Qos::new(
            limits(60),
            VideoCodec::H264,
            QualityPreset::Balanced,
            false,
            RateModel::Nominal,
            None,
            now,
        )
    }

    /// One probe round trip of `rtt_ms`, sent at `at`, with the receipts'
    /// queue at its echo.
    fn sample_with(qos: &mut Qos, at: Instant, rtt_ms: u64, tainted: bool, ack_queue_ms: u64) {
        let nonce = qos.probe_due(at).unwrap_or(qos.next_nonce);
        qos.probe_sent(nonce, at, tainted);
        qos.on_probe_echo(
            nonce,
            at + Duration::from_millis(rtt_ms),
            None,
            Some(Duration::from_millis(ack_queue_ms)),
        );
    }

    /// `(rtt_ms, forward_ms, ack_queue_ms)` probes every 250 ms from `t0`,
    /// from a viewer that stamps arrival times: each probe's queue toward
    /// it, and the receipts'; fps after each.
    fn one_way_probes(qos: &mut Qos, t0: Instant, script: &[(u64, u64, u64)]) -> Vec<u32> {
        let mut at = t0;
        script
            .iter()
            .map(|&(rtt, forward, queue)| {
                let nonce = qos.probe_due(at).unwrap_or(qos.next_nonce);
                qos.probe_sent(nonce, at, false);
                qos.on_probe_echo(
                    nonce,
                    at + Duration::from_millis(rtt),
                    Some(Duration::from_millis(forward)),
                    Some(Duration::from_millis(queue)),
                );
                at += FAST_PROBE_INTERVAL;
                qos.fps
            })
            .collect()
    }

    /// One probe round trip of `rtt_ms`, sent at `at`, with no receipt queue.
    fn sample(qos: &mut Qos, at: Instant, rtt_ms: u64, tainted: bool) {
        sample_with(qos, at, rtt_ms, tainted, 0);
    }

    /// `(rtt_ms, ack_queue_ms)` probes every 250 ms from `t0`; fps after each.
    fn probes(qos: &mut Qos, t0: Instant, script: &[(u64, u64)]) -> Vec<u32> {
        let mut at = t0;
        script
            .iter()
            .map(|&(rtt, queue)| {
                sample_with(qos, at, rtt, false, queue);
                at += FAST_PROBE_INTERVAL;
                qos.fps
            })
            .collect()
    }

    /// A 20 to 40 ms link with no queue, for `count` probes.
    fn calm(count: usize) -> impl Iterator<Item = (u64, u64)> {
        (0..count as u64).map(|n| (20 + 5 * (n % 5), 0))
    }

    fn idle() -> CreditView {
        CreditView {
            outstanding_frames: 0,
            outstanding_bytes: 0,
            idr_inflight: None,
            owed_since: None,
        }
    }

    /// How a test encoder sizes its frames.
    #[derive(Clone, Copy, Debug)]
    enum Budget {
        /// The per-frame budget it is given when the frame is encoded: the
        /// encoder bitrate over the nominal rate, as `slow_link`'s `Budget`.
        PerFrame,
        /// The encoder bitrate over the frames of a second, of which its
        /// pictures fill this share.
        PerSecond(f64),
        /// These bits whatever the bitrate: an encoder at its content floor.
        Fixed(f64),
    }
    /// An encoder that budgets per second and fills its budget.
    const PER_SECOND: Budget = Budget::PerSecond(1.0);
    impl Budget {
        /// The bits of each frame encoded now, at `rate` frames a second.
        fn bits(self, qos: &Qos, now: Instant, rate: f64) -> f64 {
            let encoder_bps = f64::from(qos.targets(now).encoder_bps);
            match self {
                Budget::PerFrame => encoder_bps / f64::from(qos.limits.max_fps),
                Budget::PerSecond(share) => share * encoder_bps / rate,
                Budget::Fixed(bits) => bits,
            }
        }
    }

    /// An encoder fed a QoS tick at a time: each tick it makes the frames of
    /// the published pace, or of `rate` when set, every one unique and none an
    /// IDR, sized by its budget at the targets then, and the controller then
    /// ticks.
    struct Ticks {
        now: Instant,
        produced: ProducedSnapshot,
        /// The fraction of a frame the next tick owes.
        owed: f64,
        /// Frames a second, when not the pace: pictures that change less often.
        rate: Option<f64>,
    }
    impl Ticks {
        fn new(now: Instant) -> Self {
            Self {
                now,
                produced: ProducedSnapshot::default(),
                owed: 0.0,
                rate: None,
            }
        }

        /// `frames` frames of `bits` each, `unique` of them unique, none an
        /// IDR, within the tick under way.
        fn produce(&mut self, frames: u64, unique: u64, bits: f64) {
            let produced = &mut self.produced;
            produced.frames += frames;
            produced.unique += unique;
            produced.non_idr_frames += frames;
            produced.non_idr_bytes += (frames as f64 * bits / 8.0) as u64;
        }

        /// The controller's tick, a QoS tick after the last.
        fn advance(&mut self, qos: &mut Qos) {
            self.now += QOS_TICK;
            qos.tick(self.now, &idle(), self.produced);
        }

        fn tick(&mut self, qos: &mut Qos, budget: Budget) {
            let rate = self
                .rate
                .unwrap_or_else(|| f64::from(qos.targets(self.now).pace_fps));
            let due = rate * QOS_TICK.as_secs_f64() + self.owed;
            let frames = due.floor();
            self.owed = due - frames;
            let bits = budget.bits(qos, self.now, rate);
            self.produce(frames as u64, frames as u64, bits);
            self.advance(qos);
        }

        fn run(&mut self, qos: &mut Qos, budget: Budget, ticks: usize) {
            for _ in 0..ticks {
                self.tick(qos, budget);
            }
        }

        /// When the session latched the timestamp model within `ticks`.
        fn latched(&mut self, qos: &mut Qos, budget: Budget, ticks: usize) -> Option<Instant> {
            (0..ticks).find_map(|_| {
                self.tick(qos, budget);
                (qos.rate_model == RateModel::Timestamp).then_some(self.now)
            })
        }
    }

    #[test]
    fn balanced_paces_at_its_ceiling_after_its_first_probe_on_any_path() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut qos = balanced(t0);
        // Balanced at 60: 45 fps, capped at 15 × 2 for the first second.
        assert_eq!((qos.fps, qos.pace(t0)), (45, 30));
        // A flat 20 ms link: the first echo lifts fps to the ceiling, which
        // the startup cap still holds back for its second.
        let fps = probes(&mut qos, t0, &[(20, 0); 8]);
        assert!(fps.iter().all(|fps| *fps == 60), "{fps:?}");
        assert_eq!(qos.pace(at(999)), 30);
        assert_eq!(qos.pace(at(1000)), 60);
        // Such a path is probed every 250 ms.
        assert_eq!(qos.probe_due_at(at(1800)), Some(at(2000)));

        // A long path starts at the ceiling too, since the receipts cut
        // within a round trip if it cannot carry it; it is probed once a
        // second.
        let mut far = balanced(t0);
        sample(&mut far, t0, 350, false);
        assert_eq!(far.fps, 60);
        assert_eq!(far.probe_due_at(at(500)), Some(at(1000)));
        // A JPEG session never starts high: it paces at 8 at most anyway.
        let mut jpeg = Qos::new(
            limits(60),
            VideoCodec::Jpeg,
            QualityPreset::Balanced,
            false,
            RateModel::Nominal,
            None,
            t0,
        );
        sample(&mut jpeg, t0, 20, false);
        assert!(jpeg.fps < 60, "{}", jpeg.fps);
    }

    #[test]
    fn a_stalled_probe_never_more_than_halves_fps() {
        let t0 = Instant::now();
        let mut qos = balanced(t0);
        // A 600 ms stall that delays the receipts as well as the probe ...
        let stall = (600, 580);
        let mut script: Vec<_> = calm(8).collect();
        let single = script.len();
        script.push(stall);
        script.extend(calm(4));
        // ... and two of them one probe apart.
        let pair = script.len();
        script.extend([stall, (30, 0), stall]);
        let after = script.len();
        script.extend(calm(12));
        let fps = probes(&mut qos, t0, &script);

        assert_eq!(fps[single - 1], 60);
        // One moves nothing, though the receipts queued behind it.
        assert!(fps[single..pair].iter().all(|fps| *fps == 60), "{fps:?}");
        // Two move it, by half at most ...
        assert!(fps[pair..after].contains(&30), "{fps:?}");
        assert!(fps[pair..].iter().all(|fps| *fps >= 30), "{fps:?}");
        // ... and fps is back at the ceiling within 3 s.
        let back = fps[after..].iter().position(|fps| *fps == 60).unwrap();
        assert!(
            FAST_PROBE_INTERVAL * back as u32 <= Duration::from_secs(3),
            "{fps:?}"
        );
    }

    #[test]
    fn a_sustained_queue_reaches_the_floor_within_two_samples() {
        let t0 = Instant::now();
        let mut qos = balanced(t0);
        let fps = probes(&mut qos, t0, &[(20, 0); 8]);
        assert_eq!(fps.last(), Some(&60));
        // 250 ms of queueing that no receipt reports: two probes in a row
        // are evidence enough, and the second is at Balanced's floor ...
        let start = t0 + FAST_PROBE_INTERVAL * 8;
        let fps = probes(&mut qos, start, &[(270, 0), (270, 0)]);
        assert_eq!(fps, [60, 10]);
        // ... and 600 ms takes it below, as RustDesk's steps do.
        let fps = probes(
            &mut qos,
            start + FAST_PROBE_INTERVAL * 2,
            &[(620, 0), (620, 0)],
        );
        assert_eq!(fps, [6, 2]);
    }

    #[test]
    fn tainted_probes_unresponsiveness_baseline_and_output_clamps() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut qos = balanced(t0);
        sample(&mut qos, s(1), 20, false);
        qos.fps = 40;

        // Tainted samples move the RTT window, never the controller.
        let history = qos.history.clone();
        sample(&mut qos, s(8), 900, true);
        assert_eq!((qos.fps, qos.history.clone()), (40, history));
        assert!(qos.view(s(8)).tainted);

        // Unresponsive: 2 fps output, the fps state untouched, and recovery
        // on the next receipt.
        let nonce = qos.next_nonce;
        qos.probe_sent(nonce, s(9), false);
        qos.tick(
            s(9) + Duration::from_millis(2100),
            &idle(),
            ProducedSnapshot::default(),
        );
        assert_eq!((qos.pace(s(12)), qos.fps), (UNRESPONSIVE_FPS, 40));
        qos.on_receipt(None, false, s(12));
        assert_eq!(qos.pace(s(12)), 40);
        // A late echo of the outstanding probe recovers it too, but its stale
        // RTT is not a sample.
        qos.tick(s(13), &idle(), ProducedSnapshot::default());
        assert!(qos.unresponsive);
        let samples = qos.rtt.len();
        qos.on_probe_echo(nonce, s(13), None, None);
        assert!(!qos.unresponsive && qos.rtt.len() == samples);
        // Receipts owed for 2 s count too; a receipt owed briefly does not.
        let owed = |since| CreditView {
            outstanding_frames: 1,
            owed_since: Some(since),
            ..idle()
        };
        qos.tick(s(14), &owed(s(13)), ProducedSnapshot::default());
        assert!(!qos.unresponsive);
        qos.tick(s(14), &owed(s(11)), ProducedSnapshot::default());
        assert!(qos.unresponsive);
        // A nonce that is not outstanding is ignored.
        qos.on_probe_echo(99, s(14), None, None);
        assert!(qos.unresponsive);

        // The baseline is the minimum over the last 60 s, and a new path
        // resets it.
        let mut qos = balanced(t0);
        sample(&mut qos, s(1), 5, false);
        assert_eq!(qos.base_rtt(), Some(DELAY_FLOOR));
        for n in 0..RTT_WINDOW.as_secs() {
            sample(&mut qos, s(2 + n), 40, false);
        }
        assert_eq!(qos.base_rtt(), Some(Duration::from_millis(40)));
        assert_eq!(qos.delay_ms(), Some(0));
        qos.reset_baseline();
        assert_eq!((qos.base_rtt(), qos.delay_ms()), (None, None));
        sample(&mut qos, s(70), 90, false);
        assert_eq!(
            (qos.base_rtt(), qos.delay_ms()),
            (Some(Duration::from_millis(90)), Some(0))
        );

        // The viewer cap and pause clamp the pace; resume restarts the
        // startup cap.
        qos.set_viewer_cap(24);
        assert_eq!(qos.pace(s(71)), 24);
        qos.set_viewer_cap(60);
        assert_eq!(qos.viewer_cap, None);
        qos.set_paused();
        assert_eq!(qos.targets(s(71)).pace_fps, 0);
        assert!(qos.targets(s(71)).paused);
        qos.resume(s(72));
        assert_eq!(qos.pace(s(72)), 30);
        assert_eq!(qos.pace(s(73)), qos.fps);

        // A JPEG session never paces above 8 fps.
        let jpeg = Qos::new(
            limits(60),
            VideoCodec::Jpeg,
            QualityPreset::Smooth,
            false,
            RateModel::Nominal,
            None,
            t0,
        );
        assert_eq!(jpeg.pace(s(2)), JPEG_MAX_FPS);
    }

    /// A receipt for each of `count` units of `bytes`, every `every_ms` from
    /// `from_ms`, each delayed `delay_ms` from its write: what a host times.
    fn receipts(
        qos: &mut Qos,
        t0: Instant,
        from_ms: u64,
        count: u64,
        every_ms: u64,
        delay_ms: u64,
    ) {
        for n in 0..count {
            let receipt = Receipt {
                delay: Duration::from_millis(delay_ms),
                bytes: 4250,
                behind_idr: false,
                arrival: None,
            };
            qos.on_receipt(
                Some(receipt),
                false,
                t0 + Duration::from_millis(from_ms + every_ms * n),
            );
        }
    }

    #[test]
    fn receipts_cut_bits_per_frame_within_a_round_trip_and_keep_the_pace() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut qos = balanced(t0);
        let fps = probes(&mut qos, t0, &[(20, 0); 8]);
        assert_eq!(fps.last(), Some(&60));
        assert_eq!(qos.targets(at(2000)).encoder_bps, 4_000_020);
        // 2 Mbit/s with 100 ms of queue above a 20 ms base: the first
        // receipts that show the busy rate cut to 85% of it, 1.7 Mbit/s, in
        // bits per frame at the pace of 60 ...
        receipts(&mut qos, t0, 2000, 10, 17, 120);
        let cut = 1_700_000.0 / 60.0;
        assert!((qos.bpf - cut).abs() < 1.0, "{}", qos.bpf);
        assert_eq!((qos.fps, qos.pace(at(2200)), qos.rate_fps), (60, 60, None));
        assert_eq!(qos.targets(at(2200)).encoder_bps, (cut * 60.0) as u32);
        assert_eq!(qos.adaptation, Adaptation::LoweringBitrate);
        let view = qos.receipt_view(at(2153));
        assert_eq!((view.cuts, view.episode), (1, true));
        // ... the byte window follows the busy rate while the episode lasts,
        // and the probe controller yields its fps cuts to the receipts.
        assert_eq!(qos.clamp_rate(at(2153)), Some(2_000_000));
        assert!(qos.receipts.owns(at(2153)));
        // The cut holds while the link still delivers what came before it:
        // nothing more in the next second's receipts at that rate.
        receipts(&mut qos, t0, 2170, 50, 17, 120);
        assert_eq!(qos.receipt_view(at(3000)).cuts, 1);
        // A JPEG session's receipts cut nothing.
        let mut jpeg = Qos::new(
            limits(60),
            VideoCodec::Jpeg,
            QualityPreset::Balanced,
            false,
            RateModel::Nominal,
            None,
            t0,
        );
        probes(&mut jpeg, t0, &[(20, 0); 8]);
        let before = jpeg.bpf;
        receipts(&mut jpeg, t0, 2000, 60, 17, 120);
        assert_eq!(
            (
                jpeg.bpf,
                jpeg.receipt_view(at(3000)).cuts,
                jpeg.clamp_rate(at(3000))
            ),
            (before, 0, None)
        );
        // A path change forgets the receipts, but not the session's cuts.
        qos.reset_baseline();
        assert_eq!(
            qos.receipt_view(at(3000)),
            ReceiptRateView {
                cuts: 1,
                ..ReceiptRateView::default()
            }
        );
    }

    /// A receipt raise to `to` bit/s, of bits per frame too.
    fn lift(to: f64) -> Lift {
        Lift {
            to,
            pace_only: false,
        }
    }

    /// Balanced on a 20 ms path at its ceiling of 60, past the startup cap.
    fn settled(t0: Instant) -> Qos {
        let mut qos = balanced(t0);
        probes(&mut qos, t0, &[(20, 0); 8]);
        qos
    }

    #[test]
    fn a_cut_below_the_floor_caps_the_pace_from_the_delivery_rate_and_raises_lift_it_first() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut qos = settled(t0);
        // 0.75 Mbit/s at 60: below Balanced's floor of 16 667 bits a frame,
        // which carries 45 FPS of it.
        qos.cut_to(45.0 * 16_667.0, at(2000));
        assert_eq!((qos.bpf, qos.rate_fps), (16_667.0, Some(45)));
        assert_eq!((qos.pace(at(2000)), qos.fps), (45, 60));
        assert_eq!(qos.adaptation, Adaptation::LoweringFps);
        // The encoder keeps its per-frame budget at the nominal rate.
        assert_eq!(qos.targets(at(2000)).encoder_bps, 1_000_020);
        // Below the preset's minimum the pace stays at it, and the probe
        // controller's RustDesk steps take over from there.
        qos.cut_to(100_000.0, at(2100));
        assert_eq!((qos.bpf, qos.rate_fps), (16_667.0, Some(10)));
        // A cut never raises the pace, nor bits per frame.
        qos.cut_to(900_000.0, at(2200));
        assert_eq!((qos.bpf, qos.rate_fps), (16_667.0, Some(10)));
        // Raises lift the pace first: 20% of 10 FPS is 12 ...
        qos.raise_to(lift(1.2 * qos.bpf * 10.0), qos.bpf * 10.0, at(2300));
        assert_eq!((qos.bpf, qos.rate_fps), (16_667.0, Some(12)));
        assert_eq!(qos.adaptation, Adaptation::Raising);
        // ... until it reaches the probes' fps, which clears the cap ...
        qos.raise_to(lift(6.0 * 12.0 * qos.bpf), 12.0 * qos.bpf, at(2400));
        assert_eq!(qos.rate_fps, None);
        assert_eq!(qos.bpf, 16_667.0);
        // ... and only then bits per frame, up to the preset's maximum.
        qos.raise_to(lift(2.0 * 60.0 * qos.bpf), 60.0 * qos.bpf, at(2500));
        assert!((qos.bpf - 33_334.0).abs() < 1.0, "{}", qos.bpf);
        qos.raise_to(lift(1e9), 60.0 * qos.bpf, at(2600));
        assert_eq!(qos.bpf, 133_333.0);
        // A new preset starts again without the cap.
        qos.cut_to(750_000.0, at(2700));
        qos.set_preset(QualityPreset::Smooth, at(2800), ProducedSnapshot::default());
        assert_eq!(qos.rate_fps, None);
    }

    #[test]
    fn quality_cuts_bits_per_frame_to_half_its_start_then_the_pace_then_its_floor() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut qos = Qos::new(
            limits(60),
            VideoCodec::H264,
            QualityPreset::Quality,
            false,
            RateModel::Nominal,
            None,
            t0,
        );
        probes(&mut qos, t0, &[(20, 0); 8]);
        // Quality at 30: 200 000 bits a frame, 6 Mbit/s.
        assert_eq!((qos.steady_pace(), qos.bpf), (30, 200_000.0));
        qos.cut_to(4_500_000.0, at(2000));
        assert_eq!((qos.bpf, qos.rate_fps), (150_000.0, None));
        // Half its start carries 20 FPS of 2 Mbit/s.
        qos.cut_to(2_000_000.0, at(2100));
        assert_eq!((qos.bpf, qos.rate_fps), (100_000.0, Some(20)));
        // Under its minimum of 8 FPS, bits per frame fall to the floor.
        qos.cut_to(600_000.0, at(2200));
        assert_eq!((qos.bpf, qos.rate_fps), (75_000.0, Some(8)));
        qos.cut_to(100_000.0, at(2300));
        assert_eq!((qos.bpf, qos.rate_fps), (50_000.0, Some(8)));
        // Raises take the same steps back: bits per frame to half the start
        // first, at the minimum pace ...
        qos.raise_to(lift(1e9), 8.0 * qos.bpf, at(2400));
        assert_eq!((qos.bpf, qos.rate_fps), (100_000.0, Some(8)));
        // ... then the pace.
        qos.raise_to(lift(2.0 * 8.0 * qos.bpf), 8.0 * qos.bpf, at(2500));
        assert_eq!((qos.bpf, qos.rate_fps), (100_000.0, Some(16)));
    }

    #[test]
    fn the_probes_yield_fps_to_the_receipts_below_600_ms_while_they_own_the_queue() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // Owned: two probes at 270 ms hold the frame rate ...
        let mut qos = settled(t0);
        qos.control_fps(270, 270, true, true, at(2000));
        qos.control_fps(270, 270, true, true, at(2250));
        assert_eq!(qos.fps, 60);
        // ... but at 600 ms or more the RustDesk steps cut whoever owns it.
        qos.control_fps(620, 620, true, true, at(2500));
        assert!(qos.fps < 60, "{}", qos.fps);
        // Not owned: the same two probes cut, as before.
        let mut qos = settled(t0);
        qos.control_fps(270, 270, true, false, at(2000));
        qos.control_fps(270, 270, true, false, at(2250));
        assert_eq!(qos.fps, 10);
        // A receipt cut owns the queue for 2 s, while receipts come.
        let mut qos = settled(t0);
        receipts(&mut qos, t0, 2000, 10, 17, 120);
        assert!(qos.receipts.owns(at(2170)));
        let fps = probes(&mut qos, at(2170), &[(270, 300), (270, 300)]);
        assert_eq!(fps, [60, 60]);
    }

    #[test]
    fn a_rebase_moves_the_probes_baseline_until_their_own_catches_up() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut qos = settled(t0);
        assert_eq!(qos.base_rtt(), Some(Duration::from_millis(20)));
        // The receipts rebased onto 140 ms: a route 120 ms longer.
        qos.rebase_to(Duration::from_millis(140));
        assert_eq!(qos.rebase_offset(), Duration::from_millis(120));
        // Its probes read no delay, so the frame rate climbs and holds.
        let fps = probes(&mut qos, at(2000), &[(140, 0); 4]);
        assert_eq!(fps, [60; 4]);
        assert_eq!(qos.delay_ms(), Some(0));
        assert_eq!(qos.view(at(3000)).rebase, Some(Duration::from_millis(120)));
        // A queue above the new baseline still shows.
        probes(&mut qos, at(3000), &[(240, 0); 3]);
        assert_eq!(qos.delay_ms(), Some(100));
        // A rebase is never more than `CUT_DELAY`.
        qos.rebase_to(Duration::from_millis(400));
        assert_eq!(qos.rebase_offset(), REBASE_MAX);
        // Probes back at the old base: the longer route is gone.
        probes(&mut qos, at(4000), &[(20, 0)]);
        assert!(qos.rebase.is_none());
        // The probes' own base rising to the rebased level ends it too.
        qos.rebase_to(Duration::from_millis(140));
        qos.rtt.clear();
        probes(&mut qos, at(5000), &[(140, 0)]);
        assert!(qos.rebase.is_none());
        // A path change, a resume and a new preset each end it.
        for reset in [0, 1, 2] {
            let mut qos = settled(t0);
            qos.rebase_to(Duration::from_millis(140));
            match reset {
                0 => qos.reset_baseline(),
                1 => qos.resume(at(3000)),
                _ => qos.set_preset(
                    QualityPreset::Balanced,
                    at(3000),
                    ProducedSnapshot::default(),
                ),
            }
            assert!(qos.rebase.is_none(), "{reset}");
        }
    }

    #[test]
    fn the_3s_window_leaves_bits_per_frame_to_the_receipts_while_they_come() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let produced = |unique: u64| ProducedSnapshot {
            frames: unique,
            unique,
            ..ProducedSnapshot::default()
        };
        // A window with 250 ms of probe delay and receipts in it: no step.
        let mut qos = settled(t0);
        let before = qos.bpf;
        qos.control_fps(250, 250, true, false, s(2));
        receipts(&mut qos, t0, 2100, 3, 17, 22);
        qos.tick(s(3), &idle(), produced(100));
        assert_eq!(qos.bpf, before);
        // The next window has none: the step comes as before.
        qos.control_fps(250, 250, true, false, s(5));
        qos.tick(s(6), &idle(), produced(200));
        assert!((qos.bpf - 0.9 * before).abs() < 1.0, "{}", qos.bpf);
    }

    /// Balanced settled at the full pace and its floor of 16 667 bits a
    /// frame, after three ticks in which the encoder made frames of `bits`
    /// each: the first, which began at the budget before the cut, and two at
    /// the floor.
    fn at_the_floor(t0: Instant, bits: [f64; 3]) -> Qos {
        let mut qos = settled(t0);
        let now = t0 + Duration::from_millis(2000);
        qos.cut_to(60.0 * 16_667.0, now);
        assert_eq!((qos.bpf, qos.rate_fps), (16_667.0, None));
        let mut ticks = Ticks::new(now);
        for bits in bits {
            ticks.produce(15, 15, bits);
            ticks.advance(&mut qos);
        }
        qos
    }

    #[test]
    fn a_cut_sizes_the_pace_by_what_the_host_measured_its_frames_cost() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(3);
        // An encoder that follows its budget: the pace carries the cut at
        // the floor, as without a measure.
        let mut qos = at_the_floor(t0, [16_667.0; 3]);
        qos.cut_to(30.0 * 16_667.0, now);
        assert_eq!(qos.rate_fps, Some(30));
        // One at its content floor, 1.8 × the budget in each tick: a lower
        // budget sheds nothing, so the pace carries what its frames cost, and
        // the receipts are told what it sends.
        let mut qos = at_the_floor(t0, [30_000.0; 3]);
        let inputs = qos.receipt_inputs(false, now);
        assert_eq!(inputs.sent_bps, 60.0 * 30_000.0);
        assert_eq!(inputs.floor_bps, 10.0 * 30_000.0);
        qos.cut_to(1_000_000.0, now);
        assert_eq!((qos.bpf, qos.rate_fps), (16_667.0, Some(33)));
        // Such a measure at most halves the pace a cut: frames three times
        // their budget would carry 20 FPS of 1 Mbit/s.
        let mut qos = at_the_floor(t0, [50_000.0; 3]);
        qos.cut_to(1_000_000.0, now);
        assert_eq!(qos.rate_fps, Some(30));
        // A VBV whose excess falls toward its budget is still following it:
        // its frames are taken at the budget.
        let mut qos = at_the_floor(t0, [60_000.0, 50_000.0, 33_000.0]);
        assert_eq!(qos.receipt_inputs(false, now).sent_bps, 60.0 * 16_667.0);
        qos.cut_to(30.0 * 16_667.0, now);
        assert_eq!(qos.rate_fps, Some(30));
        // A light picture's frames cost far less than the budget: a cut of
        // 15% of what it sends takes 15% of its pace, not most of it.
        let mut qos = at_the_floor(t0, [2400.0; 3]);
        assert_eq!(qos.receipt_inputs(false, now).sent_bps, 60.0 * 2400.0);
        qos.cut_to(0.85 * 60.0 * 2400.0, now);
        assert_eq!(qos.rate_fps, Some(51));
        // Frames from before a cut, above the budget now, are no measure.
        let mut qos = settled(t0);
        let mut ticks = Ticks::new(t0 + Duration::from_millis(2000));
        for _ in 0..2 {
            ticks.produce(15, 15, 66_667.0);
            ticks.advance(&mut qos);
        }
        qos.cut_to(60.0 * 16_667.0, ticks.now);
        assert_eq!(qos.spending(), None);
        qos.cut_to(30.0 * 16_667.0, ticks.now);
        assert_eq!(qos.rate_fps, Some(30));
    }

    #[test]
    fn a_light_pictures_raise_lifts_only_the_pace_cap() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let light = |to: f64| Lift {
            to,
            pace_only: true,
        };
        let mut qos = settled(t0);
        qos.cut_to(30.0 * 16_667.0, at(2000));
        assert_eq!((qos.bpf, qos.rate_fps), (16_667.0, Some(30)));
        qos.raise_to(light(2.0 * 30.0 * qos.bpf), 30.0 * qos.bpf, at(2100));
        assert_eq!((qos.bpf, qos.rate_fps), (16_667.0, None));
        qos.raise_to(light(1e9), 60.0 * qos.bpf, at(2200));
        assert_eq!(qos.bpf, 16_667.0);
        // Quality below its cut floor lifts the cap before bits per frame.
        let mut qos = Qos::new(
            limits(60),
            VideoCodec::H264,
            QualityPreset::Quality,
            false,
            RateModel::Nominal,
            None,
            t0,
        );
        probes(&mut qos, t0, &[(20, 0); 8]);
        qos.cut_to(100_000.0, at(2000));
        assert_eq!((qos.bpf, qos.rate_fps), (50_000.0, Some(8)));
        qos.raise_to(light(2.0 * 8.0 * qos.bpf), 8.0 * qos.bpf, at(2100));
        assert_eq!((qos.bpf, qos.rate_fps), (50_000.0, Some(16)));
    }

    #[test]
    fn a_rebase_counts_a_generation_only_when_its_offset_changes() {
        let t0 = Instant::now();
        let mut qos = settled(t0);
        assert_eq!(qos.rebase_state(), (0, Duration::ZERO));
        qos.rebase_to(Duration::from_millis(140));
        assert_eq!(qos.rebase_state(), (1, Duration::from_millis(120)));
        qos.rebase_to(Duration::from_millis(140));
        assert_eq!(qos.rebase_state().0, 1);
        qos.reset_baseline();
        assert_eq!(qos.rebase_state(), (2, Duration::ZERO));
        qos.reset_baseline();
        assert_eq!(qos.rebase_state().0, 2);
    }

    #[test]
    fn a_change_is_reported_for_a_second_before_a_probe_reports_steady() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut qos = settled(t0);
        qos.cut_to(2_000_000.0, at(2000));
        assert_eq!(qos.adaptation, Adaptation::LoweringBitrate);
        // A probe that changes nothing does not hide the cut ...
        qos.control_fps(60, 60, false, false, at(2500));
        assert_eq!(qos.view(at(2500)).adaptation, Adaptation::LoweringBitrate);
        // ... until it has been reported for a second.
        qos.control_fps(60, 60, false, false, at(3001));
        assert_eq!(qos.view(at(3001)).adaptation, Adaptation::Steady);
    }
    /// A round trip under `DELAY_FLOOR` counts as the floor, which hides
    /// the first part of any queue on such a path; the one-way delay hides
    /// as much, so a queue toward the viewer moves the controller alike.
    #[test]
    fn a_path_under_the_floor_hides_as_much_of_a_queue_one_way() {
        let t0 = Instant::now();
        let mut rtt = balanced(t0);
        let mut one_way = balanced(t0);
        one_way.use_one_way();
        // A 2 ms path, then 154 ms queued toward the viewer, twice.
        probes(&mut rtt, t0, &[(2, 0), (2, 0), (156, 0), (156, 0)]);
        one_way_probes(
            &mut one_way,
            t0,
            &[(2, 0, 0), (2, 0, 0), (156, 154, 0), (156, 154, 0)],
        );
        assert_eq!(rtt.floor_gap(), Duration::from_millis(8));
        assert_eq!(rtt.delay_ms(), Some(146));
        assert_eq!(one_way.delay_ms(), rtt.delay_ms());
        assert_eq!(one_way.fps, rtt.fps);
        assert_eq!(rtt.base_rtt(), Some(DELAY_FLOOR));
    }

    /// With arrival times, a queue on the viewer's uplink alone (the round
    /// trip 300 ms longer, nothing queued toward the viewer) never cuts, and
    /// one toward it cuts as a round trip's did.
    #[test]
    fn a_reverse_only_delay_never_cuts_and_a_forward_one_does() {
        let t0 = Instant::now();
        let mut qos = balanced(t0);
        qos.use_one_way();
        let fps = one_way_probes(&mut qos, t0, &[(20, 0, 0); 8]);
        assert_eq!(fps.last(), Some(&60));
        let start = t0 + FAST_PROBE_INTERVAL * 8;
        let fps = one_way_probes(&mut qos, start, &[(330, 2, 0); 12]);
        assert!(fps.iter().all(|fps| *fps == 60), "{fps:?}");
        assert_eq!(qos.delay_ms(), Some(2));
        assert_eq!(qos.base_rtt(), Some(Duration::from_millis(20)));
        assert_eq!(qos.last_rtt, Some(Duration::from_millis(330)));
        // 250 ms toward the viewer, twice: Balanced's floor, as the round
        // trip took it there (`a_sustained_queue_reaches_the_floor_...`).
        let start = start + FAST_PROBE_INTERVAL * 12;
        let fps = one_way_probes(&mut qos, start, &[(580, 250, 0), (580, 250, 0)]);
        assert_eq!(fps, [60, 10]);
        // A stamp that was no sample moves nothing, but the echo counts.
        let at = start + FAST_PROBE_INTERVAL * 2;
        let nonce = qos.next_nonce;
        qos.probe_sent(nonce, at, false);
        qos.on_probe_echo(nonce, at + Duration::from_millis(20), None, None);
        assert_eq!((qos.fps, qos.delay_ms()), (10, Some(250)));
        assert!(qos.outstanding.is_none());

        // A queue on the uplink past `IO_TIMEOUT` makes every round trip too
        // long to sample, while the way toward the viewer is still timed: a
        // queue there still cuts, and the baseline stays as it was.
        let mut qos = balanced(t0);
        qos.use_one_way();
        one_way_probes(&mut qos, t0, &[(20, 0, 0); 8]);
        let samples = qos.rtt.len();
        let start = t0 + FAST_PROBE_INTERVAL * 8;
        let fps = one_way_probes(&mut qos, start, &[(3_520, 2, 0); 4]);
        assert!(fps.iter().all(|fps| *fps == 60), "{fps:?}");
        let start = start + FAST_PROBE_INTERVAL * 4;
        let fps = one_way_probes(&mut qos, start, &[(3_770, 250, 0), (3_770, 250, 0)]);
        assert_eq!(fps, [60, 10]);
        assert_eq!(qos.rtt.len(), samples);
        assert_eq!(qos.last_rtt, Some(Duration::from_millis(20)));
        // Timed by round trips alone, those echoes are no samples at all.
        let mut rtt = balanced(t0);
        probes(&mut rtt, t0, &[(20, 0); 8]);
        let fps = probes(&mut rtt, start, &[(3_770, 0); 2]);
        assert_eq!(fps, [60, 60]);

        // The receipts' controller reads the same queues, on the viewer's
        // clock: receipts the uplink bunches and holds 300 ms, of units that
        // waited for nothing toward the viewer, cut nothing ...
        let mut qos = balanced(t0);
        qos.use_one_way();
        one_way_probes(&mut qos, t0, &[(20, 0, 0); 8]);
        let start = qos.bpf;
        let receipt = |arrived_ms: u64, forward_ms: u64| Receipt {
            delay: Duration::from_millis(330),
            bytes: 4250,
            behind_idr: false,
            arrival: Some(crate::credit::Arrival {
                at: Duration::from_millis(arrived_ms),
                forward_queue: Duration::from_millis(forward_ms),
            }),
        };
        let host = |n: u64| t0 + Duration::from_millis(2000 + 51 * (n / 3) + n % 3);
        for n in 0..60 {
            qos.on_receipt(Some(receipt(5_000 + 17 * n, 0)), false, host(n));
        }
        let view = qos.receipt_view(host(59));
        assert_eq!(
            (view.standing, view.cuts),
            (Some(Duration::ZERO), 0),
            "{view:?}"
        );
        assert_eq!(qos.bpf, start);
        // ... while 2 Mbit/s with 100 ms of queue toward the viewer does.
        for n in 60..120 {
            qos.on_receipt(Some(receipt(5_000 + 17 * n, 100)), false, host(n));
        }
        let view = qos.receipt_view(host(119));
        assert_eq!(view.busy_bps, Some(2_000_000), "{view:?}");
        assert_eq!(view.cuts, 1, "{view:?}");
        assert!(qos.bpf < start, "{}", qos.bpf);
        // A receipt whose stamp was no sample is left out.
        qos.on_receipt(
            Some(Receipt {
                arrival: None,
                ..receipt(0, 0)
            }),
            false,
            host(120),
        );
        assert_eq!(qos.receipt_view(host(120)), view);
    }

    /// A Balanced H.264 session at 1080p60 accepted at `now`, after the
    /// session its transport ended last left `previous`.
    fn balanced_after(previous: ClosingRate, now: Instant) -> Qos {
        Qos::new(
            limits(60),
            VideoCodec::H264,
            QualityPreset::Balanced,
            false,
            RateModel::Nominal,
            Some(previous),
            now,
        )
    }

    /// A session its transport ended starts the next of its preset no higher
    /// than the bits per frame it ended at, relative to 1080p, for 2 min after
    /// it ended: never above the preset's start, never for another preset, and
    /// with the frame rate the preset always starts at.
    #[test]
    fn a_closing_rate_only_lowers_its_presets_start_for_two_minutes() {
        let t0 = Instant::now();
        let after = |elapsed: Duration| t0 + elapsed;
        let uhd = VideoLimits {
            max_width: 3840,
            max_height: 2160,
            max_fps: 60,
        };
        let scale = 4f64.powf(0.75);
        // A 4K session at Balanced's floor ends: it leaves the floor at 1080p.
        let mut ended = Qos::new(
            uhd,
            VideoCodec::H264,
            QualityPreset::Balanced,
            false,
            RateModel::Nominal,
            None,
            t0,
        );
        ended.set_bpf(ended.clamp_bpf(16_667.0 * scale));
        let record = ended.closing_rate(t0);
        assert_eq!(record.preset, QualityPreset::Balanced);
        assert!((record.bpf - 16_667.0).abs() < 1e-6, "{}", record.bpf);

        // Up to 2 min later a 1080p session of that preset starts there; its
        // encoder is asked for the floor's 1 Mbit/s, its first tick weighs
        // frames at that budget, and its frame rate starts as Balanced's does.
        for elapsed in [Duration::ZERO, Duration::from_secs(60), START_MEMORY] {
            let qos = balanced_after(record, after(elapsed));
            assert!(
                (qos.bpf - 16_667.0).abs() < 1e-6,
                "{elapsed:?}: {}",
                qos.bpf
            );
            assert_eq!(qos.targets(after(elapsed)).encoder_bps, 1_000_020);
            assert_eq!(qos.span.peak_budget, qos.nominal_budget());
            assert_eq!((qos.fps, qos.start_high), (45, true));
        }
        // A 4K session starts at 4K's share of it, which the size the encoder
        // makes rescales as it does any start.
        let mut native = Qos::new(
            uhd,
            VideoCodec::H264,
            QualityPreset::Balanced,
            false,
            RateModel::Nominal,
            Some(record),
            t0,
        );
        assert!(
            (native.bpf - 16_667.0 * scale).abs() < 1e-6,
            "{}",
            native.bpf
        );
        native.set_video_size(1920, 1080);
        assert!((native.bpf - 16_667.0).abs() < 1e-6, "{}", native.bpf);

        // Later than that, or for another preset, the preset's start stands.
        let late = balanced_after(record, after(START_MEMORY + Duration::from_millis(1)));
        assert_eq!(late.bpf, 66_667.0);
        let quality = Qos::new(
            limits(60),
            VideoCodec::H264,
            QualityPreset::Quality,
            false,
            RateModel::Nominal,
            Some(record),
            t0,
        );
        assert_eq!(quality.bpf, 200_000.0);
        // A session that ended above the start never raises it, and one just
        // below lowers it only as far as it ended.
        let high = ClosingRate {
            bpf: 133_333.0,
            ..record
        };
        assert_eq!(balanced_after(high, t0).bpf, 66_667.0);
        let near = ClosingRate {
            bpf: 60_000.0,
            ..record
        };
        assert_eq!(balanced_after(near, t0).bpf, 60_000.0);
    }

    #[test]
    fn gpu_output_dimensions_fit_renderer_without_upscaling() {
        assert_eq!(scaled_size(2560, 1440, 1920, 1080), (1920, 1080));
        assert_eq!(scaled_size(1280, 720, 1920, 1080), (1280, 720));
        assert_eq!(scaled_size(3440, 1440, 1920, 1080), (1920, 802));
        assert_eq!(scaled_size(1080, 1920, 1920, 1080), (606, 1080));
    }

    #[test]
    fn bits_per_frame_steps_and_one_way_latch() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut qos = balanced(t0);
        // 4 Mbit/s at 60 fps; the per-frame-budget encoder gets bpf × nominal.
        assert_eq!(qos.bpf, 66_667.0);
        assert_eq!(qos.targets(s(2)).encoder_bps, 4_000_020);
        // Bits per frame follow the encoded pixels to the power 0.75, within
        // 0.4 and 3: a 4K picture gets 2.83 times 1080p's. Quality at a frame
        // rate the user chose has that rate for its ceiling, and its encoder
        // gets more than the 40 Mbit/s an encoder was once clamped at.
        let uhd = VideoLimits {
            max_width: 3840,
            max_height: 2160,
            max_fps: 120,
        };
        let chosen = Qos::new(
            uhd,
            VideoCodec::H265,
            QualityPreset::Quality,
            true,
            RateModel::Nominal,
            None,
            t0,
        );
        assert!((chosen.bpf - 200_000.0 * 4f64.powf(0.75)).abs() < 1.0);
        assert_eq!(chosen.ceiling_fps, 120);
        assert!(chosen.targets(s(2)).encoder_bps > 67_000_000);
        let auto = Qos::new(
            uhd,
            VideoCodec::H265,
            QualityPreset::Quality,
            false,
            RateModel::Nominal,
            None,
            t0,
        );
        assert_eq!(auto.ceiling_fps, 30);
        assert_eq!((pixel_scale(7680, 4320), pixel_scale(640, 360)), (3.0, 0.4));
        // The same bounds on a 1080p display encode 1080p: told so before the
        // first unit, the first IDR is budgeted for that, not for the bounds,
        // and a first unit of another size still corrects the budget.
        let mut native = Qos::new(
            VideoLimits { max_fps: 60, ..uhd },
            VideoCodec::H264,
            QualityPreset::Balanced,
            false,
            RateModel::Nominal,
            None,
            t0,
        );
        let (width, height) = scaled_size(1920, 1080, uhd.max_width, uhd.max_height);
        native.set_video_size(width, height);
        assert!((native.bpf - 66_667.0).abs() < 1.0);
        native.set_video_size(3840, 2160);
        assert!((native.bpf - 66_667.0 * 4f64.powf(0.75)).abs() < 1.0);
        let produced = |unique: u64, non_idr_frames: u64, non_idr_bytes: u64| ProducedSnapshot {
            frames: unique,
            unique,
            keyframes: 0,
            non_idr_frames,
            non_idr_bytes,
        };

        // The first untainted sample steps at once; a quiet link changes
        // nothing without moving content.
        sample(&mut qos, s(0), 20, false);
        assert_eq!(qos.bpf, 66_667.0);
        // A window with moving content and a quiet link rises by at most
        // 150 kbit/s at the current pace.
        sample(&mut qos, s(1), 20, false);
        qos.tick(s(3), &idle(), produced(100, 0, 0));
        let pace = f64::from(qos.pace(s(3)));
        assert!(
            (qos.bpf - (66_667.0 + 150_000.0 / pace)).abs() < 1e-6,
            "{}",
            qos.bpf
        );
        // A static window holds.
        let held = qos.bpf;
        sample(&mut qos, s(4), 20, false);
        qos.tick(s(6), &idle(), produced(103, 0, 0));
        assert_eq!(qos.bpf, held);
        // A window without an untainted sample holds too.
        qos.tick(s(9), &idle(), produced(200, 0, 0));
        assert_eq!(qos.bpf, held);
        // 250 ms of queueing: ×0.90, moving content or not.
        sample(&mut qos, s(10), 20, false);
        qos.control_fps(250, 250, true, false, s(10));
        qos.tick(s(12), &idle(), produced(200, 0, 0));
        assert!((qos.bpf - held * 0.90).abs() < 1e-6);
        // Only probes behind IDRs: a quick IDR holds, even with moving
        // content ...
        let lowered = qos.bpf;
        sample(&mut qos, s(12), 60, true);
        qos.tick(s(15), &idle(), produced(300, 0, 0));
        assert_eq!(qos.bpf, lowered);
        // ... and IDRs that take over a second to cross step bits per frame
        // down, which shrinks them.
        sample(&mut qos, s(15), 1220, true);
        qos.tick(s(18), &idle(), produced(400, 0, 0));
        assert!((qos.bpf - lowered * 0.80).abs() < 1e-6, "{}", qos.bpf);
        // The preset floor and max bound every step.
        for n in 0..40 {
            qos.control_fps(900, 900, true, false, s(21 + 3 * n));
            qos.tick(s(21 + 3 * n), &idle(), produced(400, 0, 0));
        }
        assert_eq!(qos.bpf, 16_667.0);
        let mut unique = 400;
        for n in 0..400 {
            unique += 100;
            qos.control_fps(0, 0, false, false, s(200 + 3 * n));
            qos.tick(s(200 + 3 * n), &idle(), produced(unique, 0, 0));
        }
        assert_eq!(qos.bpf, 133_333.0);

        // A preset switch restarts fps and bits per frame from the preset and
        // keeps the rate model.
        qos.rate_model = RateModel::Timestamp;
        qos.set_preset(QualityPreset::Quality, s(2000), produced(unique, 0, 0));
        assert_eq!(
            (qos.fps, qos.bpf, qos.rate_model),
            (24, 200_000.0, RateModel::Timestamp)
        );
        assert!(qos.window.max_delay.is_none());

        // The latch: at full pace an overshoot is no evidence ...
        let mut qos = balanced(t0);
        let bpf = qos.bpf;
        // A short path lifts Balanced to its ceiling.
        sample(&mut qos, t0, 20, false);
        let mut ticks = Ticks::new(t0);
        ticks.run(&mut qos, Budget::Fixed(2.0 * bpf), 40);
        assert_eq!(qos.rate_model, RateModel::Nominal);
        // ... nor is a per-frame budget at a lower pace ...
        qos.set_viewer_cap(30);
        ticks.run(&mut qos, Budget::PerFrame, 40);
        assert_eq!(qos.rate_model, RateModel::Nominal);
        // ... but a per-second budget at pace 30, twice the budget per frame,
        // latches the timestamp model, and the encoder bitrate then follows
        // pace.
        assert!(ticks.latched(&mut qos, PER_SECOND, 12).is_some());
        assert_eq!(qos.targets(ticks.now).encoder_bps, (qos.bpf * 30.0) as u32);
        // It never flips back.
        qos.set_viewer_cap(60);
        ticks.run(&mut qos, Budget::PerFrame, 40);
        assert_eq!(qos.rate_model, RateModel::Timestamp);

        // A backend that budgets per second starts there: from the start its
        // encoder gets bits per frame × the pace, and no window moves it.
        let mut qos = Qos::new(
            limits(60),
            VideoCodec::H264,
            QualityPreset::Balanced,
            false,
            RateModel::Timestamp,
            None,
            t0,
        );
        // A short path lifts Balanced to its ceiling.
        sample(&mut qos, s(0), 20, false);
        for pace in [60, 30, 10] {
            qos.set_viewer_cap(pace);
            let targets = qos.targets(s(2));
            assert_eq!(targets.pace_fps, pace);
            assert_eq!(targets.encoder_bps, (bpf * f64::from(pace)) as u32);
        }
        Ticks::new(t0).run(&mut qos, Budget::Fixed(4.0 * bpf), 40);
        assert_eq!(qos.view(s(10)).rate_model, RateModel::Timestamp);
    }

    /// A Balanced session on a short path, at its ceiling of 60.
    fn short_path(t0: Instant) -> Qos {
        let mut qos = balanced(t0);
        sample(&mut qos, t0, 20, false);
        qos
    }

    /// An encoder that budgets per second, which the nominal model gives
    /// `bits per frame × nominal`: at a quarter of the pace each frame gets four
    /// budgets. The latch must find it within 2.5 s of the pace falling, not
    /// after two 3 s windows.
    #[test]
    fn a_per_second_budget_latches_within_two_and_a_half_seconds() {
        let t0 = Instant::now();
        let mut qos = short_path(t0);
        let mut ticks = Ticks::new(t0);
        // At full pace both budgets agree: 10 s of it latch nothing.
        ticks.run(&mut qos, PER_SECOND, 40);
        assert_eq!(qos.rate_model, RateModel::Nominal);
        assert_eq!(qos.targets(ticks.now).pace_fps, 60);
        qos.set_viewer_cap(15);
        let capped = ticks.now;
        let latched = ticks.latched(&mut qos, PER_SECOND, 40);
        let after = latched.map(|latched| latched - capped);
        assert!(
            after.is_some_and(|after| after <= Duration::from_millis(2500)),
            "{after:?}"
        );
        // From then on the encoder gets bits per frame × the pace.
        assert_eq!(qos.targets(ticks.now).encoder_bps, (qos.bpf * 15.0) as u32);
    }

    /// A per-second encoder whose pictures fill only part of its budget still
    /// sends that share of `nominal / pace` times what is expected. It latches
    /// within 2.5 s above `LATCH_PACE_SHARE` of that ratio, and above
    /// `LATCH_FLOOR_MAX` however low the pace.
    #[test]
    fn a_per_second_budget_part_filled_still_latches() {
        let t0 = Instant::now();
        // 1.5, 2.4, 3 and 3 times what is expected.
        for (cap, share) in [(30, 0.75), (15, 0.6), (10, 0.5), (5, 0.25)] {
            let mut qos = short_path(t0);
            let mut ticks = Ticks::new(t0);
            ticks.run(&mut qos, Budget::PerSecond(share), 40);
            qos.set_viewer_cap(cap);
            let capped = ticks.now;
            let after = ticks
                .latched(&mut qos, Budget::PerSecond(share), 40)
                .map(|latched| latched - capped);
            assert!(
                after.is_some_and(|after| after <= Duration::from_millis(2500)),
                "cap {cap}, share {share}: {after:?}"
            );
        }
    }

    /// A per-frame budget whose bits per frame are cut in the middle of each
    /// 3 s window, as a controller that cuts on receipts moves them, and
    /// raised at its end. Its frames before each cut, at the higher budget,
    /// are no overshoot of the lower one.
    #[test]
    fn bits_per_frame_cuts_within_a_window_never_latch_a_per_frame_budget() {
        let t0 = Instant::now();
        let mut qos = short_path(t0);
        qos.set_viewer_cap(30);
        let mut ticks = Ticks::new(t0);
        let high = qos.bpf;
        for _ in 0..10 {
            ticks.run(&mut qos, Budget::PerFrame, 6);
            qos.set_bpf(qos.clamp_bpf(high / 2.0));
            ticks.run(&mut qos, Budget::PerFrame, 6);
            qos.set_bpf(high);
        }
        assert_eq!(qos.rate_model, RateModel::Nominal);
        // Cut at every tick to the floor, then raised again: never either.
        for _ in 0..10 {
            for _ in 0..8 {
                qos.set_bpf(qos.clamp_bpf(qos.bpf * 0.8));
                ticks.tick(&mut qos, Budget::PerFrame);
            }
            qos.set_bpf(high);
        }
        assert_eq!(qos.rate_model, RateModel::Nominal);
    }

    /// Bits per frame raised as a tick ends, as the 3 s window raises them,
    /// and halved part-way through the next tick, as a cut on receipts would:
    /// each tick holds frames from both sides of a cut. They are measured
    /// against the higher budget, so a per-frame encoder never latches, though
    /// both of the tick's ends saw only the lower one.
    #[test]
    fn a_cut_part_way_through_a_tick_never_latches_a_per_frame_budget() {
        let t0 = Instant::now();
        let mut qos = short_path(t0);
        qos.set_viewer_cap(30);
        let mut ticks = Ticks::new(t0);
        let high = qos.bpf;
        for _ in 0..40 {
            qos.set_bpf(high);
            let bits = Budget::PerFrame.bits(&qos, ticks.now, 30.0);
            ticks.produce(4, 4, bits);
            qos.set_bpf(qos.clamp_bpf(high / 2.0));
            let bits = Budget::PerFrame.bits(&qos, ticks.now, 30.0);
            ticks.produce(4, 4, bits);
            ticks.advance(&mut qos);
        }
        assert_eq!(qos.rate_model, RateModel::Nominal);
    }

    /// A viewer's cap set part-way through a tick: the frames before it came
    /// at the full pace, so that tick is not paced low, and the latch needs
    /// `LATCH_TICKS` ticks wholly at the lower pace after it. A cap lifted
    /// part-way through a tick does not let that tick count either.
    #[test]
    fn a_tick_at_full_pace_at_either_end_never_counts_toward_the_latch() {
        let t0 = Instant::now();
        let mut qos = short_path(t0);
        let mut ticks = Ticks::new(t0);
        ticks.run(&mut qos, PER_SECOND, 40);
        assert_eq!(qos.targets(ticks.now).pace_fps, 60);
        let bits = PER_SECOND.bits(&qos, ticks.now, 60.0);
        ticks.produce(8, 8, bits);
        qos.set_viewer_cap(15);
        let bits = PER_SECOND.bits(&qos, ticks.now, 15.0);
        ticks.produce(2, 2, bits);
        ticks.advance(&mut qos);
        let mixed = ticks.now;
        let after = ticks
            .latched(&mut qos, PER_SECOND, 40)
            .map(|latched| latched - mixed);
        assert_eq!(after, Some(QOS_TICK * LATCH_TICKS as u32));

        let mut qos = short_path(t0);
        let mut ticks = Ticks::new(t0);
        ticks.run(&mut qos, PER_SECOND, 40);
        qos.set_viewer_cap(15);
        // The tick the cap came in, at 60 at its start, and seven wholly at
        // the lower pace ...
        ticks.run(&mut qos, PER_SECOND, LATCH_TICKS);
        assert_eq!(qos.rate_model, RateModel::Nominal);
        // ... then half a tick at it, the cap lifted, and half at 60.
        let bits = PER_SECOND.bits(&qos, ticks.now, 15.0);
        ticks.produce(2, 2, bits);
        qos.set_viewer_cap(60);
        let bits = PER_SECOND.bits(&qos, ticks.now, 60.0);
        ticks.produce(8, 8, bits);
        ticks.advance(&mut qos);
        ticks.run(&mut qos, PER_SECOND, 40);
        assert_eq!(qos.rate_model, RateModel::Nominal);
    }

    /// Pictures that come slower than the pace, as a screen that changes now
    /// and then gives them, each span more motion: the mean pace is the
    /// frames produced, so a per-frame encoder whose sparse frames sit at
    /// 1.5 times their budget does not latch at a pace of 45. A per-second
    /// encoder, whose frames then carry a third of a second each, does.
    #[test]
    fn sparse_pictures_are_held_to_their_own_rate() {
        let t0 = Instant::now();
        let session = |t0| {
            let mut qos = short_path(t0);
            qos.set_viewer_cap(45);
            qos
        };
        let mut qos = session(t0);
        let floor = 1.5 * qos.nominal_budget();
        let mut ticks = Ticks::new(t0);
        ticks.rate = Some(20.0);
        ticks.run(&mut qos, Budget::Fixed(floor), 40);
        assert_eq!(qos.rate_model, RateModel::Nominal);

        let mut qos = session(t0);
        let mut ticks = Ticks::new(t0);
        ticks.rate = Some(20.0);
        assert!(ticks.latched(&mut qos, PER_SECOND, 12).is_some());
    }

    /// A still picture, repeated, says nothing of how the encoder budgets:
    /// fewer than six unique frames in the span latch nothing, however many
    /// bits they carry. Here a change every other tick carries a per-second
    /// budget's half second, and the repeats almost nothing.
    #[test]
    fn fewer_than_six_unique_frames_never_latch() {
        let t0 = Instant::now();
        let mut qos = short_path(t0);
        qos.set_viewer_cap(15);
        let mut ticks = Ticks::new(t0);
        for n in 0..40 {
            let budget = qos.nominal_budget();
            if n % 2 == 0 {
                ticks.produce(1, 1, 30.0 * budget);
                ticks.produce(3, 0, 0.05 * budget);
            } else {
                ticks.produce(4, 0, 0.05 * budget);
            }
            ticks.advance(&mut qos);
        }
        assert_eq!(qos.rate_model, RateModel::Nominal);
        // Once the picture moves, the same encoder latches.
        assert!(ticks.latched(&mut qos, PER_SECOND, 12).is_some());
    }

    /// A session tick can follow the last one early, after a late one: one
    /// less than half a QoS tick after the last is folded into the next, so
    /// the span still covers 2 s and a per-second budget latches no sooner.
    #[test]
    fn an_early_tick_is_folded_into_the_next() {
        let t0 = Instant::now();
        let mut qos = short_path(t0);
        let mut ticks = Ticks::new(t0);
        ticks.run(&mut qos, PER_SECOND, 40);
        qos.set_viewer_cap(15);
        let capped = ticks.now;
        let latched = (0..40).find_map(|_| {
            ticks.tick(&mut qos, PER_SECOND);
            let early = ticks.now + Duration::from_millis(10);
            qos.tick(early, &idle(), ticks.produced);
            (qos.rate_model == RateModel::Timestamp).then_some(ticks.now)
        });
        let after = latched.map(|latched| latched - capped);
        assert!(
            after.is_some_and(|after| after > LATCH_SPAN && after <= Duration::from_millis(2500)),
            "{after:?}"
        );
    }

    /// Bits per frame at the preset floor of a small picture ask for less than
    /// the encoder bitrate's lower bound, which then gives each frame more: a
    /// per-frame encoder that spends that budget is no overshoot.
    #[test]
    fn a_per_frame_budget_the_bitrate_bound_raised_never_latches() {
        let t0 = Instant::now();
        let mut qos = Qos::new(
            limits(30),
            VideoCodec::H264,
            QualityPreset::Smooth,
            false,
            RateModel::Nominal,
            None,
            t0,
        );
        qos.set_video_size(960, 540);
        qos.set_bpf(qos.clamp_bpf(0.0));
        assert_eq!(qos.targets(t0).encoder_bps, ENCODER_BITRATE_MIN);
        assert!(qos.nominal_budget() > 1.7 * qos.bpf, "{}", qos.bpf);
        qos.set_viewer_cap(15);
        Ticks::new(t0).run(&mut qos, Budget::PerFrame, 40);
        assert_eq!(qos.rate_model, RateModel::Nominal);
    }

    /// NVIDIA's H.264 MFT budgets per frame, but at a nominal 120 FPS and
    /// 4 Mbit/s its pattern's frames at pace 5 reached 1.79 times their budget:
    /// the moving picture's floor, not a per-second budget. Such an encoder
    /// never latches, while one that budgets per second at the same pace,
    /// 24 times over, still does.
    #[test]
    fn a_per_frame_encoder_at_its_content_floor_never_latches() {
        let t0 = Instant::now();
        let smooth = |t0| {
            let mut qos = Qos::new(
                limits(120),
                VideoCodec::H264,
                QualityPreset::Smooth,
                false,
                RateModel::Nominal,
                None,
                t0,
            );
            qos.set_viewer_cap(5);
            qos
        };
        let mut qos = smooth(t0);
        let floor = MEASURED_CONTENT_FLOOR * qos.nominal_budget();
        Ticks::new(t0).run(&mut qos, Budget::Fixed(floor), 120);
        assert_eq!(qos.rate_model, RateModel::Nominal);

        let mut qos = smooth(t0);
        assert!(Ticks::new(t0).latched(&mut qos, PER_SECOND, 12).is_some());
    }
}
