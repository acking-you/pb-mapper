//! Endpoint-owned desktop sharing, carried by an existing pb-mapper deployment.
//! Hosts explicitly grant view access through a secret invitation. Control is
//! separately enabled at the host and requested by the viewer. TLS 1.3 pins the
//! host certificate from that invitation before sending its bearer capability.
//! The native Windows backend captures the ordinary desktop. An explicitly
//! installed, authorized helper handles UAC/Winlogon on its bound desktop.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod auth;
mod backend;
// Neutral: only the Windows encoder and injector use them until another
// platform lands.
#[cfg_attr(not(windows), allow(dead_code))]
mod codec;
mod credit;
mod host;
mod idr;
mod input;
mod invite;
mod lane;
mod launch;
mod mapping;
mod modes;
mod native;
// Only the Windows helper speaks it until another platform has an agent.
#[cfg_attr(not(windows), allow(dead_code))]
mod oop;
mod outgoing;
mod pacer;
mod platform;
pub mod privileged;
mod protocol;
mod qos;
mod scheduler;
mod security;
mod session;
mod shared;
#[cfg(test)]
mod slow_link;
pub mod supervisor;
#[cfg(test)]
mod tests;
mod timer;
pub mod trust;
mod video_sink;
mod viewer;
mod watch;
mod wire;
mod worker;

use serde::{Deserialize, Serialize};
use std::io;

pub use backend::{
    Backend, BackendCaps, BackendError, CursorMode, DRAIN_POLL, Description, FailureKind, HostOs,
    Waker, failure_kind,
};
pub use host::{Access, Custom, DisplaySelection, Host, HostBuilder, HostOptions, Native};
pub use invite::{Invitation, InvitationError, InvitationFormat};
pub use modes::{DisplayModeError, Mode, ModeList, ModeTarget};
pub use native::{NativeBackend, native_backend};
pub use platform::{HostSupport, UnsupportedReason, displays, host_support, recover_display_modes};
pub use protocol::{
    Adaptation, DesktopKind, NoticeCode, RateModel, RejectReason, Route, SessionState, StateReason,
    StopReason, VideoSize,
};
pub use security::HostIdentity;
pub use video_sink::{AttachError, Pull, VideoConsumer, VideoSink, VideoUnit};
pub use viewer::{
    ConnectError, ConnectStage, DecoderInfo, DecoderState, Decoders, DesktopView, InputError,
    IntervalStats, NoticeInfo, PresentedInfo, VideoInfo, Viewer, ViewerConfig, ViewerLink,
    ViewerReason, ViewerState, ViewerStats, ViewerStatus,
};

/// What only a Windows build has.
#[cfg(windows)]
#[cfg_attr(docsrs, doc(cfg(windows)))]
pub mod windows {
    /// The fused encoder itself, for benches.
    #[doc(hidden)]
    pub use crate::platform::windows::video;
    pub use crate::platform::windows::{dpapi, mf::media_foundation};
}

/// Embedding applications coordinate their other input writers here. Merely
/// viewing a desktop never acquires this lease. Drop releases write admission
/// on end-control, revocation, disconnect, or local stop.
pub trait ControlAdmission: Send + Sync + 'static {
    fn acquire(&self) -> Result<Box<dyn ControlLease>>;
}
pub trait ControlLease: 'static {}
impl<T: 'static> ControlLease for T {}

pub type Result<T> = io::Result<T>;
pub(crate) fn error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
pub(crate) fn valid_service(service: &str) -> bool {
    service.starts_with("desktop:")
        && service.len() > 8
        && service.len() <= 128
        && service
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Display {
    pub id: u32,
    pub name: String,
    /// Physical desktop pixels; coordinates may be negative on secondary displays.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Generic pointer hotspot in physical pixels relative to the selected display.
/// This is position/visibility metadata, not the operating system's cursor image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPosition {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    #[default]
    Jpeg,
    H264,
    /// H.265 Main, 4:2:0. Never in a viewer's `codecs`: an older host
    /// rejects a value it does not know. A viewer that can decode it says so
    /// with the `h265` capability, and only a host that can encode it answers
    /// with it.
    H265,
}

/// What a viewer asks the host to favour. Each preset maps to bitrate and
/// fps targets on the host; changing it mid-session never reconnects. A
/// viewer listing `fps_override` makes its `limits.max_fps` every preset's
/// ceiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreset {
    Quality,
    #[default]
    Balanced,
    Smooth,
}

/// Renderer limits, fixed for the session. Encoded dimensions preserve the
/// display's aspect ratio and never upscale; `Display` remains in physical
/// desktop pixels for input and pointer metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoLimits {
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}
impl VideoLimits {
    /// Reject invalid limits before opening a session or creating GPU resources.
    pub fn validate(&self) -> Result<()> {
        if !(2..=3840).contains(&self.max_width)
            || !(2..=2160).contains(&self.max_height)
            || !(30..=120).contains(&self.max_fps)
        {
            return Err(error("video limits outside supported bounds"));
        }
        Ok(())
    }
    /// A viewer's limits by default: the display's own size, up to
    /// 3840x2160, at the display's refresh rate.
    pub fn for_viewer(refresh_hz: f64) -> Self {
        Self::choose(refresh_hz, Resolution::default(), None)
    }
    /// The one place a viewer's limits are derived: `resolution`'s bounds, at
    /// `max_fps` when its user chose one (30..=120), else at the display's
    /// refresh rate, at most 120 fps. Android then asks for 60: the mode a
    /// phone reports at connect may be a power-saving 30 or 48 Hz, and its
    /// presenter asks for the highest mode while the viewer shows. A refresh
    /// rate that is not finite and positive counts as 60 Hz.
    pub fn choose(refresh_hz: f64, resolution: Resolution, max_fps: Option<u32>) -> Self {
        let fps = if let Some(fps) = max_fps {
            fps.clamp(30, 120)
        } else if cfg!(target_os = "android") {
            60
        } else if refresh_hz.is_finite() && refresh_hz > 0.0 {
            refresh_hz.round().clamp(30.0, 120.0) as u32
        } else {
            60
        };
        let (max_width, max_height) = resolution.bounds();
        Self {
            max_width,
            max_height,
            max_fps: fps,
        }
    }
    /// These bounds fitted inside a decoder's largest picture, in either
    /// orientation: the long edge within the longer of `max`'s sides, the
    /// short edge within the shorter. Never below 2x2.
    pub fn clamped(self, max: (u32, u32)) -> Self {
        let (long, short) = (max.0.max(max.1), max.0.min(max.1));
        let (max_width, max_height) = if self.max_width >= self.max_height {
            (self.max_width.min(long), self.max_height.min(short))
        } else {
            (self.max_width.min(short), self.max_height.min(long))
        };
        Self {
            max_width: max_width.max(2),
            max_height: max_height.max(2),
            ..self
        }
    }
}

/// The largest picture a viewer asks for. The host keeps the display's
/// aspect ratio inside these bounds and never upscales, so a smaller display
/// is sent at its own size whatever is chosen. The bounds are landscape: a
/// portrait display is fitted into them as well, and a display larger than
/// 3840x2160 into 4K.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    #[serde(rename = "720p")]
    P720,
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "1440p")]
    P1440,
    /// The display's own size, up to 3840x2160: the default.
    #[default]
    #[serde(rename = "native")]
    Native,
}

impl Resolution {
    /// Its bounds, width by height.
    pub fn bounds(self) -> (u32, u32) {
        match self {
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
            Self::P1440 => (2560, 1440),
            Self::Native => (3840, 2160),
        }
    }
}

/// Why a viewer asks for an independently decodable picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeReason {
    DecoderError,
    Gap,
    Backlog,
    Stall,
    User,
}

/// The encoder rate for one capture call: the QoS bitrate and the pace the
/// worker captures at. Neither change ever rebuilds the encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoRate {
    pub bitrate: u32,
    pub pace_fps: u32,
}

/// The OS double-click time and half the double-click box, read in the
/// interactive session. A press that starts a drag waits this long after a
/// click so the two never merge into a double click.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoubleClickGuard {
    pub time_ms: u32,
    pub half_box: (u32, u32),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VideoFormat {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    pub pts_us: u64,
    #[serde(default)]
    pub duplicate: bool,
    /// Encoder submit to output, for the host's `host_us`. Zero when unknown.
    #[serde(default)]
    pub encode_us: u32,
    /// The GOP length the encoder read back when it was built, for the host's
    /// `gop`. Zero when unknown (JPEG, or an encoder that reports none). It
    /// rides with each unit, so it crosses the helper IPC as the unit does.
    #[serde(default)]
    pub gop: u32,
}

pub struct EncodedFrame {
    pub format: VideoFormat,
    pub bytes: Vec<u8>,
}

/// The capture backend's desktop identity, independent of display resolution.
/// Every switch into/out of a secure desktop must advance `generation`, even
/// when the same monitor and desktop name are reused.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopContext {
    pub display: Display,
    pub generation: u64,
    pub desktop: DesktopKind,
}

/// Deliberately small, platform-independent input vocabulary. Text is Unicode
/// insertion, not an IME session; key events are for navigation and shortcuts.
/// Every variant is a struct variant: serde ignores extra fields on internally
/// tagged unit variants, which would undo `deny_unknown_fields`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Move {
        x: u32,
        y: u32,
    },
    Button {
        button: Button,
        down: bool,
        /// A press that starts a drag. The host only delays it past the OS
        /// double-click time after a click; it authorises nothing.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        drag_start: bool,
    },
    Wheel {
        horizontal: i32,
        vertical: i32,
    },
    Key {
        key: Key,
        down: bool,
    },
    Text {
        text: String,
    },
    /// Lock the workstation in the interactive session. Control-gated
    /// exactly like every other input.
    Lock {},
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
}
/// Keys for navigation, shortcuts and the modifier bar. Letters and
/// digits are virtual keys, never characters, so a shortcut works on any host
/// keyboard layout; text goes through `Input::Text`. `Meta` is the Windows key.
/// Ctrl+Alt+Del is deliberately unrepresentable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Shift,
    Control,
    Alt,
    Meta,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Space,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputEvent {
    pub generation: u64,
    pub frame_id: u64,
    pub sequence: u64,
    pub input: Input,
}

impl Input {
    pub(crate) fn validate(&self, display: &Display) -> Result<()> {
        match self {
            Self::Move { x, y } if *x >= display.width || *y >= display.height => {
                Err(error("pointer outside display"))
            }
            Self::Text { text } if text.len() > 1024 || text.contains('\0') => {
                Err(error("text input too large or invalid"))
            }
            Self::Wheel {
                horizontal,
                vertical,
            } if horizontal.unsigned_abs() > 100 || vertical.unsigned_abs() > 100 => {
                Err(error("scroll input too large"))
            }
            _ => Ok(()),
        }
    }
}

/// The host's own view of its session, polled by the embedding app.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostStatus {
    pub state: HostState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StateReason>,
    pub message: String,
    pub control_allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<HostViewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Display>,
    pub desktop: DesktopKind,
    pub capabilities: Vec<String>,
    /// The display's modes while a viewer has it changed: what the host's
    /// owner may put back with `Host::restore_display_mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_mode: Option<ModeList>,
}
/// The connected viewer, as the host sees it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostViewer {
    /// Time since the session was accepted, on the host's monotonic clock.
    pub since_ms: u64,
    /// When the session was accepted, for display only.
    pub started_unix_ms: u64,
    pub path: HostPath,
    pub controlling: bool,
    pub paused: bool,
    pub quality: QualityPreset,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostState {
    Waiting,
    Connecting,
    Streaming,
    Paused,
    CaptureUnavailable,
    Stopped,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPath {
    Direct,
    Relay,
    Unknown,
}
