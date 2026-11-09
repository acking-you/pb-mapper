//! What a viewer reports: `ViewerStatus` for polls, `DesktopView` per vsync,
//! and the metrics behind them. The overlay, the acceptance report and the
//! docs all read these definitions:
//!
//! - `rtt_ms`: the last probe round trip, transport only (host `Stats`).
//! - `queue_delay_ms`: the median of the host's last untainted probe round
//!   trips minus its baseline.
//! - `picture_latency_ms`: `host_ms + rtt_ms / 2 + viewer_ms`, each a median
//!   over 2 s. `host_ms` is encoder submit to socket write on the host;
//!   `viewer_ms` is network receipt to presentation here. Each side uses its
//!   own clock, and the network leg is an estimate.
//! - `presented_fps`: accepted presentations per second.
//! - `received_fps`: non-duplicate `Video` records per second.
//! - `present_interval`: intervals between accepted presentations.
//! - `speed_rx_bps`, `speed_tx_bps`: the session's whole transfer rate each
//!   way, in bytes per second over the last second, as RustDesk's Speed:
//!   what its tunnel carried, TLS and every message included. A session over
//!   a test stream has no tunnel: it reports the video payload it received,
//!   and nothing sent.
use super::ConnectStage;
use crate::{
    Display, QualityPreset, VideoCodec,
    protocol::{Adaptation, DesktopKind, NoticeCode, RateModel, StateReason, StopReason},
};
use serde::Serialize;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewerState {
    Connecting,
    Streaming,
    Paused,
    CaptureUnavailable,
    Stopped,
}

/// Why the viewer is not simply streaming, or why it stopped. The UI keys off
/// this, never off `message`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewerReason {
    SecureDesktop,
    HelperUnavailable,
    DisplayLost,
    CaptureFailed,
    InputFailed,
    ControlDenied,
    InputCongestion,
    StalePresentation,
    /// The host's display is changing mode; the picture returns at its new
    /// size.
    DisplayChanging,
    HostStopped,
    HostEnded,
    Restarting,
    Revoked,
    Transport,
    Protocol,
    Timeout,
    DecoderFailed,
}
impl From<StateReason> for ViewerReason {
    fn from(reason: StateReason) -> Self {
        match reason {
            StateReason::SecureDesktop => Self::SecureDesktop,
            StateReason::HelperUnavailable => Self::HelperUnavailable,
            StateReason::CaptureFailed => Self::CaptureFailed,
            StateReason::InputFailed => Self::InputFailed,
            StateReason::ControlDenied => Self::ControlDenied,
            StateReason::InputCongestion => Self::InputCongestion,
            StateReason::StalePresentation => Self::StalePresentation,
            StateReason::DisplayLost => Self::DisplayLost,
            StateReason::DisplayChanging => Self::DisplayChanging,
        }
    }
}
impl From<StopReason> for ViewerReason {
    fn from(reason: StopReason) -> Self {
        match reason {
            StopReason::HostStopped => Self::HostStopped,
            StopReason::HostEnded => Self::HostEnded,
            StopReason::Restarting => Self::Restarting,
            StopReason::Revoked => Self::Revoked,
            StopReason::CaptureFailed => Self::CaptureFailed,
            StopReason::ProtocolError => Self::Protocol,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ViewerStatus {
    pub state: ViewerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<ConnectStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<ViewerReason>,
    pub message: String,
    pub control_allowed: bool,
    pub controlling: bool,
    /// A current, recently presented frame: input is possible.
    pub fresh: bool,
    pub paused: bool,
    pub generation: u64,
    /// Physical pixels of the host display: the input space.
    pub display: Option<Display>,
    pub desktop: DesktopKind,
    pub video: Option<VideoInfo>,
    pub presented: Option<PresentedInfo>,
    /// Time without video while it was due: since the later of the last
    /// `Video` record, of any generation, and the session last becoming
    /// streaming. `None` before the first record.
    pub last_video_age_ms: Option<u64>,
    pub quality: QualityPreset,
    /// What the host advertised.
    pub capabilities: Vec<String>,
    pub notice: Option<NoticeInfo>,
    pub decoder: DecoderInfo,
    pub stats: ViewerStats,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct VideoInfo {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PresentedInfo {
    pub generation: u64,
    pub frame_id: u64,
    pub age_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NoticeInfo {
    pub code: NoticeCode,
    pub message: String,
    pub age_ms: u64,
}

/// The decoder the native consumer reported, and how the viewer's recovery
/// sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DecoderInfo {
    pub name: String,
    /// `None` until the consumer reports.
    pub hardware: Option<bool>,
    pub state: DecoderState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecoderState {
    #[default]
    Ok,
    Recovering,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct ViewerStats {
    /// `direct`, `relay`, or empty before the first sample.
    pub path: String,
    /// `tcp` or `quic` on the relay path.
    pub relay_transport: Option<String>,
    pub rtt_ms: Option<u32>,
    pub base_rtt_ms: Option<u32>,
    pub queue_delay_ms: Option<u32>,
    pub picture_latency_ms: Option<u32>,
    pub host_ms: Option<u32>,
    pub viewer_ms: Option<u32>,
    pub presented_fps: f32,
    pub received_fps: f32,
    pub capture_fps: f32,
    pub pace_fps: u32,
    pub target_fps: u32,
    /// 0 = none.
    pub fps_cap: u32,
    /// The preset this session asked the host for.
    pub preset: QualityPreset,
    /// The session's `limits.max_fps`, a ceiling on every preset's.
    pub max_fps: u32,
    /// Host capture ticks skipped per second, for transport backpressure or
    /// a full encoder.
    pub skipped_per_s: f32,
    /// The host's median time a unit stays in its encoder, from submit to
    /// output. `None` from a host that reports none.
    pub encoder_dwell_ms: Option<u32>,
    /// What the host sent, in bits per second.
    pub sent_bitrate: u32,
    /// The host's receipt-based queue delay.
    pub ack_queue_ms: Option<u32>,
    /// What the host expects to send: bits per frame × pace.
    pub bitrate: u32,
    pub encoder_bitrate: u32,
    /// The video payload received, in bits per second.
    pub received_bitrate: u32,
    /// Bytes per second the session received, and sent; see the module docs.
    pub speed_rx_bps: u64,
    pub speed_tx_bps: u64,
    pub width: u32,
    pub height: u32,
    pub codec: VideoCodec,
    pub adaptation: Adaptation,
    pub rate_model: RateModel,
    pub keyframes: u64,
    pub keyframes_requested: u64,
    pub backlog_drops: u64,
    pub dropped_frames: u64,
    pub present_interval: IntervalStats,
    /// Queue delay, picture latency or presentation rate has been poor for 3 s.
    pub degraded: bool,
    /// The direct link under `path`, sampled once a second; `None` on the
    /// relay.
    pub link: Option<super::ViewerLink>,
}
impl Default for ViewerStats {
    fn default() -> Self {
        Self {
            path: String::new(),
            relay_transport: None,
            rtt_ms: None,
            base_rtt_ms: None,
            queue_delay_ms: None,
            picture_latency_ms: None,
            host_ms: None,
            viewer_ms: None,
            presented_fps: 0.0,
            received_fps: 0.0,
            capture_fps: 0.0,
            pace_fps: 0,
            target_fps: 0,
            fps_cap: 0,
            preset: QualityPreset::default(),
            max_fps: 0,
            skipped_per_s: 0.0,
            encoder_dwell_ms: None,
            sent_bitrate: 0,
            ack_queue_ms: None,
            bitrate: 0,
            encoder_bitrate: 0,
            received_bitrate: 0,
            speed_rx_bps: 0,
            speed_tx_bps: 0,
            width: 0,
            height: 0,
            codec: VideoCodec::default(),
            adaptation: Adaptation::Startup,
            rate_model: RateModel::Nominal,
            keyframes: 0,
            keyframes_requested: 0,
            backlog_drops: 0,
            dropped_frames: 0,
            present_interval: IntervalStats::default(),
            degraded: false,
            link: None,
        }
    }
}

/// Intervals between accepted presentations. The percentiles and the maximum
/// cover the last 60 s; the gap counts cover the whole session. A percentile
/// is the upper bound of its histogram bucket, never above the maximum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct IntervalStats {
    pub p50_ms: u32,
    pub p95_ms: u32,
    pub p99_ms: u32,
    pub max_ms: u32,
    /// Intervals longer than 100 ms.
    pub gaps_100: u64,
    /// Intervals longer than 250 ms.
    pub gaps_250: u64,
}

/// The per-vsync view: the Rust mirror of the FFI's 56-byte
/// `PbDesktopViewV1`, filled without allocating.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DesktopView {
    pub generation: u64,
    /// 0 = none.
    pub presented_frame_id: u64,
    /// Advances with every new cursor position.
    pub cursor_seq: u64,
    /// `u32::MAX` = none.
    pub presented_age_ms: u32,
    /// Physical pixels of `generation`: the input space.
    pub display_width: u32,
    pub display_height: u32,
    /// The encoded size.
    pub video_width: u32,
    pub video_height: u32,
    /// Physical pixels relative to the display.
    pub cursor_x: i32,
    pub cursor_y: i32,
    /// One of the `STATE_*` values.
    pub state: u8,
    /// `FLAG_*` bits.
    pub flags: u8,
}
impl DesktopView {
    pub const STATE_NONE: u8 = 0;
    pub const STATE_CONNECTING: u8 = 1;
    pub const STATE_STREAMING: u8 = 2;
    pub const STATE_CAPTURE_UNAVAILABLE: u8 = 3;
    pub const STATE_PAUSED: u8 = 4;
    pub const STATE_STOPPED: u8 = 5;
    pub const FLAG_CURSOR_VISIBLE: u8 = 1 << 0;
    pub const FLAG_CONTROLLING: u8 = 1 << 1;
    pub const FLAG_CONTROL_ALLOWED: u8 = 1 << 2;
    pub const FLAG_SECURE_DESKTOP: u8 = 1 << 3;
    pub const FLAG_FRESH: u8 = 1 << 4;
    pub const FLAG_PAUSED: u8 = 1 << 5;

    pub(crate) fn state_code(state: ViewerState) -> u8 {
        match state {
            ViewerState::Connecting => Self::STATE_CONNECTING,
            ViewerState::Streaming => Self::STATE_STREAMING,
            ViewerState::CaptureUnavailable => Self::STATE_CAPTURE_UNAVAILABLE,
            ViewerState::Paused => Self::STATE_PAUSED,
            ViewerState::Stopped => Self::STATE_STOPPED,
        }
    }
}

pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
fn millis_u32(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

fn prune(samples: &mut VecDeque<(Instant, u32)>, now: Instant, window: Duration) {
    while samples
        .front()
        .is_some_and(|(at, _)| now.saturating_duration_since(*at) > window)
    {
        samples.pop_front();
    }
}

/// The median of the samples taken since `now - window`, dropping older ones.
fn windowed_median(
    samples: &mut VecDeque<(Instant, u32)>,
    now: Instant,
    window: Duration,
) -> Option<u32> {
    prune(samples, now, window);
    let mut values: Vec<u32> = samples.iter().map(|(_, value)| *value).collect();
    let middle = values.len() / 2;
    (!values.is_empty()).then(|| *values.select_nth_unstable(middle).1)
}

/// Latency and rate samples are medians over this window.
const LATENCY_WINDOW: Duration = Duration::from_secs(2);
/// Upper bounds of the presentation interval buckets; the last bucket is open.
const INTERVAL_BOUNDS_MS: [u32; 12] = [8, 12, 17, 20, 25, 34, 50, 67, 100, 150, 250, 500];
const INTERVAL_BUCKETS: usize = INTERVAL_BOUNDS_MS.len() + 1;
const INTERVAL_WINDOW_SECONDS: u64 = 60;

#[derive(Clone, Copy, Default)]
struct IntervalSecond {
    second: u64,
    counts: [u32; INTERVAL_BUCKETS],
    max_ms: u32,
}

/// Accepted presentations: rate, intervals and viewer latency. Owned by the
/// sink, recorded under its lock.
pub(crate) struct PresentStats {
    epoch: Instant,
    last: Option<Instant>,
    /// Presentations in the last second.
    recent: VecDeque<Instant>,
    seconds: [IntervalSecond; INTERVAL_WINDOW_SECONDS as usize],
    gaps_100: u64,
    gaps_250: u64,
    /// Network receipt to presentation, in ms.
    latency: VecDeque<(Instant, u32)>,
}
impl PresentStats {
    pub fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            last: None,
            recent: VecDeque::new(),
            seconds: [IntervalSecond::default(); INTERVAL_WINDOW_SECONDS as usize],
            gaps_100: 0,
            gaps_250: 0,
            latency: VecDeque::new(),
        }
    }

    pub fn record(&mut self, now: Instant, latency: Duration) {
        if let Some(last) = self.last.replace(now) {
            let interval = millis_u32(now.saturating_duration_since(last));
            self.gaps_100 += u64::from(interval > 100);
            self.gaps_250 += u64::from(interval > 250);
            let second = now.saturating_duration_since(self.epoch).as_secs();
            let slot = &mut self.seconds[(second % INTERVAL_WINDOW_SECONDS) as usize];
            if slot.second != second {
                *slot = IntervalSecond {
                    second,
                    ..IntervalSecond::default()
                };
            }
            let bucket = INTERVAL_BOUNDS_MS
                .iter()
                .position(|bound| interval < *bound)
                .unwrap_or(INTERVAL_BOUNDS_MS.len());
            slot.counts[bucket] += 1;
            slot.max_ms = slot.max_ms.max(interval);
        }
        self.recent.push_back(now);
        self.prune_recent(now);
        self.latency.push_back((now, millis_u32(latency)));
        prune(&mut self.latency, now, LATENCY_WINDOW);
    }

    fn prune_recent(&mut self, now: Instant) {
        while self
            .recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= Duration::from_secs(1))
        {
            self.recent.pop_front();
        }
    }

    pub fn presented_fps(&mut self, now: Instant) -> f32 {
        self.prune_recent(now);
        self.recent.len() as f32
    }

    pub fn viewer_ms(&mut self, now: Instant) -> Option<u32> {
        windowed_median(&mut self.latency, now, LATENCY_WINDOW)
    }

    pub fn intervals(&self, now: Instant) -> IntervalStats {
        let current = now.saturating_duration_since(self.epoch).as_secs();
        let mut counts = [0u64; INTERVAL_BUCKETS];
        let mut max_ms = 0;
        for slot in &self.seconds {
            if current.saturating_sub(slot.second) < INTERVAL_WINDOW_SECONDS {
                for (total, count) in counts.iter_mut().zip(slot.counts) {
                    *total += u64::from(count);
                }
                max_ms = max_ms.max(slot.max_ms);
            }
        }
        let total: u64 = counts.iter().sum();
        let percentile = |fraction: f64| {
            if total == 0 {
                return 0;
            }
            let rank = ((total as f64 * fraction).ceil() as u64).max(1);
            let mut seen = 0;
            for (bucket, count) in counts.iter().enumerate() {
                seen += count;
                if seen >= rank {
                    return INTERVAL_BOUNDS_MS
                        .get(bucket)
                        .map_or(max_ms, |bound| (*bound).min(max_ms));
                }
            }
            max_ms
        };
        IntervalStats {
            p50_ms: percentile(0.50),
            p95_ms: percentile(0.95),
            p99_ms: percentile(0.99),
            max_ms,
            gaps_100: self.gaps_100,
            gaps_250: self.gaps_250,
        }
    }
}

/// The newest cursor of the current generation.
#[derive(Clone, Copy, Default)]
pub(crate) struct CursorSlot {
    pub generation: u64,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    /// Advances with every update, so a reader can tell a new position.
    pub seq: u64,
    /// The generation and sequence of the last move written to the host.
    pub moved: Option<(u64, u64)>,
}
impl CursorSlot {
    /// A report of `generation` the host sampled before it took the last move
    /// this viewer wrote in that generation (`input_sequence` below the
    /// move's) is superseded while this viewer controls: the host's pointer has
    /// gone where this viewer put it since, and the host does not report that
    /// echo, so taking the report would leave the cursor where the host's is
    /// not. The first report of a generation is never superseded, nor is one
    /// without a watermark (an older host).
    pub fn superseded(
        &self,
        generation: u64,
        input_sequence: Option<u64>,
        controlling: bool,
    ) -> bool {
        controlling
            && self.generation == generation
            && input_sequence
                .zip(self.moved)
                .is_some_and(|(taken, moved)| moved.0 == generation && taken < moved.1)
    }
}

/// The last host `Stats` message, as the viewer reports it.
#[derive(Clone, Copy)]
pub(crate) struct HostStats {
    pub capture_fps: f32,
    pub pace_fps: u32,
    pub target_fps: u32,
    pub fps_cap: u32,
    pub bitrate: u32,
    pub encoder_bitrate: u32,
    pub sent_bitrate: u32,
    pub rtt_ms: Option<u32>,
    pub base_rtt_ms: Option<u32>,
    pub queue_delay_ms: Option<u32>,
    pub ack_queue_ms: u32,
    pub encoder_dwell_ms: Option<u32>,
    pub dropped_frames: u64,
    pub skipped_ticks: u64,
    pub keyframes: u64,
    pub adaptation: Adaptation,
    pub rate_model: RateModel,
}

/// Bytes a session's tunnel carried so far, each way.
#[derive(Clone, Copy)]
pub(crate) struct Traffic {
    pub sent: u64,
    pub received: u64,
}

/// Received video and the host's last `Stats`. The read task records; the
/// write task's tick rolls the rates up once a second.
pub(crate) struct ViewerStatsAcc {
    pub host: Option<HostStats>,
    /// When the last `Stats` came, or the session started.
    host_at: Instant,
    /// Host ticks skipped per second between the last two `Stats`.
    skipped_per_s: f32,
    max_fps: u32,
    window_start: Instant,
    window_frames: u32,
    window_bytes: u64,
    pub received_fps: f32,
    pub received_bitrate: u32,
    /// The tunnel's traffic at the last roll, and when.
    traffic: Option<(Instant, Traffic)>,
    speed_rx_bps: u64,
    speed_tx_bps: u64,
    /// Encoder submit to socket write on the host, in ms.
    host_ms: VecDeque<(Instant, u32)>,
    pub last_video_at: Option<Instant>,
    pub last_video_size: Option<(u32, u32)>,
    poor_since: Option<Instant>,
    good_since: Option<Instant>,
    pub degraded: bool,
}
/// How long a condition must hold before `degraded` changes.
const DEGRADED_HOLD: Duration = Duration::from_secs(3);
impl ViewerStatsAcc {
    pub fn new(now: Instant, max_fps: u32) -> Self {
        Self {
            host: None,
            host_at: now,
            skipped_per_s: 0.0,
            max_fps,
            window_start: now,
            window_frames: 0,
            window_bytes: 0,
            received_fps: 0.0,
            received_bitrate: 0,
            traffic: None,
            speed_rx_bps: 0,
            speed_tx_bps: 0,
            host_ms: VecDeque::new(),
            last_video_at: None,
            last_video_size: None,
            poor_since: None,
            good_since: None,
            degraded: false,
        }
    }

    pub fn on_video(
        &mut self,
        now: Instant,
        bytes: usize,
        duplicate: bool,
        size: (u32, u32),
        host_us: u32,
    ) {
        self.window_frames += u32::from(!duplicate);
        self.window_bytes += bytes as u64;
        self.last_video_at = Some(now);
        self.last_video_size = Some(size);
        self.host_ms.push_back((now, host_us / 1000));
        prune(&mut self.host_ms, now, LATENCY_WINDOW);
    }

    pub fn host_ms(&mut self, now: Instant) -> Option<u32> {
        windowed_median(&mut self.host_ms, now, LATENCY_WINDOW)
    }

    /// The host's `Stats`, with its skipped ticks turned into a rate since the
    /// previous one, or since the session started. `Stats` carries no host
    /// time, and a link that held one delivers it right before the next, but
    /// the host sends them at least `STATS_INTERVAL` apart: the rate is over
    /// no less, and never above `max_fps`, since a tick skipped is one the
    /// pace had.
    pub fn on_host(&mut self, now: Instant, host: HostStats) {
        let skipped = self.host.map_or(0, |last| last.skipped_ticks);
        let elapsed = now
            .saturating_duration_since(self.host_at)
            .max(crate::session::STATS_INTERVAL)
            .as_secs_f32();
        self.skipped_per_s =
            (host.skipped_ticks.saturating_sub(skipped) as f32 / elapsed).min(self.max_fps as f32);
        self.host = Some(host);
        self.host_at = now;
    }

    /// `host_ms + rtt / 2 + viewer_ms`, once both medians exist.
    pub fn picture_latency(&mut self, now: Instant, viewer_ms: Option<u32>) -> Option<u32> {
        let rtt = self.host.and_then(|host| host.rtt_ms).unwrap_or(0);
        Some(
            self.host_ms(now)?
                .saturating_add(rtt / 2)
                .saturating_add(viewer_ms?),
        )
    }

    /// The host's figures, the received rates, the latency medians and
    /// `degraded`.
    pub fn fill(&mut self, now: Instant, viewer_ms: Option<u32>, stats: &mut ViewerStats) {
        if let Some(host) = self.host {
            stats.rtt_ms = host.rtt_ms;
            stats.base_rtt_ms = host.base_rtt_ms;
            stats.queue_delay_ms = host.queue_delay_ms;
            stats.capture_fps = host.capture_fps;
            stats.pace_fps = host.pace_fps;
            stats.target_fps = host.target_fps;
            stats.fps_cap = host.fps_cap;
            stats.skipped_per_s = self.skipped_per_s;
            stats.encoder_dwell_ms = host.encoder_dwell_ms;
            stats.sent_bitrate = host.sent_bitrate;
            stats.ack_queue_ms = Some(host.ack_queue_ms);
            stats.bitrate = host.bitrate;
            stats.encoder_bitrate = host.encoder_bitrate;
            stats.adaptation = host.adaptation;
            stats.rate_model = host.rate_model;
            stats.keyframes = host.keyframes;
            stats.dropped_frames = host.dropped_frames;
        }
        stats.max_fps = self.max_fps;
        stats.host_ms = self.host_ms(now);
        stats.viewer_ms = viewer_ms;
        stats.picture_latency_ms = self.picture_latency(now, viewer_ms);
        stats.received_fps = self.received_fps;
        stats.received_bitrate = self.received_bitrate;
        stats.speed_rx_bps = self.speed_rx_bps;
        stats.speed_tx_bps = self.speed_tx_bps;
        stats.degraded = self.degraded;
    }

    /// Once a second: the received rates, the speed from the tunnel's
    /// `traffic` (`None` without a tunnel), and `degraded`, which is judged
    /// only while the picture should be moving.
    pub fn roll(
        &mut self,
        now: Instant,
        streaming: bool,
        presented_fps: f32,
        viewer_ms: Option<u32>,
        traffic: Option<Traffic>,
    ) {
        let elapsed = now
            .saturating_duration_since(self.window_start)
            .as_secs_f32();
        if elapsed > 0.0 {
            self.received_fps = self.window_frames as f32 / elapsed;
            self.received_bitrate = (self.window_bytes as f32 * 8.0 / elapsed) as u32;
        }
        match traffic {
            Some(total) => {
                // The first sample is the baseline.
                if let Some((at, last)) = self.traffic.replace((now, total)) {
                    let seconds = now.saturating_duration_since(at).as_secs_f64();
                    if seconds > 0.0 {
                        let rate = |now: u64, last: u64| {
                            (now.saturating_sub(last) as f64 / seconds) as u64
                        };
                        self.speed_rx_bps = rate(total.received, last.received);
                        self.speed_tx_bps = rate(total.sent, last.sent);
                    }
                }
            }
            None if elapsed > 0.0 => {
                self.speed_rx_bps = (self.window_bytes as f64 / f64::from(elapsed)) as u64;
                self.speed_tx_bps = 0;
            }
            None => {}
        }
        self.window_start = now;
        self.window_frames = 0;
        self.window_bytes = 0;
        if !streaming {
            self.poor_since = None;
            self.good_since = None;
            return;
        }
        let latency = self.picture_latency(now, viewer_ms);
        let host = self.host;
        let pace = host.map_or(0, |host| host.pace_fps);
        let poor = host
            .and_then(|host| host.queue_delay_ms)
            .is_some_and(|delay| delay >= 150)
            || latency.is_some_and(|latency| latency >= 250)
            || (pace >= 10 && presented_fps < 0.5 * pace as f32);
        let (since, other) = if poor {
            (&mut self.poor_since, &mut self.good_since)
        } else {
            (&mut self.good_since, &mut self.poor_since)
        };
        *other = None;
        if now.saturating_duration_since(*since.get_or_insert(now)) >= DEGRADED_HOLD {
            self.degraded = poor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speed is what the tunnel carried each way per second; a session with
    /// no tunnel reports the video it received.
    #[test]
    fn speed_follows_the_tunnel_or_else_the_video() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let speed = |acc: &mut ViewerStatsAcc, now| {
            let mut stats = ViewerStats::default();
            acc.fill(now, None, &mut stats);
            (stats.speed_rx_bps, stats.speed_tx_bps)
        };
        let traffic = |sent, received| Some(Traffic { sent, received });

        let mut tunnel = ViewerStatsAcc::new(start, 60);
        tunnel.roll(at(1), true, 0.0, None, traffic(1_000, 50_000));
        assert_eq!(speed(&mut tunnel, at(1)), (0, 0));
        // The tunnel's count already holds the video, and its framing.
        tunnel.on_video(at(2), 20_000, false, (1920, 1080), 0);
        tunnel.roll(at(3), true, 0.0, None, traffic(3_000, 450_000));
        assert_eq!(speed(&mut tunnel, at(3)), (200_000, 1_000));

        let mut test_stream = ViewerStatsAcc::new(start, 60);
        test_stream.on_video(at(1), 300_000, false, (1920, 1080), 0);
        test_stream.roll(at(2), true, 0.0, None, None);
        assert_eq!(speed(&mut test_stream, at(2)), (150_000, 0));
    }
}
