//! The out-of-process backend's client: a [`Backend`] whose calls cross a
//! transport to a worker that serves its own backend (`server::serve`).
//! Windows connects it over the helper's named pipe; any platform with a
//! privileged agent can connect it over its own transport.
use super::{
    Transport, ipc,
    proto::{AdapterKey, DRAIN_WAIT_MAX_MS, Request, Response, ShapeHeader},
};
use crate::{
    Backend, BackendCaps, BackendError, Description, DesktopContext, FailureKind, Input, Result,
    VideoCodec, Waker, error,
    modes::{Mode, ModeList},
};

/// The most adapters whose H.265 answers this process keeps.
const H265_ANSWERS_KEPT: usize = 8;

/// What workers' probes answered `Request::H265` in this process, by the GPU
/// and driver each probed, oldest first. A worker is a fresh process, whose
/// probe activates an encoder cold; the host outlives it, so a later worker
/// on the same GPU and driver is not asked again. A probe's no is kept as
/// its yes is, as the in-process backend keeps its probe's; a no the worker
/// gave without a probe names no GPU, and is not kept. An H.265 encoder that
/// then fails on that GPU drops its answer (`H265Answers::forget`).
struct H265Answers(Vec<(AdapterKey, bool)>);
impl H265Answers {
    const fn new() -> Self {
        Self(Vec::new())
    }
    fn get(&self, adapter: &AdapterKey) -> Option<bool> {
        self.0
            .iter()
            .find(|(kept, _)| kept == adapter)
            .map(|&(_, h265)| h265)
    }
    /// Keep `h265` for `adapter`, in place of its last answer; when this
    /// process already keeps [`H265_ANSWERS_KEPT`] others, drop the oldest.
    fn put(&mut self, adapter: AdapterKey, h265: bool) {
        self.forget(&adapter);
        if self.0.len() >= H265_ANSWERS_KEPT {
            self.0.remove(0);
        }
        self.0.push((adapter, h265));
    }
    fn forget(&mut self, adapter: &AdapterKey) {
        self.0.retain(|(kept, _)| kept != adapter);
    }
}
static H265_ANSWERS: parking_lot::Mutex<H265Answers> = parking_lot::Mutex::new(H265Answers::new());

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
    /// Whether the last capture handed the worker's source its tick, as its
    /// answer said (`submitted_video`).
    submitted: bool,
    /// From the `Start` response: the interactive session's values, which
    /// differ from this process's when it is the SYSTEM service in session 0.
    double_click: Option<crate::DoubleClickGuard>,
    /// The GPU and driver under which this session's H.265 answer is kept
    /// for the process, which an H.265 encoder that fails here drops.
    adapter: Option<AdapterKey>,
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
            submitted: true,
            double_click: None,
            adapter: None,
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
            submitted,
            cursor,
            pending,
            double_click,
            caps,
            h265,
            adapter,
            modes,
            restored,
            shape_serial,
            shape,
        } = response;
        self.pending = pending as usize;
        // Only a capture's answer says whether it left the tick armed (it
        // found nothing new, or an encoder not yet asking for input); absent,
        // as every failure and every helper before protocol 10 answers, the
        // tick was spent.
        if matches!(request, Request::Video { submit: true, .. }) {
            self.submitted = submitted.unwrap_or(true);
        }
        if let Some(reason) = reason {
            // Thread C turns H.265 off for this process on such a failure; the
            // answer kept for this GPU goes too, so nothing reuses it.
            if failure
                == Some(FailureKind::EncoderFailed {
                    codec: VideoCodec::H265,
                })
                && let Some(adapter) = &self.adapter
            {
                H265_ANSWERS.lock().forget(adapter);
            }
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
        Ok(Answer {
            image,
            video,
            h265,
            adapter,
            modes,
            restored,
            shape,
        })
    }

    /// Whether the worker encodes H.265: what a worker on the GPU and driver
    /// that `Request::Adapter` names answered earlier in this process, else
    /// what this one answers `Request::H265`, which is kept under the GPU
    /// its probe names, and only then. A call that fails says no, and keeps
    /// nothing.
    fn h265(&mut self) -> bool {
        let started = std::time::Instant::now();
        let named = self
            .call(Request::Adapter {})
            .ok()
            .and_then(|answer| answer.adapter);
        let kept = named.and_then(|adapter| H265_ANSWERS.lock().get(&adapter));
        let (h265, adapter) = match kept {
            Some(h265) => (h265, named),
            None => {
                let Ok(Answer {
                    h265: Some(h265),
                    adapter: probed,
                    ..
                }) = self.call(Request::H265 {})
                else {
                    return false;
                };
                if let Some(probed) = probed {
                    H265_ANSWERS.lock().put(probed, h265);
                }
                (h265, probed)
            }
        };
        self.adapter = adapter;
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_h265_helper",
            usable = h265,
            named = named.is_some(),
            kept = kept.is_some(),
            ms = started.elapsed().as_millis() as u64,
        );
        h265
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
    /// In the `H265` response.
    h265: Option<bool>,
    /// In the `Adapter` response, and in the `H265` response when its
    /// answer is a probe's.
    adapter: Option<AdapterKey>,
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
    /// The helper's answer carries no bound of its encoder's own: the
    /// Windows encoder it runs has none below `pending_video`'s.
    fn video_full(&self) -> bool {
        false
    }
    /// What the last capture's answer said: a worker whose source had no new
    /// picture, or whose encoder had not asked for input, leaves the tick
    /// armed, and thread C polls it.
    fn submitted_video(&self) -> bool {
        self.submitted
    }
    fn capture_idle_until(&self, _: Option<std::time::Instant>) -> Option<std::time::Instant> {
        None
    }
    /// What the worker said it can do when it started; it probes the GPU of
    /// the display it captures, in the interactive session, which the
    /// background host in session 0 cannot. Only for a viewer that decodes
    /// H.265 is it asked to name its GPU, and then about H.265, unless a
    /// worker's probe on the same GPU and driver answered that in this
    /// process.
    fn describe(&mut self, probe_h265: bool) -> Result<Description> {
        let display = self.display()?;
        let h265 = probe_h265 && self.h265();
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
    /// The worker's capture is polled as its output is.
    fn capture_poll(&self) -> Option<std::time::Duration> {
        self.drain_poll()
    }
    /// The helper's answer carries no present time (protocol 10): its
    /// pictures pace on the grid.
    fn submitted_picture(&self) -> Option<crate::Presented> {
        None
    }
    /// Nothing to tell the worker, which the protocol has no word for: its
    /// encoder's capture thread, armed by a request that found nothing new,
    /// stops looking once no request armed it again for 2 s
    /// (`present_waiter::ASKED_WITHIN`), and the first request after the
    /// pause looks past any picture it published meanwhile.
    fn video_paused(&mut self, _paused: bool) {}
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
    use crate::{oop::server::NamedAdapter, platform::synthetic::Probe};
    use tokio::io::DuplexStream;

    /// A `RemoteBackend` over an in-memory pipe whose worker answers each
    /// request with `answer` and an empty image, until `Close`.
    fn remote(
        answer: impl Fn(&Request) -> Response + Send + 'static,
    ) -> (RemoteBackend<DuplexStream>, std::thread::JoinHandle<()>) {
        let (pipe, helper) = scripted(answer);
        (
            RemoteBackend::new(runtime(), pipe, BackendCaps::default()),
            helper,
        )
    }

    /// [`remote`], started: its first request is `Start`.
    fn started(
        answer: impl Fn(&Request) -> Response + Send + 'static,
    ) -> (RemoteBackend<DuplexStream>, std::thread::JoinHandle<()>) {
        let (pipe, helper) = scripted(answer);
        let backend =
            RemoteBackend::start(runtime(), pipe, 1, None, BackendCaps::default()).unwrap();
        (backend, helper)
    }

    /// The host's end of an in-memory pipe whose worker answers each request
    /// with `answer` and an empty image, until `Close`.
    fn scripted(
        answer: impl Fn(&Request) -> Response + Send + 'static,
    ) -> (DuplexStream, std::thread::JoinHandle<()>) {
        let (pipe, mut worker) = tokio::io::duplex(1 << 16);
        let helper = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                loop {
                    let request: Request = ipc::read_json(&mut worker).await.unwrap();
                    ipc::write_record(&mut worker, &answer(&request), &[])
                        .await
                        .unwrap();
                    if matches!(request, Request::Close {}) {
                        return;
                    }
                }
            });
        });
        (pipe, helper)
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

    /// A `RemoteBackend` started over an in-memory pipe to a worker that
    /// serves a synthetic display on `probe` through `server::serve`, as the
    /// helper's worker does, on the GPU `adapter` names; `seen` is shown
    /// each request first.
    fn synthetic(
        probe: &Probe,
        journal: std::path::PathBuf,
        adapter: fn(&crate::Display) -> Option<NamedAdapter>,
        mut seen: impl FnMut(&Request) + Send + 'static,
    ) -> (RemoteBackend<DuplexStream>, std::thread::JoinHandle<()>) {
        use crate::{
            native::PlatformBackend,
            platform::{Platform, synthetic::Synthetic},
        };
        let (pipe, mut worker) = tokio::io::duplex(1 << 16);
        let worker_probe = probe.clone();
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
                    std::sync::Arc::new(worker_probe),
                    parts,
                    crate::modes::Journal::in_dir(Some(journal)),
                );
                let mut state = super::super::server::ServeState::new(adapter);
                super::super::server::exchange(&mut worker, 1, display_device, |request| {
                    seen(&request);
                    let answer = super::super::server::serve(&mut backend, request, &mut state)
                        .unwrap_or_else(|failure| (Response::failed(&failure, 0), vec![]));
                    async move { Ok(answer) }
                })
                .await
                .unwrap();
            });
        });
        let backend = RemoteBackend::start(
            runtime(),
            pipe,
            1,
            Some(Probe::display().name),
            BackendCaps::default(),
        )
        .unwrap();
        (backend, helper)
    }

    /// Protocol 8 over a pipe to a worker serving a synthetic display: its
    /// `Start` answer says what it can do, H.265 is asked only for a viewer
    /// that decodes it, the pointer's shape crosses once per new shape, its
    /// modes cross, a change and its restore apply there, and a failure
    /// reads back as the kind the worker gave it.
    #[test]
    fn protocol_8_carries_caps_shapes_modes_and_failure_kinds() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let (probe, modes) = Probe::with_modes();
        let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop");
        let asked_h265 = Arc::new(AtomicUsize::new(0));
        let asked_shape = Arc::new(AtomicUsize::new(0));
        let (asked, asked_for_shape) = (asked_h265.clone(), asked_shape.clone());
        let (mut backend, helper) = synthetic(
            &probe,
            journal.to_path_buf(),
            |_| None,
            move |request| {
                if matches!(request, Request::H265 {}) {
                    asked.fetch_add(1, Ordering::SeqCst);
                }
                if matches!(request, Request::CursorShape {}) {
                    asked_for_shape.fetch_add(1, Ordering::SeqCst);
                }
            },
        );
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

    /// Protocol 10 over a pipe to a worker serving a synthetic display: a
    /// capture that found nothing new says so, and leaves thread C's tick
    /// armed, while still collecting what the encoder finished.
    #[test]
    fn protocol_10_says_when_a_capture_found_nothing_new() {
        let probe = Probe::default();
        let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-still");
        let (mut backend, helper) = synthetic(&probe, journal.to_path_buf(), |_| None, |_| {});
        let context = backend.context().unwrap();
        backend
            .configure_video(crate::VideoLimits::for_viewer(60.0), 60)
            .unwrap();
        let rate = crate::VideoRate {
            bitrate: crate::qos::ENCODER_BITRATE_MIN,
            pace_fps: 60,
        };
        let capture = |backend: &mut RemoteBackend<_>| {
            let unit = backend
                .capture_video(&context, crate::VideoCodec::H264, false, rate)
                .unwrap();
            (unit, backend.submitted_video())
        };
        let (first, submitted) = capture(&mut backend);
        assert!(first.is_none() && submitted);
        probe.still(true);
        let (drained, submitted) = capture(&mut backend);
        assert!(drained.is_some() && !submitted);
        // A drain says nothing of a tick.
        backend
            .drain_video(&context, std::time::Duration::ZERO)
            .unwrap();
        assert!(!backend.submitted_video());
        probe.still(false);
        let (_, submitted) = capture(&mut backend);
        assert!(submitted);
        drop(backend);
        helper.join().unwrap();
    }

    /// `submitted` crosses only when false: a capture whose answer has none,
    /// as no worker before protocol 10 sent one, spent the tick.
    #[test]
    fn an_absent_submitted_means_the_tick_was_spent() {
        assert!(
            !serde_json::to_string(&Response::default())
                .unwrap()
                .contains("submitted")
        );
        let still = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let worker_still = still.clone();
        let reported = desktop();
        let (mut backend, helper) = remote(move |request| {
            let capture = matches!(request, Request::Video { submit: true, .. });
            let still = worker_still.load(std::sync::atomic::Ordering::SeqCst);
            Response {
                context: Some(reported.clone()),
                submitted: (capture && still).then_some(false),
                ..Response::default()
            }
        });
        let rate = crate::VideoRate {
            bitrate: 4_000_000,
            pace_fps: 60,
        };
        let codec = crate::VideoCodec::H264;
        backend
            .capture_video(&desktop(), codec, false, rate)
            .unwrap();
        assert!(!backend.submitted_video());
        still.store(false, std::sync::atomic::Ordering::SeqCst);
        backend
            .capture_video(&desktop(), codec, false, rate)
            .unwrap();
        assert!(backend.submitted_video());
        drop(backend);
        helper.join().unwrap();
    }

    /// Protocol 13 over a pipe to a worker serving a synthetic display: its
    /// `Start` answer says it moves the pointer by counts, which the host
    /// lists as `relative_pointer`, a relative move reaches its injector as
    /// sent, and the answer carries where the move left the pointer.
    #[test]
    fn protocol_13_carries_relative_moves_and_the_relative_pointer() {
        let probe = Probe::default();
        let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-move-by");
        let (mut backend, helper) = synthetic(&probe, journal.to_path_buf(), |_| None, |_| {});
        let caps = backend.describe(false).unwrap().caps;
        assert!(caps.relative_pointer);
        assert!(
            caps.values()
                .iter()
                .any(|listed| listed == "relative_pointer")
        );
        let context = backend.context().unwrap();
        backend
            .input_in_context(&context, &Input::Move { x: 10, y: 10 })
            .unwrap();
        backend
            .input_in_context(&context, &Input::MoveBy { dx: -4, dy: 7 })
            .unwrap();
        assert_eq!(probe.moves_by(), [(-4, 7)]);
        let left = backend.cursor(&context).unwrap().unwrap();
        assert_eq!((left.x, left.y), (6, 17));
        // Both ends hold a relative move to its bound.
        let too_far = Input::MoveBy { dx: 4097, dy: 0 };
        assert!(backend.input_in_context(&context, &too_far).is_err());
        assert_eq!(probe.moves_by().len(), 1);
        drop(backend);
        helper.join().unwrap();
    }

    /// Protocol 12 over a pipe to a worker serving a synthetic display: its
    /// `Start` answer says it scrolls in 1/120 of a notch, which the host
    /// lists as `wheel_hires`, and a scroll reaches its injector as sent.
    #[test]
    fn protocol_12_carries_scrolls_and_the_hires_wheel() {
        let probe = Probe::default();
        let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-scroll");
        let (mut backend, helper) = synthetic(&probe, journal.to_path_buf(), |_| None, |_| {});
        let caps = backend.describe(false).unwrap().caps;
        assert!(caps.hires_wheel);
        assert!(caps.values().iter().any(|listed| listed == "wheel_hires"));
        let context = backend.context().unwrap();
        for (vertical, continuous) in [(-7, true), (120, false)] {
            let scroll = Input::Scroll {
                horizontal: 3,
                vertical,
                continuous,
            };
            backend.input_in_context(&context, &scroll).unwrap();
        }
        assert_eq!(probe.scrolls(), [(3, -7, true), (3, 120, false)]);
        // Both ends hold a scroll to its bound.
        let too_far = Input::Scroll {
            horizontal: 0,
            vertical: 12_001,
            continuous: true,
        };
        assert!(backend.input_in_context(&context, &too_far).is_err());
        assert_eq!(probe.scrolls().len(), 2);
        drop(backend);
        helper.join().unwrap();
    }

    /// Protocol 11 over a pipe to workers serving a synthetic display, one
    /// per session as the helper starts them. A worker names its GPU only
    /// for a viewer that decodes H.265. The first on a GPU is asked about
    /// H.265; a later one on the same GPU and driver is not, and says what
    /// the first's probe said. A new driver is asked again, and so is every
    /// worker that names no GPU, or whose answer was no probe's of it.
    #[test]
    fn a_later_worker_on_the_same_gpu_and_driver_is_not_asked_about_h265() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let probe = Probe::default();
        let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-h265-kept");
        let named = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(AtomicUsize::new(0));
        let session = |adapter: fn(&crate::Display) -> Option<NamedAdapter>| {
            let (named, asked) = (named.clone(), asked.clone());
            let (mut backend, helper) =
                synthetic(&probe, journal.to_path_buf(), adapter, move |request| {
                    match request {
                        Request::Adapter {} => named.fetch_add(1, Ordering::SeqCst),
                        Request::H265 {} => asked.fetch_add(1, Ordering::SeqCst),
                        _ => 0,
                    };
                });
            // The synthetic display encodes H.264 alone.
            assert!(!backend.describe(false).unwrap().caps.h265);
            assert!(!backend.describe(true).unwrap().caps.h265);
            drop(backend);
            helper.join().unwrap();
        };
        // An AMD GPU, on one driver and then on the next, as the worker's
        // probe of it answered; then on a third, found after a probe that
        // had found no GPU for the display.
        fn gpu(driver: u64) -> Option<AdapterKey> {
            AdapterKey::new(0x1002, 0x744c, 0x5315_1849, 0xc8, driver)
        }
        let first_driver: fn(&crate::Display) -> Option<NamedAdapter> = |_| {
            Some(NamedAdapter {
                key: gpu(1)?,
                h265: Some(false),
            })
        };
        let next_driver: fn(&crate::Display) -> Option<NamedAdapter> = |_| {
            Some(NamedAdapter {
                key: gpu(2)?,
                h265: Some(false),
            })
        };
        let unprobed: fn(&crate::Display) -> Option<NamedAdapter> = |_| {
            Some(NamedAdapter {
                key: gpu(3)?,
                h265: None,
            })
        };
        session(first_driver);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        session(first_driver);
        assert_eq!(asked.load(Ordering::SeqCst), 1, "the same GPU and driver");
        session(next_driver);
        assert_eq!(asked.load(Ordering::SeqCst), 2, "a new driver");
        session(next_driver);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        session(unprobed);
        session(unprobed);
        assert_eq!(asked.load(Ordering::SeqCst), 4, "a no without a probe");
        session(|_| None);
        session(|_| None);
        assert_eq!(asked.load(Ordering::SeqCst), 6, "no GPU named");
        assert_eq!(named.load(Ordering::SeqCst), 8, "once per H.265 viewer");
    }

    /// An answer of yes is kept as a no is, until that GPU's H.265 encoder
    /// fails, as thread C then turns H.265 off: the next worker on it is
    /// asked again.
    #[test]
    fn an_h265_encoder_failure_drops_the_answer_kept_for_its_gpu() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        // An Intel GPU on driver 31.0.101.5333.
        let adapter = AdapterKey::new(
            0x8086,
            0x56a0,
            0x1020_8086,
            8,
            (31 << 48) | (101 << 16) | 5333,
        );
        let encoder_failed = FailureKind::EncoderFailed {
            codec: VideoCodec::H265,
        };
        let asked = Arc::new(AtomicUsize::new(0));
        let session = || {
            let asked = asked.clone();
            started(move |request| {
                let mut response = Response {
                    context: Some(desktop()),
                    ..Response::default()
                };
                match request {
                    Request::Adapter {} => response.adapter = adapter,
                    Request::H265 {} => {
                        asked.fetch_add(1, Ordering::SeqCst);
                        response.h265 = Some(true);
                        response.adapter = adapter;
                    }
                    Request::Video { .. } => {
                        response.error = Some("the HEVC encoder took no input".into());
                        response.failure = Some(encoder_failed);
                    }
                    _ => {}
                }
                response
            })
        };
        let rate = crate::VideoRate {
            bitrate: 4_000_000,
            pace_fps: 60,
        };
        for kept in [false, true] {
            let (mut backend, helper) = session();
            assert!(backend.describe(true).unwrap().caps.h265);
            assert_eq!(asked.load(Ordering::SeqCst), 1, "kept: {kept}");
            drop(backend);
            helper.join().unwrap();
        }
        let (mut backend, helper) = session();
        assert!(backend.describe(true).unwrap().caps.h265);
        let Err(failed) = backend.capture_video(&desktop(), VideoCodec::H265, true, rate) else {
            panic!("the worker's encoder failed");
        };
        assert_eq!(crate::failure_kind(&failed), Some(encoder_failed));
        drop(backend);
        helper.join().unwrap();
        let (mut backend, helper) = session();
        assert!(backend.describe(true).unwrap().caps.h265);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        drop(backend);
        helper.join().unwrap();
    }

    /// The process keeps the answers of at most eight GPUs, dropping the
    /// oldest; a GPU answered again replaces its answer.
    #[test]
    fn the_kept_h265_answers_are_bounded() {
        let adapter = |device| AdapterKey::new(0x10de, device, 0, 0, 1).unwrap();
        let mut answers = H265Answers::new();
        for device in 0..=8 {
            answers.put(adapter(device), device % 2 == 0);
        }
        assert_eq!(answers.0.len(), H265_ANSWERS_KEPT);
        assert_eq!(answers.get(&adapter(0)), None);
        for device in 1..=8 {
            assert_eq!(answers.get(&adapter(device)), Some(device % 2 == 0));
        }
        answers.put(adapter(1), true);
        assert_eq!(answers.0.len(), H265_ANSWERS_KEPT);
        assert_eq!(answers.get(&adapter(1)), Some(true));
        assert_eq!(answers.get(&adapter(2)), Some(true));
        answers.forget(&adapter(1));
        assert_eq!(answers.get(&adapter(1)), None);
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
