//! The out-of-process backend's worker side: each request, served on the
//! worker's own backend (`serve`); the worker's loop on its desktop-bound
//! thread (`work`), which serves the host's requests first and then pushes
//! what the host would otherwise have to ask for (finished units, that its
//! source wants the host's armed tick, the pointer); and the exchange over
//! the transport (`exchange`), which writes answers and pushes in the order
//! the loop made them. The worker checks again what the host's thread C
//! already checked: the input sequence, and the whole desktop the host
//! expects, at the injection and capture boundaries.
use super::{
    ipc,
    proto::{AdapterKey, Record, Request, Response, ShapeHeader, TIMEOUT, present_age},
};
use crate::{
    Backend, CursorPosition, CursorShape, DesktopContext, Display, EncodedFrame, Input, Presented,
    Result, error, failure_kind,
    native::PlatformBackend,
    platform::Platform,
    scheduler::Waiter,
    timer::TimerResolution,
    worker::{CURSOR_INTERVAL, CURSOR_MIN, SHAPE_LATE, SHAPE_LOOK},
};
use std::{
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant, SystemTime},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc::Sender,
};

/// Held input is let go once no input or presentation renewed it this long.
const INPUT_LEASE: Duration = Duration::from_secs(2);
/// How soon the worker collects again from an encoder that holds units and
/// wakes it for each output, should none come: as thread C's context check,
/// which bounds how late an encoder that stalled is found out.
const DRAIN_IDLE: Duration = Duration::from_millis(100);
/// The longest the worker waits with nothing due; every wait also ends on
/// a request, and on its source's or encoder's wake.
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// The GPU that drives a display, as the platform names it to the host,
/// which keeps a worker's H.265 answer under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NamedAdapter {
    /// The GPU and its driver.
    pub key: AdapterKey,
    /// What this process's H.265 probe of that GPU answered; `None` while
    /// it has not probed that GPU, as when it found no GPU for the display
    /// then and answered no without a probe.
    pub h265: Option<bool>,
}

/// What the worker keeps across requests.
pub(crate) struct ServeState {
    /// The last input sequence applied.
    pub sequence: u64,
    /// When input was last applied or a presentation renewed it.
    pub last_input: Instant,
    /// The host asked for the pointer's shape: it is read with every pointer
    /// observed from then on. Never before, so a session whose viewer draws
    /// no shapes costs nothing here.
    shapes: bool,
    /// The newest shape read, and how many different ones were.
    shape: Option<CursorShape>,
    shape_serial: u64,
    /// Names the GPU that drives a display, asked only by the `Adapter` and
    /// `H265` requests, which a viewer that decodes H.265 alone causes.
    adapter: fn(&Display) -> Option<NamedAdapter>,
    /// The desktop the last answer reported, which the host keeps until
    /// another answer reports one: the pointer is pushed for it.
    reported: Option<DesktopContext>,
    /// The desktop and pace of the last capture: what finished is collected
    /// for that desktop, and the pointer is read at that pace.
    video: Option<(DesktopContext, u32)>,
    /// The host's thread C wants video (`Request::Stream`).
    streaming: bool,
}
impl ServeState {
    /// `adapter` names the GPU that drives a display; a platform that cannot
    /// name one answers `None`, and the host then asks this worker, and every
    /// later one, about H.265 again.
    pub(crate) fn new(adapter: fn(&Display) -> Option<NamedAdapter>) -> Self {
        Self {
            sequence: 0,
            last_input: Instant::now(),
            shapes: false,
            shape: None,
            shape_serial: 0,
            adapter,
            reported: None,
            video: None,
            streaming: false,
        }
    }
}

/// What the worker's loop hands the exchange to write, in its order: an
/// answer, or a push. Ages are taken as each is written.
pub(crate) enum Outgoing {
    /// The answer to the request served last, and when the display
    /// presented the picture a capture submitted, if its source dated it.
    Reply {
        response: Box<Response>,
        image: Vec<u8>,
        presented: Option<Presented>,
    },
    /// A unit the encoder finished, for the desktop of `generation`, and
    /// when the worker collected it.
    Unit {
        generation: u64,
        frame: EncodedFrame,
        pending: u32,
        collected: SystemTime,
    },
    /// The encoder failed as the worker collected its output.
    Failed {
        failure: std::io::Error,
        pending: u32,
    },
    /// The encoder holds fewer units than reported last, with no unit to
    /// send (a late output discarded).
    Pending { pending: u32 },
    /// The source wants the host's armed tick, with the picture it
    /// published, if it dated one.
    Fresh { presented: Option<Presented> },
    /// The pointer moved, or its shape's number did.
    Pointer {
        cursor: Option<CursorPosition>,
        shape_serial: Option<u64>,
    },
}

/// Hand the host's requests to the worker's loop through `forward`, one at
/// a time, from `Start` until `Close`, and write what the loop sends on
/// `outgoing` as it comes: each request's answer, and the pushes between
/// answers. Each answer is bounded by [`Request::timeout`], and each write
/// by [`TIMEOUT`], but the wait for the next request is not: a host whose
/// viewer is paused sends nothing for as long as the pause lasts. The pipe
/// closing ends that wait, and the service closes it when its caller's pipe
/// closes, or when the owner or its login ends.
pub(crate) async fn exchange<P>(
    pipe: &mut P,
    display_id: u32,
    display_device: Option<String>,
    mut forward: impl FnMut(Request) -> Result<()>,
    outgoing: &mut tokio::sync::mpsc::Receiver<Outgoing>,
) -> Result<()>
where
    P: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(pipe);
    // The loop answered the request it was handed last.
    let answered = tokio::sync::Notify::new();
    let closing = std::sync::atomic::AtomicBool::new(false);
    let requests = async {
        let mut request = Request::Start {
            display_id,
            display_device,
        };
        loop {
            let bound = request.timeout();
            closing.store(
                matches!(request, Request::Close {}),
                std::sync::atomic::Ordering::Relaxed,
            );
            forward(request)?;
            // The host asks again only once answered.
            tokio::time::timeout(bound, answered.notified())
                .await
                .map_err(std::io::Error::other)?;
            request = ipc::read_json(&mut reader).await?;
        }
    };
    let records = async {
        loop {
            let record = outgoing
                .recv()
                .await
                .ok_or_else(|| error("the worker's loop ended"))?;
            let reply = matches!(record, Outgoing::Reply { .. });
            tokio::time::timeout(TIMEOUT, write(&mut writer, record))
                .await
                .map_err(std::io::Error::other)??;
            if reply {
                if closing.load(std::sync::atomic::Ordering::Relaxed) {
                    return Ok(());
                }
                answered.notify_one();
            }
        }
    };
    tokio::select! {
        result = requests => result,
        result = records => result,
    }
}

/// `record` as the host reads it, its ages taken now.
async fn write<W: AsyncWrite + Unpin>(io: &mut W, record: Outgoing) -> Result<()> {
    match record {
        Outgoing::Reply {
            mut response,
            image,
            presented,
        } => {
            (response.presented_ago_us, response.presented_count) =
                present_age(presented, Instant::now());
            ipc::write_record(io, &Record::Reply(response), &image).await
        }
        Outgoing::Unit {
            generation,
            frame: EncodedFrame { format, bytes },
            pending,
            collected,
        } => {
            let collected_unix_us = collected
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .and_then(|since| u64::try_from(since.as_micros()).ok());
            let unit = Record::Unit {
                generation,
                video: format,
                pending,
                collected_unix_us,
            };
            ipc::write_record(io, &unit, &bytes).await
        }
        Outgoing::Failed { failure, pending } => {
            let failed = Record::Failed {
                error: failure.to_string(),
                failure: failure_kind(&failure),
                pending,
            };
            ipc::write_json(io, &failed).await
        }
        Outgoing::Pending { pending } => ipc::write_json(io, &Record::Pending { pending }).await,
        Outgoing::Fresh { presented } => {
            let (presented_ago_us, presented_count) = present_age(presented, Instant::now());
            let fresh = Record::Fresh {
                presented_ago_us,
                presented_count,
            };
            ipc::write_json(io, &fresh).await
        }
        Outgoing::Pointer {
            cursor,
            shape_serial,
        } => {
            ipc::write_json(
                io,
                &Record::Pointer {
                    cursor,
                    shape_serial,
                },
            )
            .await
        }
    }
}

impl Response {
    /// The answer to a request that failed, with the failure's kind.
    pub(crate) fn failed(failure: &std::io::Error, pending: u32) -> Self {
        Self {
            error: Some(failure.to_string()),
            failure: failure_kind(failure),
            pending,
            ..Self::default()
        }
    }
}

/// The worker's loop on its desktop-bound thread, over `backend`, which is
/// bound to the wake `waiter` waits on: until `calls` closes, `stopped`
/// says so or the host asks to close, it serves every request that waits
/// first, then pushes what is due ([`Pushes`]), then waits for a request
/// (the exchange wakes it), its source's or encoder's wake, or its next
/// deadline. What it answers and pushes goes to `out`, in order.
pub(crate) fn work<P: Platform>(
    backend: &mut PlatformBackend<P>,
    state: &mut ServeState,
    calls: &Receiver<Request>,
    out: &Sender<Outgoing>,
    waiter: &Waiter,
    stopped: &dyn Fn() -> bool,
) {
    let mut pushes = Pushes::default();
    while !stopped() {
        let Some(deadline) = pushes.round(backend, state, calls, out) else {
            return;
        };
        if let Err(failure) = waiter.wait(deadline) {
            tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "desktop_worker_wait_failed",
                detail = %failure,
            );
            return;
        }
    }
}

/// What the worker pushes between its answers, and when: each unit as its
/// encoder finishes it; while the host streams, that its source wants the
/// tick the host's last capture left armed, once per arming, and the
/// pointer at thread C's sampling interval, and at the looks at its shape
/// thread C makes after a move, when it or its shape's number changed.
#[derive(Default)]
struct Pushes {
    /// When the last capture left the host's tick armed, until the source
    /// wants it or another capture comes.
    armed: Option<Instant>,
    /// Units the worker's encoder held as of the last answer or push that
    /// named `pending`.
    reported_pending: u32,
    /// The next pointer read while streaming.
    pointer_due: Option<Instant>,
    /// The looks at the pointer's shape still owed to the last move.
    looks: [Option<Instant>; 2],
    /// The pointer and shape number the host holds, as the last push or
    /// answer that carried them left it; `None` pushes the next reading.
    pointer: Option<(Option<CursorPosition>, Option<u64>)>,
    /// The `last_input` held input was let go for, once its lease ran out.
    released: Option<Instant>,
    /// Fine timers while the host streams.
    timer: Option<TimerResolution>,
}
impl Pushes {
    /// Serve what waits, then push what is due: when to look again, or
    /// `None` once the loop ends.
    fn round<P: Platform>(
        &mut self,
        backend: &mut PlatformBackend<P>,
        state: &mut ServeState,
        calls: &Receiver<Request>,
        out: &Sender<Outgoing>,
    ) -> Option<Instant> {
        // Requests first: the host waits on each.
        loop {
            match calls.try_recv() {
                Ok(request) => {
                    if !self.answer(backend, state, request, out) {
                        return None;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return None,
            }
        }
        let now = Instant::now();
        let mut deadline = now + IDLE_WAIT;
        if self.timer.is_some() != state.streaming {
            self.streaming(state.streaming, now);
        }
        // Held input goes once no input or presentation renewed it.
        let lease = state.last_input + INPUT_LEASE;
        if self.released != Some(state.last_input) {
            if now >= lease {
                backend.release();
                self.released = Some(state.last_input);
            } else {
                deadline = deadline.min(lease);
            }
        }
        if let Some(due) = self.units(backend, state, out, now)? {
            deadline = deadline.min(due);
        }
        if state.streaming {
            if let Some(due) = self.fresh(backend, state, out, now)? {
                deadline = deadline.min(due);
            }
            deadline = deadline.min(self.pointer(backend, state, out, now)?);
        }
        Some(deadline)
    }

    /// Serve `request` and send its answer: false once the host closed, or
    /// the exchange ended.
    fn answer<P: Platform>(
        &mut self,
        backend: &mut PlatformBackend<P>,
        state: &mut ServeState,
        request: Request,
        out: &Sender<Outgoing>,
    ) -> bool {
        let close = matches!(request, Request::Close {});
        let capture = matches!(request, Request::Video { .. });
        let configured = matches!(request, Request::ConfigureVideo { .. });
        let observes_cursor = request.observes_cursor();
        let input = matches!(request, Request::Input { .. });
        let moved = matches!(
            request,
            Request::Input {
                input: Input::Move { .. },
                ..
            }
        );
        if capture {
            self.armed = None;
        }
        let result = serve(backend, request, state);
        let now = Instant::now();
        let pending = pending(backend);
        self.reported_pending = pending;
        let reply = match result {
            Ok((mut response, image)) => {
                response.pending = pending;
                if configured && backend.has_video() {
                    response.retuned = Some(true);
                }
                if capture && response.submitted == Some(false) {
                    self.armed = Some(now);
                }
                if moved && state.shapes {
                    // As thread C looks after the move it applied: the
                    // pointer's shape over what it moved onto.
                    self.looks = [Some(now + SHAPE_LOOK), Some(now + SHAPE_LATE)];
                }
                // The host takes the pointer this answer carries, as it
                // takes a push: the next reading that differs is pushed.
                if observes_cursor {
                    self.pointer = Some((response.cursor, response.shape_serial));
                } else if input
                    && response.cursor.is_some()
                    && let Some((cursor, _)) = &mut self.pointer
                {
                    *cursor = response.cursor;
                }
                Outgoing::Reply {
                    response: Box::new(response),
                    image,
                    presented: capture.then(|| backend.submitted_picture()).flatten(),
                }
            }
            Err(failure) => {
                backend.release();
                Outgoing::Reply {
                    response: Box::new(Response::failed(&failure, pending)),
                    image: vec![],
                    presented: None,
                }
            }
        };
        out.blocking_send(reply).is_ok() && !close
    }

    /// The host started or stopped streaming at `now`: fine timers follow
    /// it, the pointer goes out at once when it starts, and nothing is
    /// pushed for an armed tick or a look once it stops.
    fn streaming(&mut self, on: bool, now: Instant) {
        self.timer = on.then(TimerResolution::hold);
        self.pointer_due = on.then_some(now);
        self.pointer = None;
        if !on {
            self.armed = None;
            self.looks = [None; 2];
        }
    }

    /// Push each unit the encoder finished for the last capture's desktop,
    /// or the failure it met: when to collect again while it still holds
    /// units, if its output does not wake the loop first. `None` once the
    /// exchange ended.
    fn units<P: Platform>(
        &mut self,
        backend: &mut PlatformBackend<P>,
        state: &ServeState,
        out: &Sender<Outgoing>,
        now: Instant,
    ) -> Option<Option<Instant>> {
        let Some((context, _)) = &state.video else {
            return Some(None);
        };
        while backend.pending_video() > 0 {
            let record = match backend.drain_video(context, Duration::ZERO) {
                Ok(Some(frame)) => {
                    let pending = pending(backend);
                    self.reported_pending = pending;
                    Outgoing::Unit {
                        generation: context.generation,
                        frame,
                        pending,
                        collected: SystemTime::now(),
                    }
                }
                Ok(None) => break,
                // The backend dropped its source, and the units with it.
                Err(failure) => {
                    let pending = pending(backend);
                    self.reported_pending = pending;
                    Outgoing::Failed { failure, pending }
                }
            };
            out.blocking_send(record).ok()?;
        }
        let left = pending(backend);
        if left < self.reported_pending {
            self.reported_pending = left;
            out.blocking_send(Outgoing::Pending { pending: left })
                .ok()?;
        }
        Some(
            (backend.pending_video() > 0).then(|| now + backend.drain_poll().unwrap_or(DRAIN_IDLE)),
        )
    }

    /// Tell the host once the source wants the tick its last capture left
    /// armed: as soon as it publishes a picture, or names a time that came
    /// (a repeat due, its encoder's input request). A source that names
    /// none and is polled is wanted again a poll interval after the arming,
    /// and at least a period of the pace, as thread C asks a still one.
    /// When to look again, if not before a wake. `None` once the exchange
    /// ended.
    fn fresh<P: Platform>(
        &mut self,
        backend: &PlatformBackend<P>,
        state: &ServeState,
        out: &Sender<Outgoing>,
        now: Instant,
    ) -> Option<Option<Instant>> {
        let Some(armed) = self.armed else {
            return Some(None);
        };
        let due = match backend.capture_idle_until(None) {
            Some(until) => until,
            None => backend
                .capture_poll()
                .map_or(now, |poll| armed + poll.max(period(state))),
        };
        if due > now {
            return Some(Some(due));
        }
        self.armed = None;
        out.blocking_send(Outgoing::Fresh {
            presented: backend.published(),
        })
        .ok()?;
        Some(None)
    }

    /// Read the pointer on the desktop the worker last reported when a
    /// sample or a look is due, and its shape where the host reads shapes,
    /// and push it when it or the shape's number changed: when to read
    /// next. `None` once the exchange ended.
    fn pointer<P: Platform>(
        &mut self,
        backend: &mut PlatformBackend<P>,
        state: &mut ServeState,
        out: &Sender<Outgoing>,
        now: Instant,
    ) -> Option<Instant> {
        let sample = self.pointer_due.is_some_and(|due| now >= due);
        let look = self.looks.iter().flatten().any(|due| now >= *due);
        if sample {
            let interval = CURSOR_INTERVAL.min(period(state)).max(CURSOR_MIN);
            self.pointer_due = Some(now + interval);
        }
        if look {
            self.looks = self.looks.map(|due| due.filter(|due| *due > now));
        }
        if (sample || look)
            && let Some(context) = state.reported.take()
        {
            let read = backend.cursor(&context);
            match read {
                Ok(cursor) => {
                    if state.shapes && cursor.is_some_and(|cursor| cursor.visible) {
                        // A lost link shows in the next answer: the host
                        // keeps the last shape meanwhile.
                        let _ = observe_shape(backend, &context, state);
                    }
                    let pushed = (cursor, state.shapes.then_some(state.shape_serial));
                    if self.pointer != Some(pushed) {
                        self.pointer = Some(pushed);
                        let (cursor, shape_serial) = pushed;
                        if out
                            .blocking_send(Outgoing::Pointer {
                                cursor,
                                shape_serial,
                            })
                            .is_err()
                        {
                            state.reported = Some(context);
                            return None;
                        }
                    }
                }
                // The desktop changed: the host's next context check finds
                // it, and the next answer reports the new one.
                Err(failure) => tracing::debug!(
                    target: "pb_mapper_desktop::session",
                    event = "desktop_worker_pointer_unread",
                    detail = %failure,
                ),
            }
            state.reported = Some(context);
        }
        let looks = self.looks.iter().flatten().min().copied();
        Some(
            self.pointer_due
                .into_iter()
                .chain(looks)
                .min()
                .unwrap_or(now + IDLE_WAIT),
        )
    }
}

/// The encoder's pending units, as an answer and a push carry them.
fn pending<P: Platform>(backend: &PlatformBackend<P>) -> u32 {
    u32::try_from(backend.pending_video()).unwrap_or(u32::MAX)
}

/// The period of the last capture's pace; 60 fps before any.
fn period(state: &ServeState) -> Duration {
    let fps = state.video.as_ref().map_or(60, |(_, fps)| *fps);
    Duration::from_secs(1) / fps.max(1)
}

/// Keep `context` as the desktop the answer reports, cloning it only when
/// it is another.
fn report(reported: &mut Option<DesktopContext>, context: &DesktopContext) {
    if reported.as_ref() != Some(context) {
        *reported = Some(context.clone());
    }
}

/// One request on the worker's desktop-bound thread. The host's thread C
/// paces and authorises; this checks the input sequence again, and the
/// backend the whole desktop the host expects, at the injection and
/// capture boundaries.
pub(crate) fn serve<B: Backend>(
    backend: &mut B,
    request: Request,
    state: &mut ServeState,
) -> Result<(Response, Vec<u8>)> {
    let mut response = Response::default();
    if matches!(request, Request::Release {} | Request::Close {}) {
        backend.release();
        return Ok((response, vec![]));
    }
    // Whatever desktop is bound now: what this worker changed goes back.
    if let Request::RestoreDisplayMode {} = request {
        response.restored = Some(backend.restore_display_mode()?);
        return Ok((response, vec![]));
    }
    if let Request::Stream { on } = request {
        if on != state.streaming {
            state.streaming = on;
            backend.video_paused(!on);
        }
        return Ok((response, vec![]));
    }
    if let Request::Input {
        expected,
        sequence: next,
        input,
    } = request
    {
        if next <= state.sequence {
            backend.release();
            return Err(error("repeated helper input"));
        }
        state.sequence = next;
        // Binds this thread to the input desktop, and fails unless it is
        // still `expected`.
        backend.input_in_context(&expected, &input)?;
        state.last_input = Instant::now();
        // The pointer as the input left it, so that the host's thread C knows
        // its own move when it comes back. The input stands without it.
        response.cursor = backend.cursor(&expected).ok().flatten();
        // The host keeps the desktop each answer reports: this is the one the
        // input reached.
        report(&mut state.reported, &expected);
        response.context = Some(expected);
        return Ok((response, vec![]));
    }
    if let Request::Video {
        expected,
        codec,
        keyframe,
        rate,
    } = request
    {
        if !(crate::qos::ENCODER_BITRATE_MIN..=crate::qos::ENCODER_BITRATE_MAX)
            .contains(&rate.bitrate)
            || !(1..=crate::video::MAX_FPS).contains(&rate.pace_fps)
        {
            return Err(error("invalid video request"));
        }
        // The backend checks that the bound desktop is `expected` before
        // and after it captures; the answer reports it, as an input's does.
        let frame = backend.capture_video(&expected, codec, keyframe, rate)?;
        // Said only when it found nothing new, or an encoder that had not
        // asked for input: the host's tick stays armed then, and the worker
        // tells it when its source wants it.
        response.submitted = (!backend.submitted_video()).then_some(false);
        match &mut state.video {
            Some((context, fps)) if *context == expected => *fps = rate.pace_fps,
            video => *video = Some((expected.clone(), rate.pace_fps)),
        }
        report(&mut state.reported, &expected);
        response.context = Some(expected);
        return Ok(match frame {
            Some(frame) => {
                response.video = Some(frame.format);
                (response, frame.bytes)
            }
            None => (response, vec![]),
        });
    }
    let context = backend.context()?;
    // Context checks observe the pointer too, so it follows while capture is
    // idle. The backend checks this desktop identity; observing it never
    // renews last_input or acknowledges video.
    let observe_cursor = request.observes_cursor();
    let answer_shape = matches!(request, Request::CursorShape {});
    let mut image = match request {
        Request::Start { .. } => {
            // This worker runs in the interactive session: its settings are
            // the user's, which the service in session 0 cannot read. What
            // it can do is said at once, never H.265, which costs a probe.
            response.double_click = backend.double_click_guard();
            response.caps = Some((&backend.describe(false)?.caps).into());
            vec![]
        }
        Request::Context {} => vec![],
        // Answered above, before the context read.
        Request::Release {}
        | Request::Close {}
        | Request::Input { .. }
        | Request::Video { .. }
        | Request::Stream { .. }
        | Request::RestoreDisplayMode {} => vec![],
        Request::DisplayModes { expected } => {
            if expected != context {
                return Err(error("stale display mode request"));
            }
            response.modes = backend.display_modes(&expected)?;
            vec![]
        }
        Request::SetDisplayMode { expected, mode } => {
            // The backend checks the bound desktop is still `expected`.
            backend.set_display_mode(&expected, mode)?;
            vec![]
        }
        Request::Attach { .. } => return Err(error("worker is already attached")),
        Request::Adapter {} => {
            response.adapter = (state.adapter)(&context.display).map(|named| named.key);
            vec![]
        }
        Request::H265 {} => {
            // The GPU is named only when the answer is its probe's, which
            // the host may keep for later workers. A display the backend
            // could not describe, no GPU found for it, or a GPU that changed
            // under the probe answers for this session alone.
            let (h265, adapter) = match backend.describe(true) {
                Ok(described) => {
                    let h265 = described.caps.h265;
                    let probed = (state.adapter)(&described.display)
                        .filter(|named| named.h265 == Some(h265))
                        .map(|named| named.key);
                    (h265, probed)
                }
                Err(_) => (false, None),
            };
            response.h265 = Some(h265);
            response.adapter = adapter;
            vec![]
        }
        Request::ConfigureVideo {
            limits,
            nominal_fps,
        } => {
            if !(1..=crate::video::MAX_FPS).contains(&nominal_fps) {
                return Err(error("invalid video configuration"));
            }
            backend.configure_video(limits, nominal_fps)?;
            vec![]
        }
        Request::Capture {} => backend.capture_consistent(&context)?,
        Request::Presented { generation } => {
            if generation != context.generation {
                return Err(error("stale helper presentation"));
            }
            state.last_input = Instant::now();
            vec![]
        }
        // Answered below, once the pointer is observed.
        Request::CursorShape {} => {
            state.shapes = true;
            vec![]
        }
    };
    if observe_cursor {
        response.cursor = backend.cursor(&context)?;
        if state.shapes {
            // As thread C asks: only of a pointer showing on the display
            // shared.
            if response.cursor.is_some_and(|cursor| cursor.visible) {
                observe_shape(backend, &context, state)?;
            }
            response.shape_serial = Some(state.shape_serial);
        }
    }
    if answer_shape && let Some(shape) = &state.shape {
        response.shape = Some(ShapeHeader::of(shape));
        image = shape.rgba().to_vec();
    }
    report(&mut state.reported, &context);
    response.context = Some(context);
    Ok((response, image))
}

/// Read the pointer's shape into `state`, numbering a new one. A shape the
/// backend could not read keeps the last; only a lost link fails.
fn observe_shape<B: Backend>(
    backend: &mut B,
    context: &DesktopContext,
    state: &mut ServeState,
) -> Result<()> {
    match backend.cursor_shape(context) {
        Ok(Some(shape)) if state.shape.as_ref() != Some(&shape) => {
            state.shape = Some(shape);
            state.shape_serial += 1;
        }
        Ok(_) => {}
        Err(failure) if failure.kind() == std::io::ErrorKind::NotConnected => {
            return Err(failure);
        }
        Err(failure) => tracing::debug!(
            target: "pb_mapper_desktop::session",
            event = "desktop_cursor_shape_failed",
            detail = %failure,
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Input;

    #[test]
    fn input_reaches_only_the_desktop_the_host_expected() {
        /// The bound desktop, which the test changes under the host.
        struct Bound {
            context: DesktopContext,
            injected: u32,
        }
        impl Backend for Bound {
            fn display(&mut self) -> Result<crate::Display> {
                Ok(self.context.display.clone())
            }
            fn context(&mut self) -> Result<DesktopContext> {
                Ok(self.context.clone())
            }
            fn capture(&mut self) -> Result<Vec<u8>> {
                Ok(vec![])
            }
            fn input(&mut self, _: &Input) -> Result<()> {
                self.injected += 1;
                Ok(())
            }
            fn release(&mut self) {}
        }
        let expected = DesktopContext {
            display: crate::Display {
                id: 1,
                name: "bound".into(),
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
            generation: 1,
            desktop: crate::DesktopKind::Ordinary,
        };
        let mut backend = Bound {
            context: expected.clone(),
            injected: 0,
        };
        let mut state = ServeState::new(|_| None);
        let mut send = |backend: &mut Bound, next| {
            let request = Request::Input {
                expected: expected.clone(),
                sequence: next,
                input: Input::Move { x: 1, y: 1 },
            };
            serve(backend, request, &mut state).map(|(response, _)| response)
        };
        // The host keeps the desktop an answer reports, for its pointer.
        let response = send(&mut backend, 1).unwrap();
        assert_eq!(response.context, Some(expected.clone()));
        assert!(send(&mut backend, 1).is_err(), "a repeated sequence");
        // The same generation on a resized display is another desktop.
        backend.context.display.width = 32;
        assert!(send(&mut backend, 2).is_err());
        assert_eq!(backend.injected, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_worker_waits_out_a_paused_host() {
        async fn answer(host: &mut tokio::io::DuplexStream) -> Result<Response> {
            let Record::Reply(response) = ipc::read_json(host).await? else {
                return Err(error("expected an answer"));
            };
            ipc::read_bytes(host, ipc::MAX_FRAME).await?;
            Ok(*response)
        }
        let (mut host, mut worker) = tokio::io::duplex(1 << 16);
        let (out, mut outgoing) = tokio::sync::mpsc::channel(4);
        let exchange = tokio::spawn(async move {
            exchange(
                &mut worker,
                1,
                None,
                |_| {
                    out.try_send(Outgoing::Reply {
                        response: Box::default(),
                        image: vec![],
                        presented: None,
                    })
                    .map_err(error)
                },
                &mut outgoing,
            )
            .await
        });
        answer(&mut host).await.unwrap();
        // The viewer stays paused for a minute: its host sends nothing.
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        ipc::write_json(&mut host, &Request::Context {})
            .await
            .unwrap();
        answer(&mut host).await.unwrap();
        ipc::write_json(&mut host, &Request::Close {})
            .await
            .unwrap();
        answer(&mut host).await.unwrap();
        exchange.await.unwrap().unwrap();
    }

    /// A worker that is not answered within its bound ends the exchange,
    /// as a call that outlives it ends the session.
    #[tokio::test(start_paused = true)]
    async fn an_answer_that_never_comes_ends_the_exchange() {
        let (_host, mut worker) = tokio::io::duplex(1 << 16);
        let (_out, mut outgoing) = tokio::sync::mpsc::channel(4);
        let ended = exchange(&mut worker, 1, None, |_| Ok(()), &mut outgoing).await;
        assert!(ended.is_err());
    }

    /// A worker builds its backend apart from any session, and no thread C
    /// binds it: its video must still encode.
    #[test]
    fn a_worker_backend_encodes_without_a_session() {
        use crate::{
            VideoCodec, VideoLimits, VideoRate,
            native::PlatformBackend,
            platform::{
                Platform,
                synthetic::{Probe, Synthetic},
            },
        };
        let probe = Probe::default();
        let parts = Synthetic::<false>::open(&probe, &Probe::display()).unwrap();
        let display = probe.clone();
        let mut backend = PlatformBackend::from_parts(
            std::sync::Arc::new(probe),
            parts,
            crate::modes::Journal::default(),
        );
        let mut state = ServeState::new(|_| None);
        let mut serve = |request| serve(&mut backend, request, &mut state).unwrap();
        let (started, _) = serve(Request::Start {
            display_id: 1,
            display_device: None,
        });
        serve(Request::ConfigureVideo {
            limits: VideoLimits::for_viewer(60.0),
            nominal_fps: 60,
        });
        let video = |keyframe| Request::Video {
            expected: started.context.clone().unwrap(),
            codec: VideoCodec::H264,
            keyframe,
            rate: VideoRate {
                bitrate: crate::qos::ENCODER_BITRATE_MIN,
                pace_fps: 60,
            },
        };
        // The first submit fills the encoder; the next drains its unit.
        let (first, _) = serve(video(true));
        assert!(first.error.is_none() && first.video.is_none());
        let (second, bytes) = serve(video(false));
        assert_eq!(second.video.unwrap().codec, VideoCodec::H264);
        assert!(!bytes.is_empty());
        // Both took the tick, which goes unsaid; a capture of a still display
        // says it found nothing new.
        assert_eq!((first.submitted, second.submitted), (None, None));
        display.still(true);
        let (still, _) = serve(video(false));
        assert!(still.error.is_none());
        assert_eq!(still.submitted, Some(false));
    }
}
