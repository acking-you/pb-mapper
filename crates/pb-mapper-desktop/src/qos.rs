//! Delay-based QoS for one host session, after RustDesk's `video_qos.rs`. A
//! probe written in stream order and echoed by the viewer's network task
//! measures downstream queueing; fps follows each sample and bits per frame
//! follow each 3 s window. Resolution never adapts, and no fps, bitrate or
//! preset change rebuilds the encoder: the encoder runs at the session's
//! nominal fps, and capture skips ticks to the pace published here.
use crate::{
    QualityPreset, VideoCodec, VideoLimits,
    credit::CreditView,
    outgoing::ProducedSnapshot,
    protocol::{Adaptation, RateModel},
    wire::IO_TIMEOUT,
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub(crate) const PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// A probe or receipt owed this long marks the viewer unresponsive.
pub(crate) const UNRESPONSIVE_AFTER: Duration = Duration::from_secs(2);
/// The output pace while the viewer is unresponsive.
pub(crate) const UNRESPONSIVE_FPS: u32 = 2;
pub(crate) const DELAY_FLOOR: Duration = Duration::from_millis(10);
/// Untainted samples averaged into the delay.
pub(crate) const DELAY_HISTORY: usize = 3;
/// Samples whose minimum is the RTT baseline.
pub(crate) const RTT_WINDOW: usize = 60;
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
pub(crate) const ENCODER_BITRATE_MIN: u32 = 256_000;
pub(crate) const ENCODER_BITRATE_MAX: u32 = 40_000_000;
const PIXEL_SCALE_MIN: f64 = 0.25;
const PIXEL_SCALE_MAX: f64 = 4.0;
/// Preset bits per frame are given at 1080p.
const FULL_HD_PIXELS: f64 = 1920.0 * 1080.0;
/// The most one window may raise the expected bitrate, in bit/s.
const BPF_RAISE_MAX_BPS: f64 = 150_000.0;

/// A preset's fps and bits per frame (at 1080p).
struct Preset {
    min_fps: u32,
    normal_fps: u32,
    ceiling_fps: u32,
    bpf_start: f64,
    bpf_max: f64,
    bpf_floor: f64,
}
fn preset(quality: QualityPreset) -> Preset {
    match quality {
        QualityPreset::Quality => Preset {
            min_fps: 8,
            normal_fps: 24,
            ceiling_fps: 30,
            bpf_start: 200_000.0,
            bpf_max: 400_000.0,
            bpf_floor: 50_000.0,
        },
        QualityPreset::Balanced => Preset {
            min_fps: 10,
            normal_fps: 45,
            ceiling_fps: 60,
            bpf_start: 66_667.0,
            bpf_max: 133_333.0,
            bpf_floor: 16_667.0,
        },
        QualityPreset::Smooth => Preset {
            min_fps: 12,
            normal_fps: 50,
            ceiling_fps: 120,
            bpf_start: 50_000.0,
            bpf_max: 100_000.0,
            bpf_floor: 12_500.0,
        },
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
}

struct Probe {
    nonce: u64,
    sent: Instant,
    tainted: bool,
}

/// One bits-per-frame window: the worst untainted delay and the production
/// counters it started from.
struct RatioWindow {
    started: Instant,
    max_delay: Option<u32>,
    produced: ProducedSnapshot,
}
impl RatioWindow {
    fn new(started: Instant, produced: ProducedSnapshot) -> Self {
        Self {
            started,
            max_delay: None,
            produced,
        }
    }
}

pub(crate) struct Qos {
    limits: VideoLimits,
    codec: VideoCodec,
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
    rtt: VecDeque<Duration>,
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
    pub fn new(
        limits: VideoLimits,
        codec: VideoCodec,
        quality: QualityPreset,
        now: Instant,
    ) -> Self {
        let pixels = f64::from(limits.max_width) * f64::from(limits.max_height);
        let mut qos = Self {
            limits,
            codec,
            preset: quality,
            min_fps: 1,
            normal_fps: 1,
            ceiling_fps: 1,
            scale: 1.0,
            pixel_scale: (pixels / FULL_HD_PIXELS).clamp(PIXEL_SCALE_MIN, PIXEL_SCALE_MAX),
            fps: 1,
            quick: 0,
            increase: 0,
            last_delay: None,
            bpf: 0.0,
            rate_model: RateModel::Nominal,
            overshoot: 0,
            window: RatioWindow::new(now, ProducedSnapshot::default()),
            sampled: false,
            next_nonce: 1,
            outstanding: None,
            last_probe: None,
            rtt: VecDeque::with_capacity(RTT_WINDOW),
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

    /// The fps and bits per frame a preset starts from.
    fn apply_preset(&mut self, quality: QualityPreset) {
        let p = preset(quality);
        self.preset = quality;
        self.ceiling_fps = p.ceiling_fps.min(self.limits.max_fps);
        self.normal_fps = p.normal_fps.min(self.ceiling_fps);
        self.min_fps = p.min_fps;
        self.scale = (f64::from(self.ceiling_fps) / 30.0).max(1.0);
        self.fps = self.normal_fps;
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
    }

    /// A new viewer path: the old baseline means nothing. fps and bits per
    /// frame are kept.
    pub fn reset_baseline(&mut self) {
        self.rtt.clear();
        self.history.clear();
    }

    /// A new encoded size: bits per frame keep their 1080p-relative level.
    pub fn set_video_size(&mut self, width: u32, height: u32) {
        let pixels = f64::from(width) * f64::from(height);
        let scale = (pixels / FULL_HD_PIXELS).clamp(PIXEL_SCALE_MIN, PIXEL_SCALE_MAX);
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
        (self.outstanding.is_none()
            && self
                .last_probe
                .is_none_or(|last| now.saturating_duration_since(last) >= PROBE_INTERVAL))
        .then_some(self.next_nonce)
    }

    /// When the next probe is due; `None` while one is outstanding.
    pub fn probe_due_at(&self, now: Instant) -> Option<Instant> {
        self.outstanding
            .is_none()
            .then(|| self.last_probe.map_or(now, |last| last + PROBE_INTERVAL))
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
    pub fn on_probe_echo(&mut self, nonce: u64, now: Instant) {
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
        if self.rtt.len() == RTT_WINDOW {
            self.rtt.pop_front();
        }
        self.rtt.push_back(rtt);
        self.tainted = probe.tainted;
        if probe.tainted {
            return;
        }
        if self.history.len() == DELAY_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(rtt);
        let Some(delay) = self.delay_ms() else {
            return;
        };
        self.control_fps(delay);
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
        self.rtt.iter().min().copied()
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

    /// The mean of the recent untainted RTTs above the baseline.
    fn delay_ms(&self) -> Option<u32> {
        let base = self.base_rtt()?;
        if self.history.is_empty() {
            return None;
        }
        let mean = self.history.iter().sum::<Duration>() / self.history.len() as u32;
        Some(
            mean.saturating_sub(base)
                .as_millis()
                .min(u128::from(u32::MAX)) as u32,
        )
    }

    /// RustDesk's `user_network_delay`, with its steps scaled to the ceiling.
    fn control_fps(&mut self, delay: u32) {
        let small = (self.scale.round() as u32).max(1);
        let quick_step = (5.0 * self.scale).round() as u32;
        let (min_fps, before) = (self.min_fps, self.fps);
        let mut fps = self.fps;
        match delay {
            0..50 => {
                self.quick += 1;
                let mut step = if fps < self.normal_fps { small } else { 0 };
                if self.quick >= 3 {
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
            100..150 => fps = min_fps.max(fps),
            _ => {
                let divided = (fps * 150).div_ceil(delay);
                fps = match delay {
                    150..200 => min_fps.max(divided),
                    200..300 => min_fps.min(divided),
                    300..600 => 150 * min_fps / delay,
                    _ => (150 * min_fps / delay).min(divided),
                };
            }
        }
        if delay < 150 {
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
        // No untainted sample in the window: hold.
        if let Some(max_delay) = self.window.max_delay {
            self.step_bpf(max_delay, dynamic, now);
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

    /// One probe round trip of `rtt_ms`, sent at `at`.
    fn sample(qos: &mut Qos, at: Instant, rtt_ms: u64, tainted: bool) {
        let nonce = qos.probe_due(at).unwrap_or(qos.next_nonce);
        qos.probe_sent(nonce, at, tainted);
        qos.on_probe_echo(nonce, at + Duration::from_millis(rtt_ms));
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
    fn delay_controller_follows_rustdesk_steps() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut qos = Qos::new(limits(60), VideoCodec::H264, QualityPreset::Balanced, t0);
        // Balanced at 60: start at 45 fps, capped at 15 × 2 for the first second.
        assert_eq!((qos.fps, qos.pace(t0)), (45, 30));
        assert_eq!(qos.pace(t0 + Duration::from_millis(999)), 30);
        assert_eq!(qos.pace(s(1)), 45);

        // A flat 20 ms link ramps to the ceiling in scaled steps: +10 every
        // third sample and +2 for every three under 150 ms.
        let mut fps = vec![];
        for n in 1..=6 {
            sample(&mut qos, s(n), 20, false);
            fps.push(qos.fps);
        }
        assert_eq!(fps, [45, 45, 57, 57, 57, 60]);
        assert_eq!(qos.pace(s(7)), 60);

        // Delays of 150 ms and more divide fps; RustDesk's bands as given.
        qos.control_fps(180);
        assert_eq!(qos.fps, 50);
        qos.control_fps(250);
        assert_eq!(qos.fps, 10);
        qos.fps = 60;
        qos.control_fps(300);
        assert_eq!(qos.fps, 5);
        qos.fps = 60;
        qos.control_fps(600);
        assert_eq!(qos.fps, 2);
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
        qos.on_probe_echo(nonce, s(13));
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
        qos.on_probe_echo(99, s(14));
        assert!(qos.unresponsive);

        // The baseline is the minimum of the last 60 samples, and a new path
        // resets it.
        let mut qos = Qos::new(limits(60), VideoCodec::H264, QualityPreset::Balanced, t0);
        sample(&mut qos, s(1), 5, false);
        assert_eq!(qos.base_rtt(), Some(DELAY_FLOOR));
        for n in 0..RTT_WINDOW as u64 {
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
        let jpeg = Qos::new(limits(60), VideoCodec::Jpeg, QualityPreset::Smooth, t0);
        assert_eq!(jpeg.pace(s(2)), JPEG_MAX_FPS);
    }

    #[test]
    fn bits_per_frame_steps_and_one_way_latch() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut qos = Qos::new(limits(60), VideoCodec::H264, QualityPreset::Balanced, t0);
        // 4 Mbit/s at 60 fps; the per-frame-budget encoder gets bpf × nominal.
        assert_eq!(qos.bpf, 66_667.0);
        assert_eq!(qos.targets(s(2)).encoder_bps, 4_000_020);
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
        qos.control_fps(250);
        qos.tick(s(12), &idle(), produced(200, 0, 0));
        assert!((qos.bpf - held * 0.90).abs() < 1e-6);
        // The preset floor and max bound every step.
        for n in 0..40 {
            qos.control_fps(900);
            qos.tick(s(15 + 3 * n), &idle(), produced(200, 0, 0));
        }
        assert_eq!(qos.bpf, 16_667.0);
        let mut unique = 200;
        for n in 0..400 {
            unique += 100;
            qos.control_fps(0);
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
        let mut qos = Qos::new(limits(60), VideoCodec::H264, QualityPreset::Balanced, t0);
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
        let mut qos = Qos::new(limits(60), VideoCodec::H264, QualityPreset::Balanced, t0);
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
