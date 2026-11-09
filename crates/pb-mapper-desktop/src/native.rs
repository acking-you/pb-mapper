//! The neutral glue between a platform's parts and thread C:
//! `PlatformBackend<P>` is a [`Backend`] for any [`Platform`], with every
//! call static. It owns what every platform shares: the desktop checks
//! around each capture, pointer read and input, the release obligation of
//! held input, input validation, and the video source's lifetime.
//! [`NativeBackend`] is this build's platform behind an opaque type.

use crate::{
    Backend, BackendCaps, BackendError, Description, DesktopContext, DesktopKind, Display,
    DisplaySelection, DoubleClickGuard, EncodedFrame, FailureKind, Input, Result, VideoCodec,
    VideoFormat, VideoLimits, VideoRate, Waker, error,
    input::{cursor::confine, held::Held},
    modes::{self, Guard, Journal, Mode, ModeList},
    platform::{
        self, CursorSource, Desktop, DisplayModes, Injector, Parts, Platform, StillCapture,
        VideoSource, no_modes,
    },
};
use std::{sync::Arc, time::Duration};

/// The encoder's limits until `configure_video`, which every session calls
/// before it captures.
const UNCONFIGURED: VideoLimits = VideoLimits {
    max_width: 1920,
    max_height: 1080,
    max_fps: 60,
};

pub(crate) struct PlatformBackend<P: Platform> {
    /// Dropped before the parts: a source built on the desktop binding goes
    /// before the binding.
    video: Option<(DesktopContext, P::Video)>,
    parts: Parts<P>,
    grant: Arc<P::Grant>,
    held: Held,
    limits: VideoLimits,
    /// A pushing source's wake: a session's once bound, else one of its own,
    /// so a backend no session drives (the helper's worker, a bench) is never
    /// without one.
    waker: Waker,
    /// What this backend changed of its display's mode, journalled in
    /// `journal`, which it replays first.
    modes: Guard,
}
impl<P: Platform> PlatformBackend<P> {
    /// On thread C: the session's parts, for `display`.
    pub(crate) fn open(grant: Arc<P::Grant>, display: &Display, journal: Journal) -> Result<Self> {
        let parts = P::open(&grant, display)?;
        Ok(Self::from_parts(grant, parts, journal))
    }
    /// What a host that crashed left changed is put back before this one
    /// changes anything.
    pub(crate) fn from_parts(grant: Arc<P::Grant>, mut parts: Parts<P>, journal: Journal) -> Self {
        if <P::Modes as DisplayModes>::SUPPORTED {
            let _scope = parts.desktop.scope();
            if let Err(failure) = modes::recover(&mut parts.modes, &journal) {
                tracing::warn!(
                    target: "pb_mapper_desktop::session",
                    event = "display_mode_journal_unreplayed",
                    detail = %failure,
                );
            }
        }
        Self {
            video: None,
            parts,
            grant,
            held: Held::default(),
            limits: UNCONFIGURED,
            waker: Waker(crate::scheduler::Wake::new()),
            modes: Guard::new(journal),
        }
    }
    /// Bind to the input desktop. A switch, or a desktop this process cannot
    /// reach, releases held input and drops the video source first.
    fn prepare(&mut self) -> Result<()> {
        let (held, input, video) = (&mut self.held, &mut self.parts.input, &mut self.video);
        self.parts.desktop.prepare(|| {
            release(held, input);
            *video = None;
        })
    }
}
fn release<I: Injector>(held: &mut Held, input: &mut I) {
    held.release(|pressed| input.release(pressed));
}

#[deny(clippy::missing_trait_methods)]
impl<P: Platform> Backend for PlatformBackend<P> {
    fn display(&mut self) -> Result<Display> {
        let _scope = self.parts.desktop.scope();
        self.prepare()?;
        self.parts.desktop.display()
    }
    fn context(&mut self) -> Result<DesktopContext> {
        let display = self.display()?;
        Ok(DesktopContext {
            display,
            generation: self.parts.desktop.generation(),
            desktop: self.parts.desktop.kind(),
        })
    }
    fn describe(&mut self, probe_h265: bool) -> Result<Description> {
        let display = self.display()?;
        let modes = &mut self.parts.modes;
        Ok(Description {
            caps: BackendCaps {
                h265: probe_h265 && P::Video::encodes(&display, VideoCodec::H265),
                lock: self.parts.input.can_lock(),
                display_modes: <P::Modes as DisplayModes>::SUPPORTED
                    && modes.available(&display.name),
                os: Some(P::OS),
                cursor: self.parts.cursor.mode(),
            },
            display,
        })
    }
    fn bind(&mut self, waker: Waker) {
        self.waker = waker;
    }
    fn configure_video(&mut self, limits: VideoLimits, _nominal_fps: u32) -> Result<()> {
        limits.validate()?;
        if self.limits != limits {
            self.video = None;
            self.limits = limits;
        }
        Ok(())
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        let display = self.display()?;
        let _scope = self.parts.desktop.scope();
        self.prepare()?;
        let jpeg = self.parts.still.jpeg(&display)?;
        self.prepare()?;
        Ok(jpeg)
    }
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>> {
        if &self.context()? != expected {
            return Err(error("desktop changed before capture"));
        }
        let frame = {
            let _scope = self.parts.desktop.scope();
            self.prepare()?;
            let jpeg = self.parts.still.jpeg(&expected.display)?;
            self.prepare()?;
            jpeg
        };
        if &self.context()? != expected {
            return Err(error(
                "desktop changed during capture; waiting for a current frame",
            ));
        }
        Ok(frame)
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        if codec == VideoCodec::Jpeg {
            return Ok(Some(EncodedFrame {
                format: VideoFormat {
                    codec,
                    keyframe: true,
                    width: expected.display.width,
                    height: expected.display.height,
                    pts_us: 0,
                    duplicate: false,
                    encode_us: 0,
                    gop: 0,
                },
                bytes: self.capture_consistent(expected)?,
            }));
        }
        let _scope = self.parts.desktop.scope();
        if &self.context()? != expected {
            return Err(error("desktop changed before video capture"));
        }
        if self
            .video
            .as_ref()
            .is_none_or(|(context, video)| context != expected || video.codec() != codec)
        {
            self.video = None;
            let source = P::Video::open(
                &self.grant,
                &expected.display,
                codec,
                self.limits,
                rate,
                &self.waker,
            )?;
            self.video = Some((expected.clone(), source));
        }
        let video = &mut self
            .video
            .as_mut()
            .ok_or_else(|| error("video encoder unavailable"))?
            .1;
        // The worker paces: drain what is finished, then submit one capture.
        let image = video
            .set_rate(rate)
            .and_then(|()| video.drain())
            .and_then(|image| video.submit(keyframe).map(|()| image));
        if &self.context()? != expected {
            self.video = None;
            return Err(error("desktop changed during video capture"));
        }
        image.inspect_err(|_| self.video = None)
    }
    /// Never blocks: the wait bounds only the helper's long-poll. A drain
    /// checks no desktop: it only collects output of submits `capture_video`
    /// checked, and the next submit or the worker's context check catches a
    /// switch (on the helper, `serve` checks before and after each video
    /// request).
    fn drain_video(
        &mut self,
        expected: &DesktopContext,
        _wait: Duration,
    ) -> Result<Option<EncodedFrame>> {
        let Some((context, video)) = &mut self.video else {
            return Ok(None);
        };
        if context != expected {
            return Ok(None);
        }
        video.drain().inspect_err(|_| self.video = None)
    }
    fn pending_video(&self) -> usize {
        self.video.as_ref().map_or(0, |(_, video)| video.pending())
    }
    fn drain_poll(&self) -> Option<Duration> {
        <P::Video as VideoSource>::DRAIN
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        let _scope = self.parts.desktop.scope();
        if &self.context()? != expected {
            return Err(error("desktop changed before cursor capture"));
        }
        let read = self.parts.cursor.read()?;
        if &self.context()? != expected || self.parts.desktop.changed() {
            return Err(error("desktop changed during cursor capture"));
        }
        Ok(read.map(|(x, y, showing)| confine(&expected.display, (x, y), showing)))
    }
    fn input(&mut self, event: &Input) -> Result<()> {
        let context = self.context()?;
        self.input_in_context(&context, event)
    }
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()> {
        let _scope = self.parts.desktop.scope();
        if &self.context()? != expected || self.parts.desktop.changed() {
            self.release();
            return Err(error("desktop changed before input"));
        }
        event.validate(&expected.display)?;
        // Do not rebind to another desktop below this point. A desktop switch
        // invalidates this operation instead of replaying it on the new desktop.
        let input = &mut self.parts.input;
        match event {
            Input::Move { x, y } => {
                let display = &expected.display;
                input.pointer(display.x + *x as i32, display.y + *y as i32)
            }
            Input::Button { button, down, .. } => self
                .held
                .button(*button, *down, |down| input.button(*button, down)),
            Input::Wheel {
                horizontal,
                vertical,
            } => input.wheel(*horizontal, *vertical),
            Input::Key { key, down } => self.held.key(*key, *down, |down| input.key(*key, down)),
            Input::Text { text } => input.text(text),
            // The interactive session's own lock; the desktop switch that
            // follows invalidates the generation through the context check.
            Input::Lock {} if input.can_lock() => input.lock(),
            Input::Lock {} => {
                Err(BackendError::new(FailureKind::Unsupported, "lock the desktop").into())
            }
        }
    }
    fn presented(&mut self, _expected: &DesktopContext) -> Result<()> {
        Ok(())
    }
    fn release(&mut self) {
        release(&mut self.held, &mut self.parts.input);
    }
    fn double_click_guard(&mut self) -> Option<DoubleClickGuard> {
        let _scope = self.parts.desktop.scope();
        self.parts.input.double_click()
    }
    fn display_modes(&mut self, expected: &DesktopContext) -> Result<Option<ModeList>> {
        if !<P::Modes as DisplayModes>::SUPPORTED {
            return Ok(None);
        }
        let _scope = self.parts.desktop.scope();
        self.modes
            .list(&mut self.parts.modes, &expected.display.name)
    }
    fn set_display_mode(&mut self, expected: &DesktopContext, mode: Mode) -> Result<()> {
        if !<P::Modes as DisplayModes>::SUPPORTED {
            return Err(no_modes());
        }
        let _scope = self.parts.desktop.scope();
        if &self.context()? != expected {
            return Err(error("desktop changed before a display mode change"));
        }
        if expected.desktop != DesktopKind::Ordinary {
            return Err(BackendError::new(
                FailureKind::SecureDesktop,
                "the remote resolution cannot change on the secure desktop",
            )
            .into());
        }
        // Capture loses its device across the change; the source is built
        // again at the new size.
        self.video = None;
        self.modes
            .set(&mut self.parts.modes, &expected.display.name, mode)
    }
    fn restore_display_mode(&mut self) -> Result<bool> {
        if !<P::Modes as DisplayModes>::SUPPORTED {
            return Ok(false);
        }
        let _scope = self.parts.desktop.scope();
        let video = &mut self.video;
        self.modes.restore(&mut self.parts.modes, || *video = None)
    }
}
impl<P: Platform> Drop for PlatformBackend<P> {
    fn drop(&mut self) {
        self.release();
        // A backstop: thread C restores first. What fails here stays
        // journalled, and the host tries again.
        if let Err(failure) = self.restore_display_mode() {
            tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "display_mode_not_restored",
                detail = %failure,
            );
        }
        self.modes.release();
        // The video source goes before the desktop binding, which restores
        // the thread's original desktop when it drops.
        self.video = None;
    }
}

/// This build's own capture and input, as a [`Backend`] built and used on
/// the calling thread: for benches, and for an embedder's backend that
/// decorates the native one.
pub struct NativeBackend(PlatformBackend<platform::Current>);

/// The native backend for `display`, built on the calling thread, which then
/// owns it. Fails where this platform cannot host, or would first have to ask
/// a person (start a `Host` there instead).
pub fn native_backend(display: &DisplaySelection) -> Result<NativeBackend> {
    use platform::Platform as _;
    use std::task::{Context, Poll, Waker as TaskWaker};
    let support = platform::Current::support();
    if !support.host {
        return Err(BackendError::new(
            FailureKind::Unsupported,
            "Desktop hosting is not available on this computer",
        )
        .into());
    }
    let acquire = std::pin::pin!(platform::Current::acquire(display));
    let Poll::Ready(acquired) = acquire.poll(&mut Context::from_waker(TaskWaker::noop())) else {
        return Err(BackendError::new(
            FailureKind::PermissionMissing,
            "this platform asks before sharing; start a Host instead",
        )
        .into());
    };
    let (grant, display) = acquired?;
    Ok(NativeBackend(PlatformBackend::open(
        Arc::new(grant),
        &display,
        Journal::in_dir(platform::Current::default_state_dir()),
    )?))
}

#[deny(clippy::missing_trait_methods)]
impl Backend for NativeBackend {
    fn display(&mut self) -> Result<Display> {
        self.0.display()
    }
    fn context(&mut self) -> Result<DesktopContext> {
        self.0.context()
    }
    fn describe(&mut self, probe_h265: bool) -> Result<Description> {
        self.0.describe(probe_h265)
    }
    fn bind(&mut self, waker: Waker) {
        self.0.bind(waker);
    }
    fn configure_video(&mut self, limits: VideoLimits, nominal_fps: u32) -> Result<()> {
        self.0.configure_video(limits, nominal_fps)
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        self.0.capture()
    }
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>> {
        self.0.capture_consistent(expected)
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        self.0.capture_video(expected, codec, keyframe, rate)
    }
    fn drain_video(
        &mut self,
        expected: &DesktopContext,
        wait: Duration,
    ) -> Result<Option<EncodedFrame>> {
        self.0.drain_video(expected, wait)
    }
    fn pending_video(&self) -> usize {
        self.0.pending_video()
    }
    fn drain_poll(&self) -> Option<Duration> {
        self.0.drain_poll()
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        self.0.cursor(expected)
    }
    fn input(&mut self, event: &Input) -> Result<()> {
        self.0.input(event)
    }
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()> {
        self.0.input_in_context(expected, event)
    }
    fn presented(&mut self, expected: &DesktopContext) -> Result<()> {
        self.0.presented(expected)
    }
    fn release(&mut self) {
        self.0.release();
    }
    fn double_click_guard(&mut self) -> Option<DoubleClickGuard> {
        self.0.double_click_guard()
    }
    fn display_modes(&mut self, expected: &DesktopContext) -> Result<Option<ModeList>> {
        self.0.display_modes(expected)
    }
    fn set_display_mode(&mut self, expected: &DesktopContext, mode: Mode) -> Result<()> {
        self.0.set_display_mode(expected, mode)
    }
    fn restore_display_mode(&mut self) -> Result<bool> {
        self.0.restore_display_mode()
    }
}
