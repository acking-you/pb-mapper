//! The product's backend behind the owned window's guard.
use crate::fixture::State;
use pb_mapper_desktop::{
    Backend, CursorPosition, Description, DesktopContext, Display, DoubleClickGuard, EncodedFrame,
    Input, Mode, ModeList, Result, VideoCodec, VideoLimits, VideoRate, Waker,
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
/// The product's backend behind the window guard. Every `Backend` method
/// is forwarded, because a default left in place would quietly change the
/// session: no encoder drain, no cursor, no drag-start hold.
pub(crate) struct Guarded {
    pub(crate) inner: pb_mapper_desktop::NativeBackend,
    pub(crate) state: Arc<State>,
}
impl Guarded {
    /// One capture-side call between two guard checks, timed for the
    /// diagnostics.
    fn timed<T>(
        &mut self,
        call: impl FnOnce(&mut pb_mapper_desktop::NativeBackend) -> Result<T>,
    ) -> Result<T> {
        self.state.check()?;
        let started = Instant::now();
        let value = call(&mut self.inner)?;
        self.state
            .capture_ms
            .store(started.elapsed().as_millis() as u32, Ordering::Release);
        self.state.check()?;
        Ok(value)
    }
}
#[deny(clippy::missing_trait_methods)]
impl Backend for Guarded {
    fn describe(&mut self, probe_h265: bool) -> Result<Description> {
        self.state.check()?;
        self.inner.describe(probe_h265)
    }
    fn bind(&mut self, waker: Waker) {
        self.inner.bind(waker);
    }
    fn configure_video(&mut self, limits: VideoLimits, nominal_fps: u32) -> Result<()> {
        self.inner.configure_video(limits, nominal_fps)
    }
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>> {
        self.timed(|inner| inner.capture_consistent(expected))
    }
    fn drain_poll(&self) -> Option<Duration> {
        self.inner.drain_poll()
    }
    fn display_modes(&mut self, expected: &DesktopContext) -> Result<Option<ModeList>> {
        self.inner.display_modes(expected)
    }
    fn set_display_mode(&mut self, expected: &DesktopContext, mode: Mode) -> Result<()> {
        self.inner.set_display_mode(expected, mode)
    }
    fn restore_display_mode(&mut self) -> Result<bool> {
        self.inner.restore_display_mode()
    }
    fn display(&mut self) -> Result<Display> {
        self.state.check()?;
        self.inner.display()
    }
    fn context(&mut self) -> Result<DesktopContext> {
        self.state.check()?;
        self.inner.context()
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        self.timed(|inner| inner.capture())
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<CursorPosition>> {
        self.inner.cursor(expected)
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        self.timed(|inner| inner.capture_video(expected, codec, keyframe, rate))
    }
    fn drain_video(
        &mut self,
        expected: &DesktopContext,
        wait: Duration,
    ) -> Result<Option<EncodedFrame>> {
        self.timed(|inner| inner.drain_video(expected, wait))
    }
    fn pending_video(&self) -> usize {
        self.inner.pending_video()
    }
    fn input(&mut self, input: &Input) -> Result<()> {
        let context = self.context()?;
        self.input_in_context(&context, input)
    }
    fn input_in_context(&mut self, expected: &DesktopContext, input: &Input) -> Result<()> {
        self.state
            .guard_input(&expected.display, input)
            .and_then(|()| self.inner.input_in_context(expected, input))
            .inspect_err(|e| *self.state.input_error.lock() = Some(e.to_string()))?;
        self.state.applied.fetch_add(1, Ordering::Release);
        Ok(())
    }
    fn presented(&mut self, expected: &DesktopContext) -> Result<()> {
        self.inner.presented(expected)
    }
    fn release(&mut self) {
        self.inner.release();
    }
    fn double_click_guard(&mut self) -> Option<DoubleClickGuard> {
        self.inner.double_click_guard()
    }
}
