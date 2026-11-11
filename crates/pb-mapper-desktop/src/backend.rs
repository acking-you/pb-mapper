//! The seam between a host session's thread C and whatever captures and
//! injects for it: [`Backend`], what it can do ([`BackendCaps`]), and why it
//! failed ([`FailureKind`]), as a kind thread C acts on rather than a text it
//! would have to parse. A kind rides inside the `io::Error` every backend call
//! returns, so `Result` stays `io::Result`.

use crate::{
    DesktopContext, DesktopKind, Display, DoubleClickGuard, EncodedFrame, Input, Result,
    VideoCodec, VideoFormat, VideoLimits, VideoRate, error,
    modes::{Mode, ModeList},
};
use serde::{Deserialize, Serialize};
use std::{io, time::Duration};

/// How often thread C polls an encoder that answers only when asked.
pub const DRAIN_POLL: Duration = Duration::from_millis(2);

/// A picture source and input sink for one viewer session. It is built, used
/// and dropped on that session's thread C, and is not `Send`. Embedders may
/// provide their own (a separately authorized helper included) without
/// changing networking, trust or the viewer. `display` must fail on an
/// inaccessible desktop and change geometry on a display change; `release`
/// must release every held key. A sticky, unrecoverable link failure returns
/// `io::ErrorKind::NotConnected`, which ends the session instead of retrying
/// the capture.
pub trait Backend: 'static {
    /// The selected display's geometry now, in physical pixels.
    fn display(&mut self) -> Result<Display>;
    fn context(&mut self) -> Result<DesktopContext> {
        Ok(DesktopContext {
            display: self.display()?,
            generation: 0,
            desktop: DesktopKind::Ordinary,
        })
    }
    /// On thread C, right after the backend is built and before the host
    /// answers the viewer, which waits at most 3 s for both answers: first
    /// with `probe_h265` false, whose display sizes the first IDR and whose
    /// caps go into `Accepted`; then, only when the viewer decodes H.265 and no
    /// H.265 encoder failed in this process, again with true, of which only
    /// `caps.h265` is read. A backend probes its GPU once and keeps the answer
    /// for its process. Default: the context's display, and no capabilities.
    fn describe(&mut self, probe_h265: bool) -> Result<Description> {
        let _ = probe_h265;
        Ok(Description {
            display: self.context()?.display,
            caps: BackendCaps::default(),
        })
    }
    /// The session's wake. A push source keeps it and calls `wake()` when a
    /// unit or a display change is ready; a pull source ignores it. Called
    /// once, before `configure_video`.
    fn bind(&mut self, waker: Waker) {
        let _ = waker;
    }
    /// Called once before a session captures frames, and again only for a new
    /// context or display or an encoder recovery; never for an fps, bitrate or
    /// preset change. Backends must keep input coordinates in physical desktop
    /// pixels when scaling their video output.
    fn configure_video(&mut self, limits: VideoLimits, nominal_fps: u32) -> Result<()> {
        let _ = nominal_fps;
        limits.validate()
    }
    /// One JPEG still. Optional: an encoder the OS fuses with capture has no
    /// CPU pixels.
    fn capture(&mut self) -> Result<Vec<u8>> {
        Err(BackendError::new(FailureKind::Unsupported, "still capture").into())
    }
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>> {
        capture_checked(self, expected, Self::capture)
    }
    /// Drain finished output, then (encoder backends) submit at most one
    /// capture. The default captures one JPEG picture. A `keyframe` is asked
    /// for once: an encoder backend keeps it until a picture it submits
    /// carries it, and replaces output it discards with an IDR.
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        let _ = (codec, keyframe, rate);
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
        expected: &DesktopContext,
        wait: Duration,
    ) -> Result<Option<EncodedFrame>> {
        let _ = (expected, wait);
        Ok(None)
    }
    /// Units submitted to the encoder and not yet output (the submit gate).
    fn pending_video(&self) -> usize {
        0
    }
    /// `Some(interval)`: thread C polls `drain_video` this often while units
    /// are pending. `None`: a push source that wakes thread C itself.
    fn drain_poll(&self) -> Option<Duration> {
        Some(DRAIN_POLL)
    }
    /// Report a pointer for the expected desktop, including when no video frame
    /// changed, as it stands after every input this backend applied: thread C
    /// takes a report at the position it last injected for the controller's
    /// own move coming back, and never sends it. A desktop transition must
    /// fail instead of reporting another desktop's pointer. Backends without
    /// pointer metadata return `None`.
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        let _ = expected;
        Ok(None)
    }
    /// The pointer's shape, for a backend whose caps say `cursor_shapes`.
    /// Thread C asks only in a session whose viewer reads shapes, right
    /// after `cursor` reported a pointer for `expected`, every 16 ms while
    /// it captures. `Some` when the shape may have changed since the last
    /// `Some` this backend returned (the first call: the shape now); `None`
    /// when it is unchanged or unknown, which keeps the last one. Tell a
    /// change by the cheapest identity the OS offers, a cursor handle, never
    /// by reading pixels each time: thread C compares the content itself. A
    /// failure keeps the last shape and does not end capture, except a lost
    /// link (`NotConnected`).
    fn cursor_shape(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorShape>> {
        let _ = expected;
        Ok(None)
    }
    fn input(&mut self, event: &Input) -> Result<()>;
    /// The injection boundary: inject only while the desktop is `expected`.
    /// Backends that can switch desktops override it without switching again
    /// after checking.
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()> {
        if &self.context()? != expected {
            self.release();
            return Err(error("desktop changed before input"));
        }
        event.validate(&expected.display)?;
        self.input(event)
    }
    /// Renew held input only after the controller has presented a current frame.
    fn presented(&mut self, expected: &DesktopContext) -> Result<()> {
        let _ = expected;
        Ok(())
    }
    /// Release every held key and button.
    fn release(&mut self);
    /// The OS double-click time and box of the interactive session; `None`
    /// means a drag-start press is never held.
    fn double_click_guard(&mut self) -> Option<DoubleClickGuard> {
        None
    }
    /// The display's modes; `None` when this backend cannot change modes.
    fn display_modes(&mut self, expected: &DesktopContext) -> Result<Option<ModeList>> {
        let _ = expected;
        Ok(None)
    }
    /// Change the display's mode dynamically: the OS must not persist it.
    fn set_display_mode(&mut self, expected: &DesktopContext, mode: Mode) -> Result<()> {
        let _ = (expected, mode);
        Err(BackendError::new(FailureKind::Unsupported, "display modes").into())
    }
    /// Put back what this backend changed. `Ok(false)`: nothing to restore,
    /// or the owner changed the mode meanwhile, who is never fought.
    /// Idempotent.
    fn restore_display_mode(&mut self) -> Result<bool> {
        Ok(false)
    }
}

/// `capture` on `backend`, kept only while its desktop is `expected` both
/// before and after.
pub(crate) fn capture_checked<B: Backend + ?Sized>(
    backend: &mut B,
    expected: &DesktopContext,
    capture: impl FnOnce(&mut B) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    if &backend.context()? != expected {
        return Err(error("desktop changed before capture"));
    }
    let frame = capture(backend)?;
    if &backend.context()? != expected {
        return Err(error(
            "desktop changed during capture; waiting for a current frame",
        ));
    }
    Ok(frame)
}

/// Every method forwards, provided ones included, so a boxed backend behaves
/// exactly as the backend inside it.
#[deny(clippy::missing_trait_methods)]
impl<B: Backend + ?Sized> Backend for Box<B> {
    fn display(&mut self) -> Result<Display> {
        (**self).display()
    }
    fn context(&mut self) -> Result<DesktopContext> {
        (**self).context()
    }
    fn describe(&mut self, probe_h265: bool) -> Result<Description> {
        (**self).describe(probe_h265)
    }
    fn bind(&mut self, waker: Waker) {
        (**self).bind(waker);
    }
    fn configure_video(&mut self, limits: VideoLimits, nominal_fps: u32) -> Result<()> {
        (**self).configure_video(limits, nominal_fps)
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        (**self).capture()
    }
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>> {
        (**self).capture_consistent(expected)
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        (**self).capture_video(expected, codec, keyframe, rate)
    }
    fn drain_video(
        &mut self,
        expected: &DesktopContext,
        wait: Duration,
    ) -> Result<Option<EncodedFrame>> {
        (**self).drain_video(expected, wait)
    }
    fn pending_video(&self) -> usize {
        (**self).pending_video()
    }
    fn drain_poll(&self) -> Option<Duration> {
        (**self).drain_poll()
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        (**self).cursor(expected)
    }
    fn cursor_shape(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorShape>> {
        (**self).cursor_shape(expected)
    }
    fn input(&mut self, event: &Input) -> Result<()> {
        (**self).input(event)
    }
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()> {
        (**self).input_in_context(expected, event)
    }
    fn presented(&mut self, expected: &DesktopContext) -> Result<()> {
        (**self).presented(expected)
    }
    fn release(&mut self) {
        (**self).release();
    }
    fn double_click_guard(&mut self) -> Option<DoubleClickGuard> {
        (**self).double_click_guard()
    }
    fn display_modes(&mut self, expected: &DesktopContext) -> Result<Option<ModeList>> {
        (**self).display_modes(expected)
    }
    fn set_display_mode(&mut self, expected: &DesktopContext, mode: Mode) -> Result<()> {
        (**self).set_display_mode(expected, mode)
    }
    fn restore_display_mode(&mut self) -> Result<bool> {
        (**self).restore_display_mode()
    }
}

/// What a backend told thread C right after it was built.
#[derive(Clone, Debug)]
pub struct Description {
    pub display: Display,
    pub caps: BackendCaps,
}

/// What a backend can do. Facts that differ by machine (whether this desktop
/// can be locked, whether modes can change) come from the backend at run
/// time, never from the target it was built for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BackendCaps {
    pub h265: bool,
    pub lock: bool,
    pub display_modes: bool,
    pub os: Option<HostOs>,
    pub cursor: CursorMode,
    /// It injects `Input::PhysicalKey`: keys by their place on the
    /// keyboard, which the host's layout and input method type.
    pub physical_keys: bool,
    /// `Backend::cursor_shape` reports the pointer's shape.
    pub cursor_shapes: bool,
}
impl BackendCaps {
    /// The values a host lists in `Accepted.capabilities` for these.
    pub(crate) fn values(&self) -> Vec<String> {
        let lock = self
            .lock
            .then(|| crate::protocol::CAPABILITY_LOCK.to_string());
        let modes = self
            .display_modes
            .then(|| crate::protocol::CAPABILITY_DISPLAY_MODE.to_string());
        let os = self.os.map(|os| os.capability().to_string());
        let physical_keys = self
            .physical_keys
            .then(|| crate::protocol::CAPABILITY_PHYSICAL_KEYS.to_string());
        let shapes = self
            .cursor_shapes
            .then(|| crate::protocol::CAPABILITY_CURSOR_SHAPE.to_string());
        lock.into_iter()
            .chain(modes)
            .chain(os)
            .chain(physical_keys)
            .chain(shapes)
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostOs {
    Windows,
    Macos,
    Linux,
    Android,
}
impl HostOs {
    /// The capability value a host on this OS lists; a viewer that does not
    /// know it ignores it.
    pub(crate) fn capability(self) -> &'static str {
        match self {
            Self::Windows => "os_windows",
            Self::Macos => "os_macos",
            Self::Linux => "os_linux",
            Self::Android => "os_android",
        }
    }
    /// The OS a host's capability value names.
    pub(crate) fn from_capability(value: &str) -> Option<Self> {
        [Self::Windows, Self::Macos, Self::Linux, Self::Android]
            .into_iter()
            .find(|os| os.capability() == value)
    }
}

/// How the host reports the pointer. `Embedded`: drawn into the picture, so
/// the viewer must not draw its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorMode {
    #[default]
    Separate,
    Embedded,
    None,
}

/// A `Clone + Send + Sync` handle to the session's wake: a push source calls
/// [`Waker::wake`] from any thread when it has something for thread C.
#[derive(Clone)]
pub struct Waker(pub(crate) crate::scheduler::Wake);
impl Waker {
    pub fn wake(&self) {
        self.0.notify();
    }
}

/// A backend failure of a known kind. Its text is the message alone, so a
/// failure reads as it did before it carried a kind.
#[derive(Debug)]
pub struct BackendError {
    kind: FailureKind,
    message: String,
}
impl BackendError {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    pub fn kind(&self) -> FailureKind {
        self.kind
    }
}
impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for BackendError {}
/// `ErrorKind::Unsupported` for `FailureKind::Unsupported`, else `Other`.
impl From<BackendError> for io::Error {
    fn from(failure: BackendError) -> Self {
        let kind = if failure.kind == FailureKind::Unsupported {
            io::ErrorKind::Unsupported
        } else {
            io::ErrorKind::Other
        };
        io::Error::new(kind, failure)
    }
}

/// The kind `error` carries, if a backend gave it one.
pub fn failure_kind(error: &io::Error) -> Option<FailureKind> {
    error
        .get_ref()?
        .downcast_ref::<BackendError>()
        .map(BackendError::kind)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureKind {
    /// Input belongs to a desktop this backend cannot reach (UAC, the lock
    /// screen) until it returns.
    SecureDesktop,
    /// The selected display is gone.
    DisplayLost,
    CaptureFailed,
    /// The hardware codec itself failed, not the capture around it.
    EncoderFailed {
        codec: VideoCodec,
    },
    Unsupported,
    PermissionMissing,
    ConsentRevoked,
}
