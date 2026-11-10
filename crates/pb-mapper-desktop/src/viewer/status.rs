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
    Display, HostOs, QualityPreset, VideoCodec,
    modes::ModeList,
    protocol::{Adaptation, DesktopKind, NoticeCode, RateModel, StateReason, StopReason},
};
use pb_mapper_client::sdk::{
    AddressFamily, DirectTransport, LanEvidence, LinkKind, LinkSample, LinkVia,
};
use serde::Serialize;
use std::time::Duration;

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
    /// What the app last asked [`super::Viewer::request_control`] for on this
    /// viewer, `None` before it asked. A pause, input congestion, the host
    /// denying control or the session ending leaves it as it was: control is
    /// wanted but not held until asked again.
    pub control_wanted: Option<bool>,
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
    /// The host display's modes, from a host that offers them: what
    /// `Viewer::set_display_mode` may ask for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modes: Option<ModeList>,
    /// The host's operating system, where it says.
    pub host_os: Option<HostOs>,
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
    /// The notice's number in this session: a notice repeated word for word
    /// is a new one.
    pub seq: u64,
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

#[derive(Clone, Debug, Default, Serialize)]
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
    pub link: Option<ViewerLink>,
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

/// The direct link a session runs on, as `ViewerStats::link` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ViewerLink {
    /// `lan`, `internet` or `private`.
    pub kind: LinkKind,
    pub family: AddressFamily,
    /// How the peer address was learned.
    pub via: LinkVia,
    pub peer: String,
    pub local_ip: Option<String>,
    /// QUIC, or TCP on a LAN when this viewer's `ConnectOptions` asked for
    /// it and the host let TCP in.
    pub transport: DirectTransport,
    /// QUIC's round trip and its lowest, rounded up to whole milliseconds:
    /// the network alone, unlike `ViewerStats::rtt_ms`, which the host's
    /// probe measures behind the video. TCP keeps its figures in the
    /// kernel, so over TCP both are the handshake's round trip, measured
    /// once at setup.
    pub rtt_ms: u32,
    pub min_rtt_ms: u32,
    pub setup_ms: u32,
    /// What suggested a shared network before the setup.
    pub same_lan: Option<LanEvidence>,
}

impl From<&LinkSample> for ViewerLink {
    fn from(link: &LinkSample) -> Self {
        let ms = |us: u64| u32::try_from(us.div_ceil(1000)).unwrap_or(u32::MAX);
        Self {
            kind: link.kind,
            family: link.family,
            via: link.via,
            peer: link.peer.to_string(),
            local_ip: link.local_ip.map(|ip| ip.to_string()),
            transport: link.transport,
            rtt_ms: ms(link.stats.rtt_us),
            min_rtt_ms: ms(link.stats.min_rtt_us),
            setup_ms: u32::try_from(link.setup_ms).unwrap_or(u32::MAX),
            same_lan: link.same_lan,
        }
    }
}

pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
