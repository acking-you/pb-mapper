//! The out-of-process backend's client: a [`Backend`] whose calls cross a
//! transport to a worker that serves its own backend (`server::work`).
//! Windows connects it over the helper's named pipe; any platform with a
//! privileged agent can connect it over its own transport.
//!
//! One I/O thread owns the transport and the runtime that drives it
//! ([`Link`]): it writes each call's request, and reads whatever the worker
//! writes, in order: the call's answer, which goes back to the waiting call,
//! and the pushes between answers. A pushed unit or failure waits in a
//! queue that thread C drains without crossing the pipe, a fresh picture
//! for an armed tick is a flag, and a pushed pointer replaces the last
//! one; each wakes thread C, the pointer when it changed what thread C
//! would read (`Backend::cursor_changed`). What depends on the order of
//! answers and pushes (the queue, the pointer, the units the worker's
//! encoder holds) is kept by the I/O thread as each arrives, so that an
//! answer thread C reads late never undoes a push that came after it.
use super::{
    Transport, ipc,
    proto::{
        AdapterKey, PENDING_MAX, PRESENT_AGE_MAX, Record, Request, Response, ShapeHeader, inside,
        presented_at,
    },
};
use crate::{
    Backend, BackendCaps, BackendError, CursorPosition, Description, DesktopContext, EncodedFrame,
    FailureKind, Input, Presented, Result, VideoCodec, VideoLimits, Waker,
    codec::pipeline::LIVENESS_INTERVAL,
    error,
    modes::{Mode, ModeList},
    worker::ENCODER_DEPTH,
};
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime},
};
use tokio_util::sync::CancellationToken;

/// The most units and failures the worker may have pushed that thread C has
/// not taken yet: thread C submits only while the worker's encoder and this
/// queue hold fewer than `ENCODER_DEPTH` together, so a worker that pushes
/// more is broken, and its link is dropped.
const QUEUED_MAX: usize = ENCODER_DEPTH + 2;

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
static H265_ANSWERS: Mutex<H265Answers> = Mutex::new(H265Answers::new());

pub(crate) struct RemoteBackend {
    link: Link,
    /// What the worker's backend can do, as its `Start` answer said; what
    /// the connector expects of its worker until then.
    caps: BackendCaps,
    context: Option<DesktopContext>,
    /// Whether the last capture handed the worker's source its tick, as its
    /// answer said (`submitted_video`).
    submitted: bool,
    /// When the answer to the last capture that left the tick armed came,
    /// and whether that capture asked for a keyframe, which the worker's
    /// encoder then keeps; `None` once a capture spent the tick.
    armed: Option<(Instant, bool)>,
    /// The keyframe the capture under way asked for.
    keyframe: bool,
    /// When the display presented the picture the last capture submitted,
    /// as its answer dated it.
    presented: Option<Presented>,
    /// From the `Start` response: the interactive session's values, which
    /// differ from this process's when it is the SYSTEM service in session 0.
    double_click: Option<crate::DoubleClickGuard>,
    /// The GPU and driver under which this session's H.265 answer is kept
    /// for the process, which an H.265 encoder that fails here drops.
    adapter: Option<AdapterKey>,
    /// This client changed the worker's display mode and has not asked it
    /// back since: a session's end asks only then.
    changed_mode: bool,
    /// The worker streams (`Request::Stream`): thread C bound this backend
    /// and is not paused.
    streaming: bool,
    sequence: u64,
    failed: bool,
    /// Thread C asked for the pointer's shape, so the worker reads it.
    shapes: bool,
    /// The newest shape's number this client fetched: a newer one the worker
    /// reports is fetched once.
    fetched_shape: u64,
}
impl RemoteBackend {
    /// Start the worker on `display_id`, found again by `display_device`
    /// when that id went stale, over `pipe`, which `runtime` drives on the
    /// backend's I/O thread. `caps` is what the connector expects of the
    /// worker, which its `Start` answer replaces.
    pub(crate) fn start<T: Transport>(
        runtime: tokio::runtime::Runtime,
        pipe: T,
        display_id: u32,
        display_device: Option<String>,
        caps: BackendCaps,
    ) -> Result<Self> {
        let mut backend = Self::new(runtime, pipe, caps)?;
        backend.call(Request::Start {
            display_id,
            display_device,
        })?;
        Ok(backend)
    }
    fn new<T: Transport>(
        runtime: tokio::runtime::Runtime,
        pipe: T,
        caps: BackendCaps,
    ) -> Result<Self> {
        Ok(Self {
            link: Link::start(runtime, pipe)?,
            caps,
            context: None,
            submitted: true,
            armed: None,
            keyframe: false,
            presented: None,
            double_click: None,
            adapter: None,
            changed_mode: false,
            streaming: false,
            sequence: 0,
            failed: false,
            shapes: false,
            fetched_shape: 0,
        })
    }

    /// One request and its answer. A lost or stalled pipe, or a worker that
    /// breaks the protocol, fails the call and every later one with
    /// `NotConnected`, which ends the session: a reinstalled or crashed
    /// helper never leaves a viewer waiting. The desktop the helper last
    /// reported stays until an answer reports another: Release, Close,
    /// Stream and RestoreDisplayMode report none; and an error, of the kind
    /// the worker said, answers the request, not the desktop. The engine's
    /// own context checks notice a desktop change.
    fn call(&mut self, request: Request) -> Result<Answer> {
        if self.failed {
            return Err(disconnected("helper disconnected; restart desktop sharing"));
        }
        let bound = request.timeout();
        let owed = Owed {
            reports_context: request.reports_context(),
            observes_cursor: request.observes_cursor(),
            input: matches!(request, Request::Input { .. }),
            capture: match &request {
                Request::Video { expected, .. } => Some(expected.generation),
                _ => None,
            },
            configures_video: matches!(request, Request::ConfigureVideo { .. }),
        };
        let (reply, answer) = mpsc::sync_channel(1);
        if self
            .link
            .calls
            .try_send(Call {
                request,
                owed,
                reply,
            })
            .is_err()
        {
            return Err(self.fail("helper link ended"));
        }
        match answer.recv_timeout(bound) {
            Ok(delivered) => self.absorb(owed, delivered),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(self.fail(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "helper call timed out",
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.fail("helper link ended")),
        }
    }

    /// Keep what the answer `delivered` says of the worker that the I/O
    /// thread did not keep already, and hand the caller the rest.
    fn absorb(&mut self, owed: Owed, delivered: Delivered) -> Result<Answer> {
        let Delivered {
            response,
            image,
            received,
        } = delivered;
        let Response {
            context,
            error: reason,
            failure,
            // A capture's unit went to the queue, in its order.
            video: _,
            submitted,
            // The I/O thread keeps the pointer and the units pending.
            cursor: _,
            pending: _,
            double_click,
            caps,
            h265,
            adapter,
            retuned: _,
            modes,
            restored,
            shape_serial,
            shape,
            presented_ago_us,
            presented_count,
        } = response;
        // Only a capture's answer says whether it left the tick armed (it
        // found nothing new, or an encoder not yet asking for input); absent,
        // as every failure answers, the tick was spent, on the picture the
        // answer dates if its source could.
        if owed.capture.is_some() {
            self.submitted = submitted.unwrap_or(true);
            self.armed = (!self.submitted).then_some((received, self.keyframe));
            self.presented = self
                .submitted
                .then(|| presented_at(presented_ago_us, presented_count, received))
                .flatten();
        }
        if let Some(reason) = reason {
            return Err(self.failure(reason, failure));
        }
        if owed.reports_context {
            self.context = context;
        }
        self.double_click = double_click.or(self.double_click);
        if let Some(caps) = caps {
            self.caps = caps.into();
        }
        Ok(Answer {
            image,
            h265,
            adapter,
            modes,
            restored,
            shape_serial,
            shape,
        })
    }

    /// The worker's failure `reason`, of the kind it said. Thread C turns
    /// H.265 off for this process on an H.265 encoder's; the answer kept for
    /// this GPU goes too, so nothing reuses it.
    fn failure(&self, reason: String, failure: Option<FailureKind>) -> std::io::Error {
        if failure
            == Some(FailureKind::EncoderFailed {
                codec: VideoCodec::H265,
            })
            && let Some(adapter) = &self.adapter
        {
            H265_ANSWERS.lock().forget(adapter);
        }
        match failure {
            Some(kind) => BackendError::new(kind, reason).into(),
            None => error(reason),
        }
    }

    /// Whether the worker encodes H.265: what a worker on the GPU and driver
    /// that `Request::Adapter` names answered earlier in this process, else
    /// what this one answers `Request::H265`, which is kept under the GPU
    /// its probe names, and only then. A call that fails says no, and keeps
    /// nothing.
    fn h265(&mut self) -> bool {
        let started = Instant::now();
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

    /// The link is lost: every later call fails, and the I/O thread stops.
    fn fail(
        &mut self,
        failure: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> std::io::Error {
        self.failed = true;
        self.link.close();
        disconnected(failure)
    }

    /// Fail when the I/O thread lost the link, as a call then would.
    fn check_link(&mut self) -> Result<()> {
        if self.failed {
            return Err(disconnected("helper disconnected; restart desktop sharing"));
        }
        if self.link.shared.lock().lost {
            return Err(self.fail("helper link ended"));
        }
        Ok(())
    }

    /// Tell the worker whether thread C wants video, once per change. The
    /// worker drops the tick its last capture armed when the stream stops,
    /// and its capture thread stops looking, so the arming goes here too:
    /// the first tick after a resume captures, as in process.
    fn stream(&mut self, on: bool) {
        if on != self.streaming && self.call(Request::Stream { on }).is_ok() {
            self.streaming = on;
            self.armed = None;
        }
    }
}

/// What one answer carried for its caller alone; what the worker holds
/// across calls, the backend or its I/O thread keeps.
struct Answer {
    /// The still or the shape's bitmap after the answer; empty when it has
    /// none.
    image: Vec<u8>,
    /// In the `H265` response.
    h265: Option<bool>,
    /// In the `Adapter` response, and in the `H265` response when its
    /// answer is a probe's.
    adapter: Option<AdapterKey>,
    /// In the `DisplayModes` response.
    modes: Option<ModeList>,
    /// In the `RestoreDisplayMode` response.
    restored: Option<bool>,
    /// The newest shape's number, in an answer that observed the pointer.
    shape_serial: Option<u64>,
    /// In the `CursorShape` response: `image`'s shape.
    shape: Option<ShapeHeader>,
}
/// `failure`, which ends the session, kept as the cause.
fn disconnected(failure: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotConnected, failure)
}
#[deny(clippy::missing_trait_methods)]
impl Backend for RemoteBackend {
    fn configure_video(&mut self, limits: VideoLimits, nominal_fps: u32) -> Result<()> {
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
    /// The pointer as the worker last pushed it, or an answer that observed
    /// it carried it, whichever came last, on the desktop the worker last
    /// reported.
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        self.check_link()?;
        let mut pushed = self.link.shared.lock();
        pushed.cursor_changed = false;
        if self.context.as_ref() != Some(expected) {
            return Err(error("helper cursor belongs to a different desktop"));
        }
        Ok(pushed.cursor)
    }
    /// The worker pushed a pointer, or a shape's number, other than the one
    /// thread C last read; the I/O thread woke thread C for it.
    fn cursor_changed(&self) -> bool {
        !self.failed && self.link.shared.lock().cursor_changed
    }
    /// The first call asks the worker to read shapes from then on; later
    /// ones cross the pipe only when the worker numbered a newer shape. The
    /// worker's answer is checked here: a shape out of bounds, or a bitmap
    /// of another length, is refused as `InvalidData`, and not asked again.
    fn cursor_shape(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorShape>> {
        self.check_link()?;
        if self.context.as_ref() != Some(expected) {
            return Err(error("helper cursor belongs to a different desktop"));
        }
        if self.shapes && self.link.shared.lock().shape_serial == self.fetched_shape {
            return Ok(None);
        }
        // Until an ask is answered, the worker may not read shapes yet: the
        // next tick asks again.
        let answer = self.call(Request::CursorShape {})?;
        self.shapes = true;
        self.fetched_shape = answer.shape_serial.unwrap_or(0);
        answer
            .shape
            .map(|shape| shape.shape(answer.image))
            .transpose()
    }
    /// Submit one capture on the worker, then hand thread C the oldest unit
    /// the worker finished: one it pushed before the answer, or the one the
    /// answer carried.
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: crate::VideoCodec,
        keyframe: bool,
        rate: crate::VideoRate,
    ) -> Result<Option<crate::EncodedFrame>> {
        self.keyframe = keyframe;
        self.call(Request::Video {
            expected: expected.clone(),
            codec,
            keyframe,
            rate,
        })?;
        self.drain_video(expected, Duration::ZERO)
    }
    /// The oldest unit the worker pushed, with no pipe call; one of another
    /// desktop is dropped, and a failure the worker pushed is returned in its
    /// order. Never waits.
    fn drain_video(
        &mut self,
        expected: &DesktopContext,
        _wait: Duration,
    ) -> Result<Option<crate::EncodedFrame>> {
        self.check_link()?;
        let taken = {
            let mut pushed = self.link.shared.lock();
            loop {
                match pushed.queue.pop_front() {
                    Some(Queued::Unit { generation, frame })
                        if generation == expected.generation =>
                    {
                        break Ok(Some(frame));
                    }
                    // Captured for a desktop thread C no longer expects.
                    Some(Queued::Unit { .. }) => {}
                    Some(Queued::Failed { error, failure }) => break Err((error, failure)),
                    None => break Ok(None),
                }
            }
        };
        taken.map_err(|(reason, failure)| self.failure(reason, failure))
    }
    /// What the worker's encoder holds, as its last answer or push said,
    /// and what it pushed that thread C has not taken.
    fn pending_video(&self) -> usize {
        if self.failed {
            return 0;
        }
        let pushed = self.link.shared.lock();
        pushed.reported + pushed.queue.len()
    }
    /// The helper's answer carries no bound of its encoder's own: the
    /// Windows encoder it runs has none below `pending_video`'s.
    fn video_full(&self) -> bool {
        false
    }
    /// What the last capture's answer said: a worker whose source had no new
    /// picture, or whose encoder had not asked for input, leaves the tick
    /// armed, and tells this backend once its source wants it.
    fn submitted_video(&self) -> bool {
        self.submitted
    }
    /// The present the last capture's answer dated, by its age when the
    /// worker wrote it, taken from this answer's receipt: a little later than
    /// the present, by the answer's way through the pipe.
    fn submitted_picture(&self) -> Option<crate::Presented> {
        self.presented
    }
    /// As a source that pushes: none while the tick is spent (the next tick
    /// captures), once the worker said its source wants the armed tick, or
    /// once the link is lost (the next call says so); otherwise the
    /// liveness interval after the answer that armed it, or `keyframe_due`
    /// if sooner, unless the capture that armed it asked for a keyframe
    /// already: the worker's encoder keeps that, and says when it can send
    /// it. The worker's own liveness repeat comes due before the interval
    /// and tells this backend; the interval is the backstop.
    fn capture_idle_until(&self, keyframe_due: Option<Instant>) -> Option<Instant> {
        if self.failed {
            return None;
        }
        let (armed, keyframe) = self.armed?;
        let pushed = self.link.shared.lock();
        if pushed.fresh || pushed.lost {
            return None;
        }
        let until = armed + LIVENESS_INTERVAL;
        Some(match keyframe_due {
            Some(due) if !keyframe => due.min(until),
            Some(_) | None => until,
        })
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
    /// The I/O thread wakes thread C for each unit, failure and fresh
    /// picture the worker pushes, and for a pointer that changed; the
    /// worker streams from now on.
    fn bind(&mut self, waker: Waker) {
        self.link.shared.lock().waker = Some(waker);
        self.stream(true);
    }
    fn capture_consistent(&mut self, expected: &DesktopContext) -> Result<Vec<u8>> {
        crate::backend::capture_checked(self, expected, Self::capture)
    }
    /// The worker pushes each unit as it finishes.
    fn drain_poll(&self) -> Option<Duration> {
        None
    }
    /// The worker says when its source wants an armed tick.
    fn capture_poll(&self) -> Option<Duration> {
        None
    }
    /// The worker stops streaming while every viewer is paused: its capture
    /// thread stops looking, and it pushes neither the pointer nor fresh
    /// pictures until thread C resumes.
    fn video_paused(&mut self, paused: bool) {
        self.stream(!paused);
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
    fn forget_scroll(&mut self) {}
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
impl Drop for RemoteBackend {
    /// Close the worker, then stop the I/O thread (`Link`'s drop).
    fn drop(&mut self) {
        if !self.failed {
            let _ = self.call(Request::Close {});
        }
    }
}

/// The I/O thread, and what it shares with thread C.
struct Link {
    shared: Arc<Mutex<Pushed>>,
    calls: tokio::sync::mpsc::Sender<Call>,
    stop: CancellationToken,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Link {
    /// Start the I/O thread, which drives `pipe` on `runtime`.
    fn start<T: Transport>(runtime: tokio::runtime::Runtime, pipe: T) -> Result<Self> {
        let shared = Arc::new(Mutex::new(Pushed::default()));
        // One call at a time: thread C waits on each.
        let (calls, incoming) = tokio::sync::mpsc::channel(1);
        let stop = CancellationToken::new();
        let thread = std::thread::Builder::new()
            .name("pb-mapper-helper-link".into())
            .spawn({
                let (shared, stop) = (shared.clone(), stop.clone());
                move || runtime.block_on(run(pipe, incoming, &shared, &stop))
            })?;
        Ok(Self {
            shared,
            calls,
            stop,
            thread: Some(thread),
        })
    }

    /// Stop the I/O thread and wait for it, which ends at once: nothing it
    /// waits on outlives the stop.
    fn close(&mut self) {
        self.stop.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Link {
    fn drop(&mut self) {
        self.close();
    }
}

/// One call, handed to the I/O thread.
struct Call {
    request: Request,
    owed: Owed,
    reply: mpsc::SyncSender<Delivered>,
}
/// What the answer to a call changes as the I/O thread receives it, in its
/// order among the pushes.
#[derive(Clone, Copy)]
struct Owed {
    reports_context: bool,
    observes_cursor: bool,
    input: bool,
    /// A capture of the desktop of this generation: its answer ends the
    /// arming that a fresh picture answered, and a unit it carries is
    /// queued for that desktop.
    capture: Option<u64>,
    /// `ConfigureVideo`: what the worker pushed before goes unless its
    /// answer says its encoder stayed open (`retuned: Some(true)`).
    configures_video: bool,
}
/// An answer, as the I/O thread received it.
struct Delivered {
    response: Response,
    image: Vec<u8>,
    received: Instant,
}

/// What the worker pushed, kept by the I/O thread for thread C.
#[derive(Default)]
struct Pushed {
    /// Units and failures, in order, until thread C takes them.
    queue: VecDeque<Queued>,
    /// The units the worker's encoder holds, as its latest record said.
    reported: usize,
    /// The worker's source wants the tick the last capture left armed.
    fresh: bool,
    /// The desktop the worker last reported, inside whose display a pushed
    /// pointer must lie.
    context: Option<DesktopContext>,
    cursor: Option<CursorPosition>,
    /// The newest shape's number the worker reported.
    shape_serial: u64,
    /// A pushed pointer changed `cursor` or `shape_serial` since thread C
    /// last read the pointer.
    cursor_changed: bool,
    /// Thread C's wake, once it bound the backend.
    waker: Option<Waker>,
    /// The link ended: the pipe closed, or the worker broke the protocol.
    lost: bool,
}
impl Pushed {
    /// Queue `queued`, which the worker pushed, and wake thread C for it.
    fn push(&mut self, queued: Queued) -> Result<()> {
        if self.queue.len() >= QUEUED_MAX {
            return Err(error("the helper pushed more units than its encoder holds"));
        }
        self.queue.push_back(queued);
        self.wake();
        Ok(())
    }
    fn wake(&self) {
        if let Some(waker) = &self.waker {
            waker.wake();
        }
    }
}
/// A unit or a failure the worker pushed.
enum Queued {
    Unit {
        generation: u64,
        frame: EncodedFrame,
    },
    Failed {
        error: String,
        failure: Option<FailureKind>,
    },
}

/// The I/O thread: write each call's request, read every record, until
/// `stop`, the backend's end, a lost pipe or a broken record; then the link
/// is lost, thread C is woken, and a call still waiting fails.
async fn run<T: Transport>(
    pipe: T,
    mut calls: tokio::sync::mpsc::Receiver<Call>,
    shared: &Mutex<Pushed>,
    stop: &CancellationToken,
) {
    let (mut reader, mut writer) = tokio::io::split(pipe);
    // The calls written and not yet answered, oldest first: one at a time.
    let owed = RefCell::new(VecDeque::new());
    let requests = async {
        while let Some(Call {
            request,
            owed: effect,
            reply,
        }) = calls.recv().await
        {
            owed.borrow_mut().push_back((effect, reply));
            ipc::write_json(&mut writer, &request).await?;
        }
        Ok::<_, std::io::Error>(())
    };
    let ended: Result<()> = tokio::select! {
        biased;
        () = stop.cancelled() => Ok(()),
        ended = requests => ended,
        ended = read(&mut reader, &owed, shared) => ended,
    };
    let mut pushed = shared.lock();
    pushed.lost = true;
    pushed.wake();
    drop(pushed);
    if let Err(failure) = ended {
        tracing::debug!(
            target: "pb_mapper_desktop::session",
            event = "desktop_helper_link_lost",
            detail = %failure,
        );
    }
}

/// Read the worker's records, each checked as it comes, and keep what each
/// says in `shared`, until one fails.
async fn read<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    owed: &RefCell<VecDeque<(Owed, mpsc::SyncSender<Delivered>)>>,
    shared: &Mutex<Pushed>,
) -> Result<()> {
    let mut stats = PushStats::new(Instant::now());
    loop {
        let record: Record = ipc::read_json(reader).await?;
        let received = Instant::now();
        match record {
            Record::Reply(mut response) => {
                let mut image = ipc::read_bytes(reader, ipc::MAX_FRAME).await?;
                let (effect, reply) = owed
                    .borrow_mut()
                    .pop_front()
                    .ok_or_else(|| error("an answer to no request"))?;
                let pending = checked(response.pending)?;
                let mut pushed = shared.lock();
                pushed.reported = pending;
                if response.error.is_none() {
                    if effect.reports_context && pushed.context != response.context {
                        pushed.context.clone_from(&response.context);
                    }
                    if effect.observes_cursor || effect.input && response.cursor.is_some() {
                        pushed.cursor = response.cursor;
                    }
                    if effect.observes_cursor
                        && let Some(serial) = response.shape_serial
                    {
                        pushed.shape_serial = serial;
                    }
                }
                if effect.configures_video && response.retuned != Some(true) {
                    pushed.queue.clear();
                }
                if let Some(generation) = effect.capture {
                    // Any fresh picture told before this answer is the one
                    // this capture took, or found it had taken.
                    pushed.fresh = false;
                    // The unit the worker drained before it submitted: the
                    // next in order.
                    if let Some(format) = response.video.take() {
                        let bytes = std::mem::take(&mut image);
                        pushed.push(Queued::Unit {
                            generation,
                            frame: EncodedFrame { format, bytes },
                        })?;
                    }
                }
                drop(pushed);
                stats.replies += 1;
                // A call that timed out meanwhile no longer listens.
                let _ = reply.send(Delivered {
                    response: *response,
                    image,
                    received,
                });
            }
            Record::Unit {
                generation,
                video,
                pending,
                collected_unix_us,
            } => {
                let bytes = ipc::read_bytes(reader, ipc::MAX_FRAME).await?;
                let pending = checked(pending)?;
                stats.unit(collected_unix_us);
                let mut pushed = shared.lock();
                pushed.reported = pending;
                pushed.push(Queued::Unit {
                    generation,
                    frame: EncodedFrame {
                        format: video,
                        bytes,
                    },
                })?;
            }
            Record::Failed {
                error,
                failure,
                pending,
            } => {
                let pending = checked(pending)?;
                let mut pushed = shared.lock();
                pushed.reported = pending;
                pushed.push(Queued::Failed { error, failure })?;
            }
            Record::Pending { pending } => {
                let pending = checked(pending)?;
                let mut pushed = shared.lock();
                let freed = pending < pushed.reported;
                pushed.reported = pending;
                if freed {
                    pushed.wake();
                }
            }
            Record::Fresh {
                presented_ago_us,
                presented_count: _,
            } => {
                // Thread C dates what it submits by the capture's answer.
                stats.fresh(presented_ago_us);
                let mut pushed = shared.lock();
                pushed.fresh = true;
                pushed.wake();
            }
            Record::Pointer {
                cursor,
                shape_serial,
            } => {
                let mut pushed = shared.lock();
                let fits = cursor.is_none_or(|cursor| {
                    pushed
                        .context
                        .as_ref()
                        .is_some_and(|context| inside(&cursor, &context.display))
                });
                if fits {
                    let serial = shape_serial.unwrap_or(pushed.shape_serial);
                    // Thread C samples a change at once, as it would have
                    // read it in process.
                    if pushed.cursor != cursor || pushed.shape_serial != serial {
                        pushed.cursor = cursor;
                        pushed.shape_serial = serial;
                        pushed.cursor_changed = true;
                        pushed.wake();
                    }
                    stats.pointers += 1;
                } else {
                    // Read for the desktop the worker reported, which
                    // confines it: a worker that sends one outside is broken,
                    // and its pointer is not shown.
                    stats.refused += 1;
                }
            }
        }
        stats.roll(received);
    }
}

/// `pending` as an answer or a push gave it, within `PENDING_MAX`.
fn checked(pending: u32) -> Result<usize> {
    if pending > PENDING_MAX {
        return Err(error(
            "the helper reported more pending units than it may hold",
        ));
    }
    Ok(pending as usize)
}

/// Where `desktop_helper_push` goes: with the feedback interval.
const PUSH_TARGET: &str = "pb_mapper_desktop::feedback";
/// The samples of each kind `desktop_helper_push` keeps a second: more than a
/// second of units at the highest pace.
const PUSH_SAMPLES: usize = 256;

/// What the I/O thread counts for `desktop_helper_push`, a debug event once
/// a second while records come: never published, and nothing allocated.
struct PushStats {
    started: Instant,
    replies: u32,
    units: u32,
    fresh: u32,
    pointers: u32,
    /// Pointers outside the display the worker reported.
    refused: u32,
    /// From each pushed unit's collection in the worker to its receipt here,
    /// in microseconds, on the system clock both read.
    receipts: [u32; PUSH_SAMPLES],
    receipts_len: usize,
    /// Each fresh picture's age as the worker wrote its notice, in
    /// microseconds.
    ages: [u32; PUSH_SAMPLES],
    ages_len: usize,
}
impl PushStats {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            replies: 0,
            units: 0,
            fresh: 0,
            pointers: 0,
            refused: 0,
            receipts: [0; PUSH_SAMPLES],
            receipts_len: 0,
            ages: [0; PUSH_SAMPLES],
            ages_len: 0,
        }
    }

    /// A unit the worker collected at `collected_unix_us` arrived now.
    fn unit(&mut self, collected_unix_us: Option<u64>) {
        self.units += 1;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|since| u64::try_from(since.as_micros()).ok());
        // A clock stepped back, or one over a second: no sample.
        let receipt = now
            .zip(collected_unix_us)
            .and_then(|(now, collected)| now.checked_sub(collected))
            .filter(|us| *us <= 1_000_000)
            .and_then(|us| u32::try_from(us).ok());
        if let (Some(us), Some(slot)) = (receipt, self.receipts.get_mut(self.receipts_len)) {
            *slot = us;
            self.receipts_len += 1;
        }
    }

    /// A fresh picture's notice arrived, dating it `ago_us` old as the
    /// worker wrote it, within `PRESENT_AGE_MAX`.
    fn fresh(&mut self, ago_us: Option<u32>) {
        self.fresh += 1;
        let age = ago_us.filter(|us| Duration::from_micros((*us).into()) <= PRESENT_AGE_MAX);
        if let (Some(us), Some(slot)) = (age, self.ages.get_mut(self.ages_len)) {
            *slot = us;
            self.ages_len += 1;
        }
    }

    /// `desktop_helper_push` once a second has passed at `now`, then a new
    /// window: per second the answers (the pipe's round trips), units, fresh
    /// pictures and pointers received and the pointers refused, and the
    /// p50, p95 and maximum of each unit's delay from the worker's
    /// collection to its receipt here and of each fresh picture's age.
    fn roll(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        if tracing::enabled!(target: PUSH_TARGET, tracing::Level::DEBUG) {
            let receipts = quantiles(self.receipts.get_mut(..self.receipts_len));
            let ages = quantiles(self.ages.get_mut(..self.ages_len));
            let seconds = elapsed.as_secs_f64();
            let per_second = |count: u32| f64::from(count) / seconds;
            tracing::debug!(
                target: PUSH_TARGET,
                event = "desktop_helper_push",
                replies_per_s = per_second(self.replies),
                units_per_s = per_second(self.units),
                fresh_per_s = per_second(self.fresh),
                pointers_per_s = per_second(self.pointers),
                refused = self.refused,
                receipt_p50_us = receipts.0,
                receipt_p95_us = receipts.1,
                receipt_max_us = receipts.2,
                fresh_age_p50_us = ages.0,
                fresh_age_p95_us = ages.1,
            );
        }
        *self = Self::new(now);
    }
}

/// The p50, p95 and maximum of `samples`, which this sorts.
fn quantiles(samples: Option<&mut [u32]>) -> (Option<u32>, Option<u32>, Option<u32>) {
    let samples = samples.unwrap_or_default();
    samples.sort_unstable();
    let at = |quantile: usize| {
        samples
            .get((samples.len() * quantile / 100).min(samples.len().saturating_sub(1)))
            .copied()
    };
    (at(50), at(95), samples.last().copied())
}

#[cfg(test)]
pub(crate) mod tests;
