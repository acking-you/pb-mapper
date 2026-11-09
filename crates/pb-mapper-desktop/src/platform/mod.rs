//! The host platform, chosen once, at compile time. A [`Platform`] names a
//! session's parts: the desktop binding, the fused capture and encoder, the
//! injector, the pointer, display modes and stills. `PlatformBackend<P>`
//! (`native.rs`) drives them as one [`Backend`](crate::Backend) with static
//! calls only. Nothing above this module names an operating system: a new
//! one adds `platform/<os>/` and an arm of [`Current`]. "Platforms" in
//! `docs/remote-desktop.md` is the guide.

#[cfg(test)]
pub(crate) mod synthetic;
#[cfg(not(windows))]
pub(crate) mod unsupported;
// Every platform's, until one has a better wait; Windows tests it too.
#[cfg(any(not(windows), test))]
pub(crate) mod waiter;
#[cfg(windows)]
pub(crate) mod windows;

use crate::{
    Access, BackendCaps, BackendError, Button, CursorMode, DesktopKind, Display, DisplaySelection,
    DoubleClickGuard, EncodedFrame, FailureKind, HostOptions, HostOs, Key, Result, VideoCodec,
    VideoLimits, VideoRate, Waker,
    input::held::Pressed,
    launch::{Launch, Launcher},
    modes::{Journal, Mode, Setting},
    native::PlatformBackend,
};
use serde::{Deserialize, Serialize};
use std::{future::Future, path::Path, path::PathBuf, sync::Arc, time::Duration};

/// This build's platform.
#[cfg(windows)]
pub(crate) type Current = windows::Windows;
#[cfg(not(windows))]
pub(crate) type Current = unsupported::Unsupported;

#[cfg(not(windows))]
pub(crate) use waiter::{Waiter, Wake};
#[cfg(windows)]
pub(crate) use windows::waiter::{Waiter, Wake};

/// Fine process timers, counted by `timer::TimerResolution`: on where the
/// platform coarsens them, nothing elsewhere.
#[cfg(windows)]
pub(crate) use windows::timer as timers;
#[cfg(not(windows))]
pub(crate) mod timers {
    pub(crate) fn acquire() {}
    pub(crate) fn release() {}
}

/// An operating system that hosts: a session's parts, built on its thread C.
pub(crate) trait Platform: Sized + 'static {
    const OS: HostOs;
    /// Host-scoped consent and resources, acquired once per host, never per
    /// session. Windows: `()`.
    type Grant: Send + Sync + 'static;
    type Desktop: Desktop;
    type Video: VideoSource<Grant = Self::Grant>;
    type Input: Injector;
    type Cursor: CursorSource;
    type Modes: DisplayModes;
    type Still: StillCapture;

    /// Can this machine host now, and if not why. Cheap.
    fn support() -> HostSupport;
    fn displays() -> Result<Vec<Display>>;
    /// Outside thread C, when a host starts and before it registers; it may
    /// wait on a person (a consent dialog). Resolves the selection. Windows:
    /// ready at once.
    fn acquire(
        selection: &DisplaySelection,
    ) -> impl Future<Output = Result<(Self::Grant, Display)>> + Send;
    /// On thread C: the session's parts, built there because they are not
    /// `Send`. Re-resolves the display by `Display::name` when its id is
    /// stale.
    fn open(grant: &Self::Grant, display: &Display) -> Result<Parts<Self>>;
    /// Where an in-process host keeps what it must restore after a crash.
    fn default_state_dir() -> Option<PathBuf>;
    /// Replay the display-mode journal under `state_dir`: restore what a
    /// host left changed, unless its owner changed it since. How many
    /// entries it restored.
    fn recover(state_dir: &Path) -> Result<usize>;
}

/// A session's parts. They drop in this order: the desktop binding last,
/// after everything that may still touch the desktop.
pub(crate) struct Parts<P: Platform> {
    pub input: P::Input,
    pub cursor: P::Cursor,
    pub modes: P::Modes,
    pub still: P::Still,
    pub desktop: P::Desktop,
}

/// The input desktop this thread captures and injects on.
pub(crate) trait Desktop: Sized {
    /// Held around every OS call on thread C. Windows: per-monitor DPI
    /// awareness.
    type Scope;
    fn scope(&self) -> Self::Scope;
    /// Bind to the current input desktop. Runs `on_switch` (release held
    /// input, drop the video source) before returning when the binding
    /// moved, and when the ordinary desktop is not the input desktop, which
    /// fails with `SecureDesktop`.
    fn prepare(&mut self, on_switch: impl FnOnce()) -> Result<()>;
    /// The geometry now. A display whose handle went stale is found again
    /// by its device name before this fails.
    fn display(&mut self) -> Result<Display>;
    fn generation(&self) -> u64 {
        0
    }
    fn kind(&self) -> DesktopKind {
        DesktopKind::Ordinary
    }
    /// The desktop moved since the last `prepare` (a privileged binding).
    fn changed(&self) -> bool {
        false
    }
}

/// Capture and encode fused, as thread C sees them.
pub(crate) trait VideoSource: Sized {
    type Grant;
    /// `Some(poll)`: output that thread C polls for. `None`: the source
    /// pushes, and wakes thread C.
    const DRAIN: Option<Duration>;
    /// Asked only for a viewer that decodes the codec; the platform caches
    /// the answer per adapter.
    fn encodes(display: &Display, codec: VideoCodec) -> bool;
    fn open(
        grant: &Self::Grant,
        display: &Display,
        codec: VideoCodec,
        limits: VideoLimits,
        rate: VideoRate,
        waker: &Waker,
    ) -> Result<Self>;
    fn codec(&self) -> VideoCodec;
    fn set_rate(&mut self, rate: VideoRate) -> Result<()>;
    /// Pull: capture and submit one picture. Push: carry `keyframe` to the
    /// next picture; never blocks.
    fn submit(&mut self, keyframe: bool) -> Result<()>;
    fn drain(&mut self) -> Result<Option<EncodedFrame>>;
    fn pending(&self) -> usize;
}

/// Injection primitives. The release obligation, validation and the desktop
/// checks live once, in `PlatformBackend`.
pub(crate) trait Injector: Sized {
    /// The compile-time default; the runtime truth is `can_lock`.
    const LOCK: bool;
    fn can_lock(&self) -> bool {
        Self::LOCK
    }
    /// Absolute physical virtual-desktop pixels: display origin plus offset.
    fn pointer(&mut self, x: i32, y: i32) -> Result<()>;
    fn button(&mut self, button: Button, down: bool) -> Result<()>;
    fn wheel(&mut self, horizontal: i32, vertical: i32) -> Result<()>;
    fn key(&mut self, key: Key, down: bool) -> Result<()>;
    fn text(&mut self, text: &str) -> Result<()>;
    /// Called only when `can_lock`.
    fn lock(&mut self) -> Result<()>;
    fn double_click(&mut self) -> Option<DoubleClickGuard> {
        None
    }
    /// Let go of a held key or button. A failure keeps it held for the next
    /// release, and is not an input failure to diagnose.
    fn release(&mut self, pressed: Pressed) -> Result<()> {
        match pressed {
            Pressed::Button(button) => self.button(button, false),
            Pressed::Key(key) => self.key(key, false),
        }
    }
}

pub(crate) trait CursorSource: Sized {
    const MODE: CursorMode;
    fn mode(&self) -> CursorMode {
        Self::MODE
    }
    /// The absolute pointer and whether it shows; `None` where the platform
    /// has none.
    fn read(&mut self) -> Result<Option<(i32, i32, bool)>>;
}

/// A display's modes, by its device name (`Display::name`), the key that
/// stays when a mode change gives the display another id, and the only one
/// a journal replay knows.
pub(crate) trait DisplayModes: Sized {
    const SUPPORTED: bool;
    /// The runtime truth: `SUPPORTED && available()` gates everything.
    fn available(&mut self, device: &str) -> bool {
        let _ = device;
        Self::SUPPORTED
    }
    /// Physical pixels, deduplicated, largest first, at most 64.
    fn list(&mut self, device: &str) -> Result<Vec<Mode>>;
    fn current(&mut self, device: &str) -> Result<Setting>;
    /// What `apply` would set for `mode`: its refresh rate and depth.
    fn target(&mut self, device: &str, mode: Mode) -> Result<Setting>;
    /// Dynamic: the OS must not keep it.
    fn apply(&mut self, device: &str, setting: &Setting) -> Result<()>;
    /// Back to the user's own mode.
    fn reset(&mut self, device: &str, original: &Setting) -> Result<()>;
}

/// No mode changes.
pub(crate) struct NoModes;
impl DisplayModes for NoModes {
    const SUPPORTED: bool = false;
    fn list(&mut self, _: &str) -> Result<Vec<Mode>> {
        Err(no_modes())
    }
    fn current(&mut self, _: &str) -> Result<Setting> {
        Err(no_modes())
    }
    fn target(&mut self, _: &str, _: Mode) -> Result<Setting> {
        Err(no_modes())
    }
    fn apply(&mut self, _: &str, _: &Setting) -> Result<()> {
        Err(no_modes())
    }
    fn reset(&mut self, _: &str, _: &Setting) -> Result<()> {
        Err(no_modes())
    }
}
pub(crate) fn no_modes() -> std::io::Error {
    BackendError::new(FailureKind::Unsupported, "display modes").into()
}

pub(crate) trait StillCapture: Sized {
    fn jpeg(&mut self, display: &Display) -> Result<Vec<u8>>;
}

/// No stills: an encoder the OS fuses with capture has no CPU pixels.
#[expect(dead_code, reason = "for the first platform without stills (Android)")]
pub(crate) struct NoStill;
impl StillCapture for NoStill {
    fn jpeg(&mut self, _: &Display) -> Result<Vec<u8>> {
        Err(BackendError::new(FailureKind::Unsupported, "still capture").into())
    }
}

/// Whether this machine can host and view, and if it cannot host, why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct HostSupport {
    pub host: bool,
    pub viewer: bool,
    /// The secure desktop (UAC, the lock screen) without the privileged
    /// helper.
    pub secure_desktop: bool,
    pub os: Option<HostOs>,
    pub reason: Option<UnsupportedReason>,
}

/// Why this machine cannot host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnsupportedReason {
    /// Hosting is not implemented on this operating system yet.
    Platform,
    /// Windows without Media Foundation (N and KN editions without the Media
    /// Feature Pack, Windows Server without the feature).
    MediaFoundationMissing,
    NoDisplay,
    PermissionMissing,
}

/// Whether this machine can host now, and if not why.
pub fn host_support() -> HostSupport {
    Current::support()
}

/// The displays this machine could share.
pub fn displays() -> Result<Vec<Display>> {
    Current::displays()
}

/// What sessions on platform `P` can do when their backend does not describe
/// itself in time: its compile-time facts.
pub(crate) fn fallback_caps<P: Platform>() -> BackendCaps {
    BackendCaps {
        lock: <P::Input as Injector>::LOCK,
        os: Some(P::OS),
        cursor: <P::Cursor as CursorSource>::MODE,
        ..BackendCaps::default()
    }
}

/// The platform's own capture and input for `options`. Refused before the
/// host registers where there are none, so that no service is published only
/// to fail every viewer at its encoder.
pub(crate) async fn native_launch(options: &HostOptions) -> Result<Arc<dyn Launch>> {
    let support = Current::support();
    if !support.host {
        return Err(unsupported(support.reason));
    }
    match options.access {
        Access::Ordinary => {
            let (grant, display) = Current::acquire(&options.display).await?;
            let grant = Arc::new(grant);
            let state_dir = options
                .state_dir
                .clone()
                .or_else(Current::default_state_dir);
            let journal = Journal::in_dir(state_dir.clone());
            let replayed = journal.clone();
            Ok(Arc::new(
                Launcher::new(
                    move || {
                        PlatformBackend::<Current>::open(grant.clone(), &display, journal.clone())
                    },
                    fallback_caps::<Current>(),
                )
                .recovering(move || {
                    if let Some(state_dir) = &state_dir {
                        Current::recover(state_dir)?;
                    }
                    Ok(replayed.pending())
                }),
            ))
        }
        // The helper's worker journals and replays in its protected
        // directory; this host keeps nothing.
        Access::Privileged => privileged_launch(&options.display),
    }
}

/// Put back the display modes a host of this process left changed: a
/// crashed one, or one whose restore failed (the ordinary desktop was away).
/// `state_dir` is the host's [`HostOptions::state_dir`]; `None` is this
/// platform's own. A display whose mode its owner changed since is left as
/// it is. How many were restored. A host replays its journal when it starts,
/// and every session before it changes anything; an app calls this once
/// when it starts too.
pub fn recover_display_modes(state_dir: Option<&Path>) -> Result<usize> {
    match state_dir
        .map(Path::to_path_buf)
        .or_else(Current::default_state_dir)
    {
        Some(state_dir) => Current::recover(&state_dir),
        None => Ok(0),
    }
}

#[cfg(windows)]
fn privileged_launch(selection: &DisplaySelection) -> Result<Arc<dyn Launch>> {
    windows::helper::launch(selection)
}
#[cfg(not(windows))]
fn privileged_launch(_: &DisplaySelection) -> Result<Arc<dyn Launch>> {
    Err(BackendError::new(
        FailureKind::Unsupported,
        "privileged desktop access requires Windows",
    )
    .into())
}

/// Whether this machine offers privileged access now, and if not why.
#[cfg(windows)]
pub(crate) fn privileged_support() -> crate::privileged::PrivilegedSupport {
    let ready = windows::helper::status().ensure_ready();
    crate::privileged::PrivilegedSupport {
        available: ready.is_ok(),
        reason: ready.err().map(|failure| failure.to_string()),
    }
}
#[cfg(not(windows))]
pub(crate) fn privileged_support() -> crate::privileged::PrivilegedSupport {
    crate::privileged::PrivilegedSupport {
        available: false,
        reason: Some("privileged desktop access requires Windows".into()),
    }
}

fn unsupported(reason: Option<UnsupportedReason>) -> std::io::Error {
    let message = match reason {
        Some(UnsupportedReason::MediaFoundationMissing) => {
            "Desktop hosting requires Windows Media Foundation"
        }
        Some(UnsupportedReason::NoDisplay) => "No display is available to share",
        Some(UnsupportedReason::PermissionMissing) => {
            "Desktop hosting needs a permission this computer has not granted"
        }
        Some(UnsupportedReason::Platform) | None => "Desktop hosting currently requires Windows",
    };
    BackendError::new(FailureKind::Unsupported, message).into()
}
