//! Delay-based QoS for one host session, after RustDesk's `video_qos.rs`. A
//! probe written in stream order and echoed by the viewer's network task
//! measures downstream queueing; fps follows each sample and bits per frame
//! follow each 3 s window. Resolution never adapts, and no fps, bitrate or
//! preset change rebuilds the encoder: the encoder runs at the session's
//! nominal fps, and capture skips ticks to the pace published here.
//!
//! Unlike RustDesk, one stalled probe moves nothing: Wi-Fi holds a packet for
//! hundreds of milliseconds now and then without any queue behind it. The
//! delay is the median of the last three samples, a cut needs the receipts to
//! show a standing queue too unless two probes in a row saw one, and a cut
//! the receipts alone confirm at most halves fps.
use crate::{
    QualityPreset, VideoCodec, VideoLimits,
    credit::{ACK_QUEUE_LIMIT, CreditView},
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
/// Balanced starts at its ceiling when the first probe after the preset is
/// applied comes back within this.
pub(crate) const FAST_START_RTT: Duration = Duration::from_millis(50);
/// A delay from which fps falls.
const CUT_DELAY: u32 = 150;
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
/// Unique frames a window needs before bits per frame may rise.
pub(crate) const DYNAMIC_UNIQUE_FRAMES: u64 = 6;
/// Non-IDR bits above this multiple of bits per frame are an overshoot.
pub(crate) const LATCH_OVERSHOOT: f64 = 1.3;
/// Consecutive overshoot windows that latch the timestamp rate model.
pub(crate) const LATCH_WINDOWS: u32 = 2;
/// The latch looks only at windows paced at most this share of nominal.
pub(crate) const LATCH_PACE_RATIO: f64 = 0.75;
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

/// A preset's fps and bits per frame (at 1080p).
struct Preset {
    min_fps: u32,
    normal_fps: u32,
    ceiling_fps: u32,
    bpf_start: f64,
    bpf_max: f64,
    bpf_floor: f64,
}
const QUALITY: Preset = Preset {
    min_fps: 8,
    normal_fps: 24,
    ceiling_fps: 30,
    bpf_start: 200_000.0,
    bpf_max: 400_000.0,
    bpf_floor: 50_000.0,
};
const BALANCED: Preset = Preset {
    min_fps: 10,
    normal_fps: 45,
    ceiling_fps: 60,
    bpf_start: 66_667.0,
    bpf_max: 133_333.0,
    bpf_floor: 16_667.0,
};
const SMOOTH: Preset = Preset {
    min_fps: 12,
    normal_fps: 50,
    ceiling_fps: 120,
    bpf_start: 50_000.0,
    bpf_max: 100_000.0,
    bpf_floor: 12_500.0,
};
/// A preset's fps and bits per frame are each in order, floor to max.
const fn ordered(p: &Preset) -> bool {
    p.min_fps <= p.normal_fps
        && p.normal_fps <= p.ceiling_fps
        && p.bpf_floor <= p.bpf_start
        && p.bpf_start <= p.bpf_max
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
    overshoot: u32,
    window: RatioWindow,
    /// An untainted sample was seen this session.
    sampled: bool,
    next_nonce: u64,
    outstanding: Option<Probe>,
    last_probe: Option<Instant>,
    /// `(echoed at, rtt)` over the last `RTT_WINDOW`.
    rtt: VecDeque<(Instant, Duration)>,
    last_rtt: Option<Duration>,
    history: VecDeque<Duration>,
    tainted: bool,
    viewer_cap: Option<u32>,
    paused: bool,
    unresponsive: bool,
    startup_until: Instant,
    adaptation: Adaptation,
}
impl Qos {
    /// Bits per frame start at the viewer's bounds, until `set_video_size`
    /// says what the encoder produces: the host's expectation before the first
    /// unit, then each unit's own size.
    pub fn new(
        limits: VideoLimits,
        codec: VideoCodec,
        quality: QualityPreset,
        fps_override: bool,
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
            rate_model: RateModel::Nominal,
            overshoot: 0,
            window: RatioWindow::new(now, ProducedSnapshot::default()),
            sampled: false,
            next_nonce: 1,
            outstanding: None,
            last_probe: None,
            rtt: VecDeque::new(),
            last_rtt: None,
            history: VecDeque::with_capacity(DELAY_HISTORY),
            tainted: false,
            viewer_cap: None,
            paused: false,
            unresponsive: false,
            startup_until: now + STARTUP_HOLD,
            adaptation: Adaptation::Steady,
        };
        qos.apply_preset(quality);
        qos
    }

    /// The fps and bits per frame a preset starts from. Balanced starts at its
    /// ceiling instead when the next probe finds a short, empty path.
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
        self.start_high = quality == QualityPreset::Balanced;
        self.bpf = p.bpf_start * self.pixel_scale;
    }

    /// A live preset change: fps and bits per frame restart from the preset,
    /// the rate model is kept, and nothing rebuilds the encoder.
    pub fn set_preset(&mut self, quality: QualityPreset, now: Instant, produced: ProducedSnapshot) {
        self.apply_preset(quality);
        self.quick = 0;
        self.increase = 0;
        self.window = RatioWindow::new(now, produced);
    }

    /// The viewer's runtime fps cap; a cap at or above `limits.max_fps` clears it.
    pub fn set_viewer_cap(&mut self, fps: u32) {
        self.viewer_cap = (fps < self.limits.max_fps).then_some(fps);
    }

    pub fn set_paused(&mut self) {
        self.paused = true;
        self.unresponsive = false;
    }

    /// Back through the startup cap, with a fresh delay history.
    pub fn resume(&mut self, now: Instant) {
        self.paused = false;
        self.unresponsive = false;
        self.startup_until = now + STARTUP_HOLD;
        self.history.clear();
        self.last_raw = None;
    }

    /// A new viewer path: the old baseline means nothing. fps and bits per
    /// frame are kept.
    pub fn reset_baseline(&mut self) {
        self.rtt.clear();
        self.history.clear();
        self.last_raw = None;
    }

    /// A new encoded size: bits per frame keep their 1080p-relative level.
    pub fn set_video_size(&mut self, width: u32, height: u32) {
        let scale = pixel_scale(width, height);
        if scale != self.pixel_scale {
            self.bpf *= scale / self.pixel_scale;
            self.pixel_scale = scale;
            self.bpf = self.clamp_bpf(self.bpf);
        }
    }

    /// A transport receipt arrived: the viewer is alive.
    pub fn on_receipt(&mut self) {
        self.unresponsive = false;
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

    /// An echo of the outstanding probe; any other nonce is ignored.
    /// `ack_queue` is the receipts' queue delay at the echo, the witness a cut
    /// needs unless the probes' own delay persists.
    pub fn on_probe_echo(&mut self, nonce: u64, now: Instant, ack_queue: Option<Duration>) {
        let Some(probe) = self.outstanding.take_if(|probe| probe.nonce == nonce) else {
            return;
        };
        self.unresponsive = false;
        let elapsed = now.saturating_duration_since(probe.sent);
        if elapsed > IO_TIMEOUT {
            return;
        }
        let rtt = elapsed.max(DELAY_FLOOR);
        self.last_rtt = Some(rtt);
        while self
            .rtt
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > RTT_WINDOW)
        {
            self.rtt.pop_front();
        }
        self.rtt.push_back((now, rtt));
        self.tainted = probe.tainted;
        let Some(base) = self.base_rtt() else {
            return;
        };
        let raw = millis(rtt.saturating_sub(base));
        if probe.tainted {
            // Kept apart, for a window that has nothing else to go on.
            let max = &mut self.window.max_tainted;
            *max = Some(max.map_or(raw, |max| max.max(raw)));
            return;
        }
        if self.history.len() == DELAY_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(rtt);
        let Some(delay) = self.delay_ms() else {
            return;
        };
        if std::mem::take(&mut self.start_high) && rtt < FAST_START_RTT {
            self.fps = self.ceiling_fps;
        }
        let witnessed = ack_queue.is_some_and(|queue| queue >= ACK_QUEUE_LIMIT);
        self.control_fps(delay, raw, witnessed);
        if !self.sampled {
            // As RustDesk: the first delay also sets bits per frame at once,
            // which can only lower them.
            self.sampled = true;
            self.step_bpf(delay, false, now);
        }
    }

    /// Unresponsiveness, and the bits-per-frame window when it is due.
    pub fn tick(&mut self, now: Instant, credit: &CreditView, produced: ProducedSnapshot) {
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
    }

    pub fn targets(&self, now: Instant) -> TargetValues {
        let pace_fps = self.pace(now);
        let bps = match self.rate_model {
            RateModel::Nominal => self.bpf * f64::from(self.limits.max_fps),
            RateModel::Timestamp => self.bpf * f64::from(pace_fps),
        };
        TargetValues {
            pace_fps,
            encoder_bps: (bps as u32).clamp(ENCODER_BITRATE_MIN, ENCODER_BITRATE_MAX),
            paused: self.paused,
        }
    }

    /// What the viewer is expected to receive: bits per frame at the pace.
    pub fn expected_bps(&self, now: Instant) -> u64 {
        (self.bpf * f64::from(self.pace(now))) as u64
    }

    pub fn base_rtt(&self) -> Option<Duration> {
        self.rtt.iter().map(|(_, rtt)| *rtt).min()
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
        }
    }

    /// The capture pace. The fps state is never changed by the startup cap,
    /// the viewer cap, pause or unresponsiveness: those clamp only the output.
    fn pace(&self, now: Instant) -> u32 {
        if self.paused {
            return 0;
        }
        let mut pace = self
            .fps
            .min(self.ceiling_fps)
            .min(self.viewer_cap.unwrap_or(u32::MAX))
            .min(self.limits.max_fps);
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

    /// The median of the recent untainted RTTs above the baseline; of two,
    /// the lower. One stalled probe among them moves it not at all.
    fn delay_ms(&self) -> Option<u32> {
        let base = self.base_rtt()?;
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
    /// fps. A short path takes the quick step at every sample.
    fn control_fps(&mut self, delay: u32, raw: u32, witnessed: bool) {
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
        self.adaptation = match self.fps.cmp(&before) {
            std::cmp::Ordering::Greater => Adaptation::Raising,
            std::cmp::Ordering::Less => Adaptation::LoweringFps,
            std::cmp::Ordering::Equal => Adaptation::Steady,
        };
    }

    /// Close a 3 s window: the rate-model latch, then the bits-per-frame step.
    fn close_window(&mut self, now: Instant, produced: ProducedSnapshot) {
        let start = self.window.produced;
        let dynamic = produced.unique.saturating_sub(start.unique) >= DYNAMIC_UNIQUE_FRAMES;
        if self.rate_model == RateModel::Nominal && dynamic {
            // A per-second-budget encoder spends nominal/pace times bits per
            // frame when the pace drops; a per-frame-budget one does not.
            let frames = produced.non_idr_frames.saturating_sub(start.non_idr_frames);
            let bits = produced.non_idr_bytes.saturating_sub(start.non_idr_bytes) * 8;
            let slow = frames as f64
                <= LATCH_PACE_RATIO * f64::from(self.limits.max_fps) * RATIO_INTERVAL.as_secs_f64();
            if slow && bits as f64 > LATCH_OVERSHOOT * self.bpf * frames as f64 {
                self.overshoot += 1;
            } else {
                self.overshoot = 0;
            }
            if self.overshoot >= LATCH_WINDOWS {
                self.rate_model = RateModel::Timestamp;
                tracing::info!(
                    target: "pb_mapper_desktop::feedback",
                    event = "desktop_rate_model_latched",
                    frames,
                    bits,
                    bpf = self.bpf as u32,
                );
            }
        }
        match (self.window.max_delay, self.window.max_tainted) {
            (Some(max_delay), _) => self.step_bpf(max_delay, dynamic, now),
            // Only probes behind IDRs: their delay is mostly the IDRs' own
            // transfer. It may lower bits per frame, which shrinks the IDRs,
            // but never raise them; otherwise a link where every IDR takes
            // seconds would freeze the controller.
            (None, Some(tainted)) => self.step_bpf(tainted, false, now),
            // No sample in the window: hold.
            (None, None) => {}
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
        self.adaptation = if bpf > self.bpf {
            Adaptation::Raising
        } else if bpf < self.bpf {
            Adaptation::LoweringBitrate
        } else {
            Adaptation::Steady
        };
        self.bpf = bpf;
    }

    fn clamp_bpf(&self, bpf: f64) -> f64 {
        let p = preset(self.preset);
        bpf.clamp(p.bpf_floor * self.pixel_scale, p.bpf_max * self.pixel_scale)
    }
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
            Some(Duration::from_millis(ack_queue_ms)),
        );
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

    #[test]
    fn balanced_paces_at_its_ceiling_within_two_seconds_of_a_short_path() {
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

        // A long path starts where Balanced always did, probed once a second.
        let mut far = balanced(t0);
        sample(&mut far, t0, 120, false);
        assert_eq!(far.fps, 45);
        assert_eq!(far.probe_due_at(at(500)), Some(at(1000)));
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
        qos.on_receipt();
        assert_eq!(qos.pace(s(12)), 40);
        // A late echo of the outstanding probe recovers it too, but its stale
        // RTT is not a sample.
        qos.tick(s(13), &idle(), ProducedSnapshot::default());
        assert!(qos.unresponsive);
        let samples = qos.rtt.len();
        qos.on_probe_echo(nonce, s(13), None);
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
        qos.on_probe_echo(99, s(14), None);
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
            t0,
        );
        assert_eq!(jpeg.pace(s(2)), JPEG_MAX_FPS);
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
        let chosen = Qos::new(uhd, VideoCodec::H265, QualityPreset::Quality, true, t0);
        assert!((chosen.bpf - 200_000.0 * 4f64.powf(0.75)).abs() < 1.0);
        assert_eq!(chosen.ceiling_fps, 120);
        assert!(chosen.targets(s(2)).encoder_bps > 67_000_000);
        let auto = Qos::new(uhd, VideoCodec::H265, QualityPreset::Quality, false, t0);
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
        qos.control_fps(250, 250, true);
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
            qos.control_fps(900, 900, true);
            qos.tick(s(21 + 3 * n), &idle(), produced(400, 0, 0));
        }
        assert_eq!(qos.bpf, 16_667.0);
        let mut unique = 400;
        for n in 0..400 {
            unique += 100;
            qos.control_fps(0, 0, false);
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
        let window = |n: u64, frames: u64, ratio: f64| {
            produced(
                n * frames,
                n * frames,
                (n as f64 * frames as f64 * bpf * ratio / 8.0) as u64,
            )
        };
        for n in 1..=3 {
            qos.tick(s(3 * n), &idle(), window(n, 180, 2.0));
        }
        assert_eq!(qos.rate_model, RateModel::Nominal);
        // ... nor is a per-frame budget at a lower pace ...
        let mut qos = balanced(t0);
        for n in 1..=3 {
            qos.tick(s(3 * n), &idle(), window(n, 90, 1.0));
        }
        assert_eq!(qos.rate_model, RateModel::Nominal);
        // ... but two windows at pace 30 spending twice the budget per frame
        // latch the timestamp model, and the encoder bitrate then follows pace.
        qos.tick(s(12), &idle(), window(4, 90, 2.0));
        assert_eq!(qos.rate_model, RateModel::Nominal);
        qos.tick(s(15), &idle(), window(5, 90, 2.0));
        assert_eq!(qos.rate_model, RateModel::Timestamp);
        qos.set_viewer_cap(30);
        assert_eq!(qos.targets(s(16)).encoder_bps, (bpf * 30.0) as u32);
        // It never flips back.
        for n in 6..=9 {
            qos.tick(s(3 * n), &idle(), window(n, 180, 1.0));
        }
        assert_eq!(qos.rate_model, RateModel::Timestamp);
    }
}
