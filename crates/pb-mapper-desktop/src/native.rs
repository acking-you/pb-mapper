//! The neutral glue between a platform's parts and thread C:
//! `PlatformBackend<P>` is a [`Backend`] for any [`Platform`], with every
//! call static. It owns what every platform shares: the desktop checks
//! around each capture, pointer read and input, the release obligation of
//! held input, input validation, and the video source's lifetime.
//! [`NativeBackend`] is this build's platform behind an opaque type.

use crate::{
    Backend, BackendCaps, BackendError, Description, DesktopContext, DesktopKind, Display,
    DisplaySelection, DoubleClickGuard, EncodedFrame, FailureKind, Input, Presented, Result,
    VideoCodec, VideoFormat, VideoLimits, VideoRate, Waker,
    backend::capture_checked,
    error,
    input::{Typed, cursor::confine, held::Held},
    modes::{self, Guard, Journal, Mode, ModeList},
    platform::{
        self, CursorSource, Desktop, DisplayModes, Injector, Parts, Platform, StillCapture,
        VideoSource, no_modes,
    },
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

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
    submitted_video: bool,
    /// When the picture the last capture submitted was presented, if it
    /// was a new one its source dated.
    submitted_picture: Option<Presented>,
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
            submitted_video: true,
            submitted_picture: None,
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
    /// `recent`: a check that passed a moment ago may answer
    /// (`Desktop::prepare_recent`).
    fn prepare(&mut self, recent: bool) -> Result<()> {
        let (held, input, video) = (&mut self.held, &mut self.parts.input, &mut self.video);
        let on_switch = || {
            release(held, input);
            *video = None;
        };
        if recent {
            self.parts.desktop.prepare_recent(on_switch)
        } else {
            self.parts.desktop.prepare(on_switch)
        }
    }
    /// Within the desktop's scope, bind and verify `expected` without
    /// allocating `Display::name`. `recent`, around a capture or a pointer
    /// read: the platform may answer from a check that passed a moment ago
    /// (macOS: within 16 ms). The injection boundary and a display mode
    /// change pass `false`, and check the desktop now.
    fn verify(&mut self, expected: &DesktopContext, recent: bool) -> Result<bool> {
        self.prepare(recent)?;
        if self.parts.desktop.generation() != expected.generation
            || self.parts.desktop.kind() != expected.desktop
        {
            return Ok(false);
        }
        if recent {
            self.parts.desktop.display_matches_recent(&expected.display)
        } else {
            self.parts.desktop.display_matches(&expected.display)
        }
    }
    /// One JPEG of `display`, bound to the input desktop before and after.
    fn still(&mut self, display: &Display) -> Result<Vec<u8>> {
        let _scope = self.parts.desktop.scope();
        self.prepare(false)?;
        let jpeg = self.parts.still.jpeg(display)?;
        self.prepare(false)?;
        Ok(jpeg)
    }
}
fn release<I: Injector>(held: &mut Held, input: &mut I) {
    held.release(|pressed| input.release(pressed));
}

#[deny(clippy::missing_trait_methods)]
impl<P: Platform> Backend for PlatformBackend<P> {
    fn display(&mut self) -> Result<Display> {
        let _scope = self.parts.desktop.scope();
        self.prepare(false)?;
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
                physical_keys: <P::Input as Injector>::PHYSICAL_KEYS,
                cursor_shapes: <P::Cursor as CursorSource>::SHAPES,
                rate_model: <P::Video as VideoSource>::RATE_MODEL,
            },
            display,
        })
    }
    fn bind(&mut self, waker: Waker) {
        self.waker = waker;
        self.video = None;
    }
    /// New limits that the open source can serve as it is (a frame rate
    /// change) keep it, with its pending output: thread C asks for an IDR
    /// anyway. Anything else, the same limits again (a recovery or a codec
    /// change, which thread C sends only after it passed on any change of
    /// limits) included, drops it, and the next capture builds a new one.
    fn configure_video(&mut self, limits: VideoLimits, _nominal_fps: u32) -> Result<()> {
        limits.validate()?;
        let retuned = limits != self.limits
            && match &mut self.video {
                Some((context, video)) => {
                    let _scope = self.parts.desktop.scope();
                    video.retune(&context.display, limits).unwrap_or(false)
                }
                None => false,
            };
        if !retuned {
            self.video = None;
        }
        self.limits = limits;
        Ok(())
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        let display = self.display()?;
        self.still(&display)
    }
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>> {
        capture_checked(self, expected, |this| this.still(&expected.display))
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        self.submitted_video = true;
        self.submitted_picture = None;
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
        if !self.verify(expected, true)? {
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
        // The worker paces: drain what is finished, then submit one capture,
        // and say when the display presented it.
        let (image, submitted, picture) = video
            .set_rate(rate)
            .and_then(|()| video.drain())
            .and_then(|image| {
                let submitted = video.submit(keyframe)?;
                Ok((
                    image,
                    submitted,
                    submitted.then(|| video.presented()).flatten(),
                ))
            })
            .inspect_err(|_| self.video = None)?;
        if !self.verify(expected, true)? {
            self.video = None;
            return Err(error("desktop changed during video capture"));
        }
        self.submitted_video = submitted;
        self.submitted_picture = picture;
        Ok(image)
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
    fn video_full(&self) -> bool {
        self.video.as_ref().is_some_and(|(_, video)| video.full())
    }
    fn drain_poll(&self) -> Option<Duration> {
        self.video
            .as_ref()
            .map_or(<P::Video as VideoSource>::DRAIN, |(_, video)| {
                video.drain_poll()
            })
    }
    fn capture_poll(&self) -> Option<Duration> {
        self.video
            .as_ref()
            .map_or(<P::Video as VideoSource>::CAPTURE_POLL, |(_, video)| {
                video.capture_poll()
            })
    }
    fn submitted_video(&self) -> bool {
        self.submitted_video
    }
    fn submitted_picture(&self) -> Option<Presented> {
        self.submitted_picture
    }
    fn capture_idle_until(&self, keyframe_due: Option<Instant>) -> Option<Instant> {
        self.video.as_ref()?.1.idle_until(keyframe_due)
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        let _scope = self.parts.desktop.scope();
        if !self.verify(expected, true)? {
            return Err(error("desktop changed before cursor capture"));
        }
        let read = self.parts.cursor.read()?;
        if !self.verify(expected, true)? || self.parts.desktop.changed() {
            return Err(error("desktop changed during cursor capture"));
        }
        Ok(read.map(|(x, y, showing)| confine(&expected.display, (x, y), showing)))
    }
    /// The shape of the pointer `cursor` just read, on the desktop it
    /// checked around that read.
    fn cursor_shape(&mut self, _expected: &DesktopContext) -> Result<Option<crate::CursorShape>> {
        let _scope = self.parts.desktop.scope();
        self.parts.cursor.shape()
    }
    fn input(&mut self, event: &Input) -> Result<()> {
        let context = self.context()?;
        self.input_in_context(&context, event)
    }
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()> {
        let _scope = self.parts.desktop.scope();
        if !self.verify(expected, false)? || self.parts.desktop.changed() {
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
            Input::Text { text } => {
                let held = &mut self.held;
                Input::typed(text, |typed| match typed {
                    Typed::Chars(chars) => input.text(chars),
                    Typed::Key(key) => held.tap(key, |down| input.key(key, down)),
                })
            }
            Input::PhysicalKey { usage, down } if <P::Input as Injector>::PHYSICAL_KEYS => self
                .held
                .physical(*usage, *down, |down| input.physical_key(*usage, down)),
            Input::PhysicalKey { .. } => {
                Err(BackendError::new(FailureKind::Unsupported, "physical keys").into())
            }
            // The interactive session's own lock; the desktop switch that
            // follows invalidates the generation through the context check.
            Input::Lock {} if input.can_lock() => input.lock(),
            Input::Lock {} => {
                Err(BackendError::new(FailureKind::Unsupported, "lock the desktop").into())
            }
        }
    }
    fn video_paused(&mut self, paused: bool) {
        if let Some((_, video)) = &mut self.video {
            let _scope = self.parts.desktop.scope();
            video.paused(paused);
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
        if !self.verify(expected, false)? {
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
        let set = self
            .modes
            .set(&mut self.parts.modes, &expected.display.name, mode);
        // A check from before the change, made or failed, no longer answers
        // capture and the pointer.
        self.parts.desktop.forget_recent();
        set
    }
    fn restore_display_mode(&mut self) -> Result<bool> {
        if !<P::Modes as DisplayModes>::SUPPORTED {
            return Ok(false);
        }
        let _scope = self.parts.desktop.scope();
        let video = &mut self.video;
        let restored = self.modes.restore(&mut self.parts.modes, || *video = None);
        // As after a change: the display may be another size now.
        self.parts.desktop.forget_recent();
        restored
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
    fn video_full(&self) -> bool {
        self.0.video_full()
    }
    fn drain_poll(&self) -> Option<Duration> {
        self.0.drain_poll()
    }
    fn capture_poll(&self) -> Option<Duration> {
        self.0.capture_poll()
    }
    fn submitted_video(&self) -> bool {
        self.0.submitted_video()
    }
    fn submitted_picture(&self) -> Option<Presented> {
        self.0.submitted_picture()
    }
    fn capture_idle_until(&self, keyframe_due: Option<Instant>) -> Option<Instant> {
        self.0.capture_idle_until(keyframe_due)
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        self.0.cursor(expected)
    }
    fn cursor_shape(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorShape>> {
        self.0.cursor_shape(expected)
    }
    fn input(&mut self, event: &Input) -> Result<()> {
        self.0.input(event)
    }
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()> {
        self.0.input_in_context(expected, event)
    }
    fn video_paused(&mut self, paused: bool) {
        self.0.video_paused(paused);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::synthetic::{Checks, Probe, Synthetic};

    /// Capture and the pointer let the platform answer from a desktop check
    /// that passed a moment ago; the injection boundary, a display mode
    /// change and the context check always check the desktop now, and a
    /// mode change or restore leaves nothing to reuse.
    #[test]
    fn only_capture_and_the_pointer_may_reuse_a_desktop_check() {
        let probe = Probe::default();
        let mut backend = PlatformBackend::<Synthetic<false>>::open(
            Arc::new(probe.clone()),
            &Probe::display(),
            Journal::default(),
        )
        .unwrap();
        let fresh = |checks| Checks {
            prepare: checks,
            matches: checks,
            ..Checks::default()
        };
        let recent = |checks| Checks {
            prepare_recent: checks,
            matches_recent: checks,
            ..Checks::default()
        };

        let context = backend.context().unwrap();
        let bound = Checks {
            prepare: 1,
            ..Checks::default()
        };
        assert_eq!(probe.checks(), bound);
        let rate = VideoRate {
            bitrate: 1_000_000,
            pace_fps: 30,
        };
        backend
            .capture_video(&context, VideoCodec::H264, true, rate)
            .unwrap();
        assert_eq!(probe.checks(), recent(2));
        probe.point(-1900, 10);
        assert!(backend.cursor(&context).unwrap().is_some());
        assert_eq!(probe.checks(), recent(2));

        backend
            .input_in_context(&context, &Input::Move { x: 3, y: 4 })
            .unwrap();
        assert_eq!(probe.checks(), fresh(1));
        // With no journal it changes nothing, after its check; a change,
        // made or failed, and a restore leave capture and the pointer
        // nothing to reuse.
        let mode = Mode {
            width: 32,
            height: 24,
        };
        assert!(backend.set_display_mode(&context, mode).is_err());
        let forgot = |checks| Checks {
            forgets: 1,
            ..checks
        };
        assert_eq!(probe.checks(), forgot(fresh(1)));
        assert!(!backend.restore_display_mode().unwrap());
        assert_eq!(probe.checks(), forgot(Checks::default()));
    }
}
