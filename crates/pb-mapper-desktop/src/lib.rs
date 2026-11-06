//! Endpoint-owned desktop sharing, carried by an existing pb-mapper deployment.
//! Hosts explicitly grant view access through a secret invitation. Control is
//! separately enabled at the host and requested by the viewer. TLS 1.3 pins the
//! host certificate from that invitation before sending its bearer capability.
//! The native Windows backend captures the ordinary desktop. An explicitly
//! installed, authorized helper handles UAC/Winlogon on its bound desktop.

mod congestion;
#[cfg(windows)]
pub mod helper;
mod host;
mod media;
mod platform;
mod scheduler;
mod security;
#[cfg(test)]
mod slow_link;
#[cfg(test)]
mod tests;
mod viewer;
mod wire;
mod worker;
#[cfg(windows)]
pub mod video {
    pub mod windows;
}

use serde::{Deserialize, Serialize};
use std::{io, sync::Arc};

pub use host::{Host, HostOptions};
pub use platform::{capabilities, displays, native_backend};
pub use security::HostIdentity;
pub use viewer::Viewer;

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

/// Deliver privately to a specific trusted controller. Possession grants view
/// access until the host stops. Never log, publish, or put it in a service name.
/// Relay credentials are deliberately absent; configure each endpoint normally.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub version: u32,
    pub service: String,
    pub certificate: String,
    pub token: String,
}
impl std::fmt::Debug for Invitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invitation")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}
impl Invitation {
    pub fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(error)
    }
    pub fn decode(text: &str) -> Result<Self> {
        if text.len() > 8192 {
            return Err(error("invitation too large"));
        }
        let value: Self =
            serde_json::from_str(text).map_err(|_| error("invalid desktop invitation"))?;
        if value.version != 1
            || !valid_service(&value.service)
            || value.token.len() != 43
            || value.certificate.len() > 4096
        {
            return Err(error("invalid desktop invitation"));
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionStatus {
    pub state: String,
    pub message: String,
    pub control_allowed: bool,
    pub controlling: bool,
    pub generation: u64,
    pub frame_id: u64,
    pub display: Option<Display>,
    pub path: String,
    /// Local transport observation, never trusted as host authorization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_failure: Option<String>,
    pub frames: u64,
    pub bytes: u64,
    #[serde(default)]
    pub desktop: String,
    #[serde(default)]
    pub video: VideoFormat,
    #[serde(default)]
    pub capture_fps: f32,
    #[serde(default)]
    pub receive_fps: f32,
    #[serde(default)]
    pub dropped_frames: u64,
    #[serde(default)]
    pub bitrate: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<CursorPosition>,
    /// Host frame-ready to renderer acknowledgment round trip, including
    /// transport, presentation and acknowledgment coalescing. Not one-way latency.
    /// Zero means no current-generation renderer acknowledgment has been measured.
    #[serde(default)]
    pub frame_ack_ms: f32,
    /// Optional transport-only startup probe. Older viewers ignore it; newer
    /// viewers echo it before any decoder/UI work. It is never a picture ACK.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roundtrip_probe: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    #[default]
    Jpeg,
    H264,
}

/// Renderer limits for a session. Encoded dimensions preserve the display's
/// aspect ratio and never upscale; `Display` remains in physical desktop pixels
/// for input and pointer metadata. Bitrate is a ceiling and may adapt downward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoPreferences {
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub bitrate: u32,
}
impl Default for VideoPreferences {
    fn default() -> Self {
        Self {
            max_width: 1920,
            max_height: 1080,
            max_fps: 90,
            bitrate: 12_000_000,
        }
    }
}
impl VideoPreferences {
    /// Reject invalid limits before opening a session or creating GPU resources.
    pub fn validate(&self) -> Result<()> {
        if !(2..=3840).contains(&self.max_width)
            || !(2..=2160).contains(&self.max_height)
            || !(30..=120).contains(&self.max_fps)
            || !(1_000_000..=40_000_000).contains(&self.bitrate)
        {
            return Err(error("video preferences outside supported bounds"));
        }
        Ok(())
    }
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
    pub desktop: String,
}

/// An owned encoded access unit plus geometry. Bridges copy or retain it, never a
/// borrowed pointer to a buffer a capture worker may overwrite.
#[derive(Clone)]
pub struct Frame {
    pub status: SessionStatus,
    pub bytes: Arc<[u8]>,
}

/// Deliberately small, platform-independent input vocabulary. Text is Unicode
/// insertion, not an IME session; key events are for navigation and shortcuts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Move { x: u32, y: u32 },
    Button { button: Button, down: bool },
    Wheel { horizontal: i32, vertical: i32 },
    Key { key: Key, down: bool },
    Text { text: String },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
}
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
    A,
    C,
    V,
    X,
    Z,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
pub trait Backend: 'static {
    /// Called once before a session captures frames. Backends must keep input
    /// coordinates in physical desktop pixels when scaling their video output.
    fn configure_video(&mut self, preferences: VideoPreferences) -> Result<()> {
        preferences.validate()
    }
    fn display(&mut self) -> Result<Display>;
    fn context(&mut self) -> Result<DesktopContext> {
        Ok(DesktopContext {
            display: self.display()?,
            generation: 0,
            desktop: "ordinary".into(),
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>>;
    /// Report a pointer for the expected desktop, including when no video frame
    /// changed. A desktop transition must fail instead of reporting another
    /// desktop's pointer. Backends without pointer metadata return `None`.
    fn cursor(&mut self, _expected: &DesktopContext) -> Result<Option<CursorPosition>> {
        Ok(None)
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        _codec: VideoCodec,
        _keyframe: bool,
        _bitrate: u32,
    ) -> Result<Option<EncodedFrame>> {
        Ok(Some(EncodedFrame {
            format: VideoFormat {
                codec: VideoCodec::Jpeg,
                width: expected.display.width,
                height: expected.display.height,
                keyframe: true,
                pts_us: 0,
                duplicate: false,
            },
            bytes: self.capture_consistent(expected)?,
        }))
    }
    /// Retrieve already submitted asynchronous encoder output without capturing
    /// or submitting another image. Backpressure pauses production, not codec
    /// completion; synchronous backends have nothing to drain.
    fn drain_video(&mut self, _expected: &DesktopContext) -> Result<Option<EncodedFrame>> {
        Ok(None)
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
