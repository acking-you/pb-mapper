//! Endpoint-owned desktop sharing, carried by an existing pb-mapper deployment.
//! Hosts explicitly grant view access through a secret invitation. Control is
//! separately enabled at the host and requested by the viewer. TLS 1.3 pins the
//! host certificate from that invitation before sending its bearer capability.
//! The native Windows backend captures the ordinary desktop. An explicitly
//! installed, authorized helper handles UAC/Winlogon on its bound desktop.

mod auth;
mod backend;
mod credit;
#[cfg(windows)]
pub mod dpapi;
#[cfg(windows)]
pub mod helper;
mod host;
mod idr;
mod invite;
mod lane;
mod outgoing;
mod pacer;
mod platform;
mod protocol;
mod qos;
mod scheduler;
mod security;
mod session;
mod shared;
#[cfg(test)]
mod slow_link;
#[cfg(test)]
mod tests;
mod timer;
mod video_sink;
mod viewer;
mod wire;
mod worker;
#[cfg(windows)]
pub mod video {
    pub mod windows;
}

use serde::{Deserialize, Serialize};
use std::{io, time::Duration};

pub use backend::{BackendError, FailureKind, failure_kind};
pub use host::{Host, HostOptions};
pub use invite::{Invitation, InvitationError, InvitationFormat};
pub use platform::{capabilities, displays, media_foundation, native_backend};
pub use protocol::{
    Adaptation, DesktopKind, NoticeCode, RateModel, RejectReason, Route, SessionState, StateReason,
    StopReason, VideoSize,
};
pub use security::HostIdentity;
pub use video_sink::{AttachError, Pull, VideoConsumer, VideoSink, VideoUnit};
pub use viewer::{
    ConnectError, ConnectStage, DecoderInfo, DecoderState, DesktopView, InputError, IntervalStats,
    NoticeInfo, PresentedInfo, VideoInfo, Viewer, ViewerConfig, ViewerLink, ViewerReason,
    ViewerState, ViewerStats, ViewerStatus,
};

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub host: bool,
    pub viewer: bool,
    pub secure_desktop: bool,
    pub mode: String,
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
    /// A viewer's limits by default: 1080p at the display's refresh rate.
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
}

/// The largest picture a viewer asks for. The host keeps the display's
/// aspect ratio inside these bounds and never upscales, so a smaller display
/// is sent at its own size whatever is chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    #[serde(rename = "720p")]
    P720,
    #[default]
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "1440p")]
    P1440,
    /// The display's own size, up to 3840x2160.
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

/// A host backend lives only on its capture/input worker. Embedders may provide
/// another backend (including a separately authorized helper) without changing
/// networking, trust, or the viewer. `display` must fail on inaccessible desktops
/// and change geometry on display changes. `release` must release every held key.
/// A sticky, unrecoverable link failure must return `io::ErrorKind::NotConnected`,
/// which ends the session instead of retrying the capture.
pub trait Backend: 'static {
    /// Called once before a session captures frames, and again only for a new
    /// context or display or an encoder recovery; never for an fps, bitrate or
    /// preset change. Backends must keep input coordinates in physical desktop
    /// pixels when scaling their video output.
    fn configure_video(&mut self, limits: VideoLimits, _nominal_fps: u32) -> Result<()> {
        limits.validate()
    }
    fn display(&mut self) -> Result<Display>;
    fn context(&mut self) -> Result<DesktopContext> {
        Ok(DesktopContext {
            display: self.display()?,
            generation: 0,
            desktop: DesktopKind::Ordinary,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>>;
    /// Report a pointer for the expected desktop, including when no video frame
    /// changed, as it stands after every input this backend applied: thread C
    /// takes a report at the position it last injected for the controller's
    /// own move coming back, and never sends it. A desktop transition must
    /// fail instead of reporting another desktop's pointer. Backends without
    /// pointer metadata return `None`.
    fn cursor(&mut self, _expected: &DesktopContext) -> Result<Option<CursorPosition>> {
        Ok(None)
    }
    /// Whether `capture_video` can encode `VideoCodec::H265` on this
    /// backend's display. Asked once a session, right after the backend is
    /// built and before the host answers a viewer that decodes H.265; a
    /// backend probes its GPU once and keeps the answer for its process.
    fn encodes_h265(&mut self) -> bool {
        false
    }
    /// Drain finished output, then (encoder backends) submit at most one
    /// capture. The default captures one JPEG picture. A `keyframe` is asked
    /// for once: an encoder backend keeps it until a picture it submits
    /// carries it, and replaces output it discards with an IDR.
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        _codec: VideoCodec,
        _keyframe: bool,
        _rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        Ok(Some(EncodedFrame {
            format: VideoFormat {
                codec: VideoCodec::Jpeg,
                width: expected.display.width,
                height: expected.display.height,
                keyframe: true,
                ..VideoFormat::default()
            },
            bytes: self.capture_consistent(expected)?,
        }))
    }
    /// Retrieve already submitted asynchronous encoder output without capturing
    /// or submitting another image. `wait` bounds a long-poll (the helper); the
    /// ordinary backend never blocks. Synchronous backends have nothing to drain.
    fn drain_video(
        &mut self,
        _expected: &DesktopContext,
        _wait: Duration,
    ) -> Result<Option<EncodedFrame>> {
        Ok(None)
    }
    /// Units submitted to the encoder and not yet output (the submit gate).
    fn pending_video(&self) -> usize {
        0
    }
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>> {
        if &self.context()? != expected {
            return Err(error("desktop changed before capture"));
        }
        let frame = self.capture()?;
        if &self.context()? != expected {
            return Err(error(
                "desktop changed during capture; waiting for a current frame",
            ));
        }
        Ok(frame)
    }
    fn input(&mut self, event: &Input) -> Result<()>;
    /// Validate the desktop at the final injection boundary. Backends that can
    /// switch desktops must override this without switching again after checking.
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()> {
        if &self.context()? != expected {
            self.release();
            return Err(error("desktop changed before input"));
        }
        event.validate(&expected.display)?;
        self.input(event)
    }
    /// Renew held input only after the controller has presented a current frame.
    fn presented(&mut self, _expected: &DesktopContext) -> Result<()> {
        Ok(())
    }
    fn release(&mut self);
    /// The OS double-click time and box of the interactive session; `None`
    /// means a drag-start press is never held.
    fn double_click_guard(&mut self) -> Option<DoubleClickGuard> {
        None
    }
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
