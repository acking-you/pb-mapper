//! The out-of-process backend's client: a [`Backend`] whose calls cross a
//! transport to a worker that serves its own backend (`server::serve`).
//! Windows connects it over the helper's named pipe; any platform with a
//! privileged agent can connect it over its own transport.
use super::{
    Transport, ipc,
    proto::{DRAIN_WAIT_MAX_MS, Request, Response, ShapeHeader},
};
use crate::{
    Backend, BackendCaps, BackendError, Description, DesktopContext, Input, Result, Waker, error,
    modes::{Mode, ModeList},
};

pub(crate) struct RemoteBackend<T: Transport> {
    runtime: tokio::runtime::Runtime,
    pipe: Option<T>,
    /// What the worker's backend can do, as its `Start` answer said; what
    /// the connector expects of its worker until then.
    caps: BackendCaps,
    context: Option<DesktopContext>,
    cursor: Option<crate::CursorPosition>,
    /// The worker encoder's pending units, from the last response.
    pending: usize,
    /// From the `Start` response: the interactive session's values, which
    /// differ from this process's when it is the SYSTEM service in session 0.
    double_click: Option<crate::DoubleClickGuard>,
    /// From the `H265` response.
    h265: Option<bool>,
    /// This client changed the worker's display mode and has not asked it
    /// back since: a session's end asks only then.
    changed_mode: bool,
    /// The last capture's codec and rate, repeated by drains.
    codec: crate::VideoCodec,
    rate: crate::VideoRate,
    sequence: u64,
    failed: bool,
    /// Thread C asked for the pointer's shape, so the worker reads it.
    shapes: bool,
    /// The newest shape's number the worker reported, and the one this
    /// client last fetched: a newer one is fetched once.
    shape_serial: u64,
    fetched_shape: u64,
}
impl<T: Transport> RemoteBackend<T> {
    /// Start the worker on `display_id`, found again by `display_device`
    /// when that id went stale, over `pipe`, whose calls `runtime` drives.
    /// `caps` is what the connector expects of the worker, which its
    /// `Start` answer replaces.
    pub(crate) fn start(
        runtime: tokio::runtime::Runtime,
        pipe: T,
        display_id: u32,
        display_device: Option<String>,
        caps: BackendCaps,
    ) -> Result<Self> {
        let mut backend = Self::new(runtime, pipe, caps);
        backend.call(Request::Start {
            display_id,
            display_device,
        })?;
        Ok(backend)
    }
    fn new(runtime: tokio::runtime::Runtime, pipe: T, caps: BackendCaps) -> Self {
        Self {
            runtime,
            pipe: Some(pipe),
            caps,
            context: None,
            cursor: None,
            pending: 0,
            double_click: None,
            h265: None,
            changed_mode: false,
            codec: crate::VideoCodec::H264,
            rate: crate::VideoRate {
                bitrate: 0,
                pace_fps: 0,
            },
            sequence: 0,
            failed: false,
            shapes: false,
            shape_serial: 0,
            fetched_shape: 0,
        }
    }

    /// One request and its response. A lost or stalled pipe fails every later
    /// call with `NotConnected`, which ends the session: a reinstalled or
    /// crashed helper never leaves a viewer waiting. The desktop and pointer
    /// the helper last reported stay until a response reports others: Release,
    /// Close and RestoreDisplayMode report no desktop; Context, Capture and
    /// Video observe the pointer, and so does an applied input when the
    /// worker could read it, so the pointer is never older than the last
    /// input; and an error, of the kind the worker said, answers the
    /// request, not the desktop. The engine's own context checks notice a
    /// desktop change.
    fn call(&mut self, request: Request) -> Result<Answer> {
        if self.failed {
            return Err(disconnected("helper disconnected; restart desktop sharing"));
        }
        let bound = request.timeout();
        let exchanged = self.runtime.block_on(async {
            tokio::time::timeout(bound, async {
                let pipe = self
                    .pipe
                    .as_mut()
                    .ok_or_else(|| error("helper disconnected"))?;
                ipc::write_json(pipe, &request).await?;
                let response: Response = ipc::read_json(pipe).await?;
                let image = ipc::read_bytes(pipe, ipc::MAX_FRAME).await?;
                Ok::<_, std::io::Error>((response, image))
            })
            .await
        });
        match exchanged {
            Ok(Ok((response, image))) => self.absorb(&request, response, image),
            Ok(Err(failure)) => Err(self.fail(failure)),
            Err(elapsed) => Err(self.fail(elapsed)),
        }
    }

    /// Keep what `response` to `request` says of the worker, and hand the
    /// caller the rest.
    fn absorb(&mut self, request: &Request, response: Response, image: Vec<u8>) -> Result<Answer> {
        let Response {
            context,
            error: reason,
            failure,
            video,
            cursor,
            pending,
            double_click,
            caps,
            h265,
            modes,
            restored,
            shape_serial,
            shape,
        } = response;
        self.pending = pending as usize;
        if let Some(reason) = reason {
            return Err(match failure {
                Some(kind) => BackendError::new(kind, reason).into(),
                None => error(reason),
            });
        }
        if request.reports_context() {
            self.context = context;
        }
        let applied = matches!(request, Request::Input { .. }) && cursor.is_some();
        if request.observes_cursor() || applied {
            self.cursor = cursor;
        }
        if request.observes_cursor()
            && let Some(serial) = shape_serial
        {
            self.shape_serial = serial;
        }
        self.double_click = double_click.or(self.double_click);
        if let Some(caps) = caps {
            self.caps = caps.into();
        }
        self.h265 = h265.or(self.h265);
        Ok(Answer {
            image,
            video,
            modes,
            restored,
            shape,
        })
    }

    fn fail(
        &mut self,
        failure: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> std::io::Error {
        self.failed = true;
        self.cursor = None;
        self.pending = 0;
        self.pipe.take();
        disconnected(failure)
    }

    fn video(
        &mut self,
        expected: &DesktopContext,
        submit: bool,
        keyframe: bool,
        wait: std::time::Duration,
    ) -> Result<Option<crate::EncodedFrame>> {
        let answer = self.call(Request::Video {
            expected: expected.clone(),
            codec: self.codec,
            submit,
            keyframe,
            rate: self.rate,
            wait_ms: u32::try_from(wait.as_millis())
                .unwrap_or(u32::MAX)
                .min(DRAIN_WAIT_MAX_MS),
        })?;
        Ok(answer.video.map(|format| crate::EncodedFrame {
            format,
            bytes: answer.image,
        }))
    }
}

/// What one response carried for its caller alone; what the worker holds
/// across calls, the backend keeps.
struct Answer {
    /// The unit or still after the response; empty when it has none.
    image: Vec<u8>,
    /// In a `Video` response that drained a unit: `image`'s format.
    video: Option<crate::VideoFormat>,
    /// In the `DisplayModes` response.
    modes: Option<ModeList>,
    /// In the `RestoreDisplayMode` response.
    restored: Option<bool>,
    /// In the `CursorShape` response: `image`'s shape.
    shape: Option<ShapeHeader>,
}
/// `failure`, which ends the session, kept as the cause.
fn disconnected(failure: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotConnected, failure)
}
#[deny(clippy::missing_trait_methods)]
impl<T: Transport> Backend for RemoteBackend<T> {
    fn configure_video(&mut self, limits: crate::VideoLimits, nominal_fps: u32) -> Result<()> {
        limits.validate()?;
        self.call(Request::ConfigureVideo {
            limits,
            nominal_fps,
        })?;
        Ok(())
    }
    fn display(&mut self) -> Result<crate::Display> {
        Ok(self.context()?.display)
    }
    fn context(&mut self) -> Result<DesktopContext> {
        self.call(Request::Context {})?;
        self.context
            .clone()
            .ok_or_else(|| error("helper did not report desktop identity"))
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        Ok(self.call(Request::Capture {})?.image)
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        if self.failed {
            return Err(disconnected("helper disconnected; restart desktop sharing"));
        }
        if self.context.as_ref() != Some(expected) {
            return Err(error("helper cursor belongs to a different desktop"));
        }
        Ok(self.cursor)
    }
    /// The first call asks the worker to read shapes from then on; later
    /// ones cross the pipe only when an answer numbered a newer shape. The
    /// worker's answer is checked here: a shape out of bounds, or a bitmap
    /// of another length, is refused as `InvalidData`, and not asked again.
    fn cursor_shape(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorShape>> {
        if self.failed {
            return Err(disconnected("helper disconnected; restart desktop sharing"));
        }
        if self.context.as_ref() != Some(expected) {
            return Err(error("helper cursor belongs to a different desktop"));
        }
        if self.shapes && self.shape_serial == self.fetched_shape {
            return Ok(None);
        }
        // Until an ask is answered, the worker may not read shapes yet: the
        // next tick asks again.
        let answer = self.call(Request::CursorShape {})?;
        self.shapes = true;
        self.fetched_shape = self.shape_serial;
        answer
            .shape
            .map(|shape| shape.shape(answer.image))
            .transpose()
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: crate::VideoCodec,
        keyframe: bool,
        rate: crate::VideoRate,
    ) -> Result<Option<crate::EncodedFrame>> {
        self.codec = codec;
        self.rate = rate;
        self.video(expected, true, keyframe, std::time::Duration::ZERO)
    }
    fn drain_video(
        &mut self,
        expected: &DesktopContext,
        wait: std::time::Duration,
    ) -> Result<Option<crate::EncodedFrame>> {
        self.video(expected, false, false, wait)
    }
    fn pending_video(&self) -> usize {
        self.pending
    }
    /// What the worker said it can do when it started; it probes the GPU of
    /// the display it captures, in the interactive session, which the
    /// background host in session 0 cannot, and is asked about H.265 only
    /// for a viewer that decodes it.
    fn describe(&mut self, probe_h265: bool) -> Result<Description> {
        let display = self.display()?;
        let h265 = probe_h265 && self.call(Request::H265 {}).is_ok() && self.h265 == Some(true);
        Ok(Description {
            display,
            caps: BackendCaps {
                h265,
                ..self.caps.clone()
            },
        })
    }
    /// The worker wakes nothing here: its output is drained by polling.
    fn bind(&mut self, _waker: Waker) {}
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>> {
        crate::backend::capture_checked(self, expected, Self::capture)
    }
    fn drain_poll(&self) -> Option<std::time::Duration> {
        Some(crate::DRAIN_POLL)
    }
    fn input(&mut self, input: &Input) -> Result<()> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| error("no current helper frame"))?
            .clone();
        self.input_in_context(&context, input)
    }
    fn input_in_context(&mut self, expected: &DesktopContext, input: &Input) -> Result<()> {
        input.validate(&expected.display)?;
        self.sequence += 1;
        self.call(Request::Input {
            expected: expected.clone(),
            sequence: self.sequence,
            input: input.clone(),
        })?;
        Ok(())
    }
    fn presented(&mut self, expected: &DesktopContext) -> Result<()> {
        self.call(Request::Presented {
            generation: expected.generation,
        })?;
        Ok(())
    }
    fn release(&mut self) {
        let _ = self.call(Request::Release {});
    }
    /// Never read here: the background host runs in the SYSTEM service in
    /// session 0, whose values are defaults, not the user's.
    fn double_click_guard(&mut self) -> Option<crate::DoubleClickGuard> {
        self.double_click
    }
    fn display_modes(&mut self, expected: &DesktopContext) -> Result<Option<ModeList>> {
        if !self.caps.display_modes {
            return Ok(None);
        }
        let answer = self.call(Request::DisplayModes {
            expected: expected.clone(),
        })?;
        Ok(answer.modes)
    }
    fn set_display_mode(&mut self, expected: &DesktopContext, mode: Mode) -> Result<()> {
        self.call(Request::SetDisplayMode {
            expected: expected.clone(),
            mode,
        })?;
        self.changed_mode = true;
        Ok(())
    }
    /// The worker puts back what it changed; one never changed through this
    /// client is not asked. The worker restores again when it ends.
    fn restore_display_mode(&mut self) -> Result<bool> {
        if !self.changed_mode {
            return Ok(false);
        }
        let answer = self.call(Request::RestoreDisplayMode {})?;
        self.changed_mode = false;
        Ok(answer.restored.unwrap_or(false))
    }
}
impl<T: Transport> Drop for RemoteBackend<T> {
    fn drop(&mut self) {
        if !self.failed {
            let _ = self.call(Request::Close {});
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// A `RemoteBackend` over an in-memory pipe whose worker answers each
    /// request with `answer` and an empty image, until `Close`.
    fn remote(
        answer: impl Fn(&Request) -> Response + Send + 'static,
    ) -> (RemoteBackend<DuplexStream>, std::thread::JoinHandle<()>) {
        let (pipe, mut worker) = tokio::io::duplex(1 << 16);
        let helper = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                loop {
                    let request: Request = ipc::read_json(&mut worker).await.unwrap();
                    ipc::write_json(&mut worker, &answer(&request))
                        .await
                        .unwrap();
                    ipc::write_bytes(&mut worker, &[], ipc::MAX_FRAME)
                        .await
                        .unwrap();
                    if matches!(request, Request::Close {}) {
                        return;
                    }
                }
            });
        });
        (
            RemoteBackend::new(runtime(), pipe, BackendCaps::default()),
            helper,
        )
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn desktop() -> DesktopContext {
        DesktopContext {
            display: crate::Display {
                id: 1,
                name: "helper".into(),
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
            generation: 1,
            desktop: crate::DesktopKind::Ordinary,
        }
    }

    /// The desktop kind crosses the pipe as it always did.
    #[test]
    fn the_desktop_kind_crosses_the_pipe_as_its_string() {
        let secure = DesktopContext {
            desktop: crate::DesktopKind::Secure,
            ..desktop()
        };
        assert_eq!(
            serde_json::to_string(&secure).unwrap(),
            r#"{"display":{"id":1,"name":"helper","x":0,"y":0,"width":64,"height":64},"generation":1,"desktop":"secure"}"#
        );
        let ordinary: DesktopContext = serde_json::from_str(
            r#"{"display":{"id":1,"name":"helper","x":0,"y":0,"width":64,"height":64},"generation":1,"desktop":"ordinary"}"#,
        )
        .unwrap();
        assert_eq!(ordinary, desktop());
    }

    /// Protocol 8 over a pipe to a worker serving a synthetic display: its
    /// `Start` answer says what it can do, H.265 is asked only for a viewer
    /// that decodes it, the pointer's shape crosses once per new shape, its
    /// modes cross, a change and its restore apply there, and a failure
    /// reads back as the kind the worker gave it.
    #[test]
    fn protocol_8_carries_caps_shapes_modes_and_failure_kinds() {
        use crate::{
            native::PlatformBackend,
            platform::{
                Platform,
                synthetic::{Probe, Synthetic},
            },
        };
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let (probe, modes) = Probe::with_modes();
        let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop");
        let asked_h265 = Arc::new(AtomicUsize::new(0));
        let asked_shape = Arc::new(AtomicUsize::new(0));
        let (pipe, mut worker) = tokio::io::duplex(1 << 16);
        let (worker_probe, worker_journal, asked, asked_for_shape) = (
            probe.clone(),
            journal.to_path_buf(),
            asked_h265.clone(),
            asked_shape.clone(),
        );
        let helper = std::thread::spawn(move || {
            runtime().block_on(async move {
                // The service reads the host's `Start`; the worker answers.
                let Request::Start { display_device, .. } =
                    ipc::read_json(&mut worker).await.unwrap()
                else {
                    panic!("expected Start");
                };
                let parts = Synthetic::<false>::open(&worker_probe, &Probe::display()).unwrap();
                let mut backend = PlatformBackend::from_parts(
                    Arc::new(worker_probe),
                    parts,
                    crate::modes::Journal::in_dir(Some(worker_journal)),
                );
                let mut state = super::super::server::ServeState::new();
                super::super::server::exchange(&mut worker, 1, display_device, |request| {
                    if matches!(request, Request::H265 {}) {
                        asked.fetch_add(1, Ordering::SeqCst);
                    }
                    if matches!(request, Request::CursorShape {}) {
                        asked_for_shape.fetch_add(1, Ordering::SeqCst);
                    }
                    let answer = super::super::server::serve(&mut backend, request, &mut state)
                        .unwrap_or_else(|failure| (Response::failed(&failure, 0), vec![]));
                    async move { Ok(answer) }
                })
                .await
                .unwrap();
            });
        });
        let mut backend = RemoteBackend::start(
            runtime(),
            pipe,
            1,
            Some(Probe::display().name),
            BackendCaps::default(),
        )
        .unwrap();
        let described = backend.describe(false).unwrap();
        assert!(described.caps.display_modes && !described.caps.h265);
        assert_eq!(described.caps.os, Some(crate::HostOs::Linux));
        assert_eq!(asked_h265.load(Ordering::SeqCst), 0);
        backend.describe(true).unwrap();
        assert_eq!(asked_h265.load(Ordering::SeqCst), 1);
        assert!(described.caps.cursor_shapes);

        // The first ask turns the worker's shape reads on; after it, only a
        // newer shape crosses again. A pointer on another display is not
        // read: its shape crosses once it is back on the shared one.
        let display = Probe::display();
        probe.point(display.x - 1, 1);
        let shape = |kind| crate::CursorShape::new(kind, 2, 2, (1, 1), vec![3; 16]).unwrap();
        probe.set_shape(shape(crate::CursorKind::Ibeam));
        let context = backend.context().unwrap();
        assert_eq!(backend.cursor_shape(&context).unwrap(), None);
        probe.point(display.x + 1, 1);
        backend.context().unwrap();
        assert_eq!(
            backend.cursor_shape(&context).unwrap(),
            Some(shape(crate::CursorKind::Ibeam))
        );
        backend.context().unwrap();
        assert_eq!(backend.cursor_shape(&context).unwrap(), None);
        probe.set_shape(shape(crate::CursorKind::Custom));
        backend.context().unwrap();
        assert_eq!(
            backend.cursor_shape(&context).unwrap(),
            Some(shape(crate::CursorKind::Custom))
        );
        assert_eq!(asked_shape.load(Ordering::SeqCst), 3);

        let context = backend.context().unwrap();
        let listed = backend.display_modes(&context).unwrap().unwrap();
        assert_eq!(listed.supported, modes);
        backend.set_display_mode(&context, modes[1]).unwrap();
        assert_eq!(backend.display().unwrap().width, modes[1].width);
        assert!(backend.restore_display_mode().unwrap());
        assert_eq!(backend.display().unwrap().width, Probe::display().width);

        probe.lose(true);
        let lost = backend.context().unwrap_err();
        assert_eq!(
            crate::failure_kind(&lost),
            Some(crate::FailureKind::DisplayLost)
        );
        drop(backend);
        helper.join().unwrap();
    }

    /// A lost pipe fails the call that met it and every later one with
    /// `NotConnected`, which ends the session; nothing is sent again.
    #[test]
    fn a_lost_helper_fails_every_later_call_as_not_connected() {
        let (pipe, worker) = tokio::io::duplex(1 << 16);
        drop(worker);
        let mut backend = RemoteBackend::new(runtime(), pipe, BackendCaps::default());
        let lost = backend.context().unwrap_err();
        assert_eq!(lost.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(crate::failure_kind(&lost), None);
        for later in [
            backend.capture().unwrap_err(),
            backend.cursor(&desktop()).unwrap_err(),
        ] {
            assert_eq!(later.kind(), std::io::ErrorKind::NotConnected);
        }
    }

    #[test]
    fn a_helper_error_keeps_the_desktop_and_its_pointer() {
        let desktop = desktop();
        let pointer = crate::CursorPosition {
            x: 3,
            y: 4,
            visible: true,
        };
        // A helper whose LockWorkStation fails: Input gets an error, Release
        // and Close report no desktop, Presented reports it without looking
        // at the pointer, and the rest report both.
        let reported = desktop.clone();
        let (mut backend, helper) = remote(move |request| {
            let mut response = Response::default();
            match request {
                Request::Input { .. } => response.error = Some("lock failed".into()),
                Request::Release {} | Request::Close {} => {}
                Request::Presented { .. } => response.context = Some(reported.clone()),
                _ => {
                    response.context = Some(reported.clone());
                    response.cursor = Some(pointer);
                }
            }
            response
        });
        assert_eq!(backend.context().unwrap(), desktop);
        assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
        // Neither a failed lock nor a release says the desktop changed, and
        // a lease renewal does not observe the pointer.
        assert!(backend.input_in_context(&desktop, &Input::Lock {}).is_err());
        assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
        backend.release();
        assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
        backend.presented(&desktop).unwrap();
        assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
        drop(backend);
        helper.join().unwrap();
    }

    /// A shape whose bitmap is not the size its header gives is refused,
    /// and not asked for again until the worker numbers a newer one.
    #[test]
    fn a_misfit_helper_shape_is_refused() {
        let desktop = desktop();
        let reported = desktop.clone();
        let (mut backend, helper) = remote(move |request| Response {
            context: Some(reported.clone()),
            shape_serial: request.observes_cursor().then_some(1),
            // A pixel's header over the empty record `remote` writes.
            shape: matches!(request, Request::CursorShape {}).then_some(ShapeHeader {
                kind: crate::CursorKind::Arrow,
                width: 1,
                height: 1,
                hot_x: 0,
                hot_y: 0,
            }),
            ..Response::default()
        });
        backend.context().unwrap();
        let refused = backend.cursor_shape(&desktop).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(backend.cursor_shape(&desktop).unwrap(), None);
        drop(backend);
        helper.join().unwrap();
    }

    #[test]
    fn a_helper_hosted_unit_carries_the_encoder_gop() {
        // The worker's encoder read back an endless GOP; thread C reports the
        // GOP of the units it takes from this backend.
        let (mut backend, helper) = remote(|request| Response {
            video: matches!(request, Request::Video { .. }).then(|| crate::VideoFormat {
                codec: crate::VideoCodec::H264,
                width: 64,
                height: 64,
                keyframe: true,
                gop: u32::MAX,
                ..crate::VideoFormat::default()
            }),
            ..Response::default()
        });
        let rate = crate::VideoRate {
            bitrate: 4_000_000,
            pace_fps: 60,
        };
        let unit = backend
            .capture_video(&desktop(), crate::VideoCodec::H264, true, rate)
            .unwrap()
            .unwrap();
        assert_eq!(unit.format.gop, u32::MAX);
        drop(backend);
        helper.join().unwrap();
    }
}
