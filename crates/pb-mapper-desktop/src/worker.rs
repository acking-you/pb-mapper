//! Thread C: the one desktop-bound thread of a host session. It owns the
//! backend (capture, encoder, input injection and the desktop binding), the
//! capture pacer and the cursor sampler. Every iteration drains the input lane
//! ahead of and after its single backend capture call, so input never waits
//! behind capture or encode, and nothing queued before a stop is executed.
//!
//! Input is authorised only by `AuthLedger`, which only a validated
//! presentation advances. Capture, receipts and cursor sampling renew nothing.

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;

use crate::{
    Backend, Button, ControlAdmission, ControlLease, DesktopContext, EncodedFrame, FailureKind,
    Input, InputEvent, Result, VideoCodec, VideoLimits, VideoRate, error, failure_kind,
    idr::IdrReason,
    lane::{LANE_CAPACITY, LaneCommand},
    outgoing::{HostUnit, Push},
    pacer::Pacer,
    protocol::{NoticeCode, SessionState, StateReason, VideoSize},
    scheduler::Waiter,
    shared::{CursorReport, CursorUpdate, EngineState, InputWaitStats, SessionShared},
    timer::TimerResolution,
};
use std::{
    collections::VecDeque,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

pub(crate) type Factory = Arc<dyn Fn() -> Result<Box<dyn Backend>> + Send + Sync>;
type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// An H.265 encoder failed in this process.
static H265_FAILED: AtomicBool = AtomicBool::new(false);

/// Whether an H.265 encoder failed in this process: the host offers H.265 no
/// more, and its viewers reconnect to H.264.
pub(crate) fn h265_failed() -> bool {
    H265_FAILED.load(Ordering::Acquire)
}

/// Units waiting in the hand-off for send credit at which a tick is skipped:
/// transport backpressure.
pub(crate) const TRANSPORT_GATE: usize = 2;
/// Units in the encoder, or in the helper, at which a tick is skipped. A unit
/// stays there for the encoder's latency, so this many cover a latency of
/// nearly four periods without lowering the rate; a fast encoder never holds
/// more than one, so it adds no latency. Fixed rather than derived from the
/// measured dwell: a queueing encoder's dwell grows with its depth, and a
/// bound that followed it would deepen its own queue.
pub(crate) const ENCODER_DEPTH: usize = 4;
/// The least time between two helper lease renewals.
const LEASE_RENEW_MIN: Duration = Duration::from_millis(250);
const CONTEXT_CHECK_INTERVAL: Duration = Duration::from_millis(100);
const UNAVAILABLE_RETRY: Duration = Duration::from_millis(200);
/// How often the ordinary backend's encoder is polled while it holds units.
const DRAIN_POLL: Duration = Duration::from_millis(2);
/// The longest the helper may block one drain call for encoder output.
const HELPER_DRAIN_WAIT_MAX: Duration = Duration::from_millis(4);
const CURSOR_INTERVAL: Duration = Duration::from_millis(16);
/// The window of the published capture rate and input wait.
const STATS_WINDOW: Duration = Duration::from_secs(1);
/// Input wait samples kept per window; the maximum is exact beyond it.
const WAIT_SAMPLES: usize = 4096;
/// How long a lock notice stays in the published state.
const NOTICE_TTL: Duration = Duration::from_secs(5);
/// A held drag-start press waits this much past the double-click time...
const DRAG_GUARD_MARGIN: Duration = Duration::from_millis(20);
/// ...but never longer than this.
const DRAG_GUARD_MAX: Duration = Duration::from_millis(600);

/// What a session's thread C is started with.
pub(crate) struct WorkerConfig {
    pub codec: VideoCodec,
    pub limits: VideoLimits,
    /// `limits.max_fps`: the encoder's nominal rate for the whole session.
    pub nominal_fps: u32,
    pub admission: Option<Arc<dyn ControlAdmission>>,
}

/// How thread C ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerExit {
    /// `shared.cancel` ended it.
    Cancelled,
    /// The backend failed for good (no backend, a lost helper or an
    /// unrecoverable encoder); the session ends with `Stopped{capture_failed}`.
    CaptureFailed(String),
}

/// Thread C of one session. It exits on `shared.cancel` or fatally; `exited`
/// reports which, once it has also dropped its backend. Dropping the handle
/// ends the session, so thread C never outlives it.
pub(crate) struct WorkerHandle {
    join: Option<tokio::task::JoinHandle<WorkerExit>>,
    exit: Option<WorkerExit>,
    cancel: tokio_util::sync::CancellationToken,
    wake: crate::scheduler::Wake,
}
impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.wake.notify();
    }
}
impl WorkerHandle {
    /// Cancel-safe, and repeatable after the worker has exited.
    pub async fn exited(&mut self) -> WorkerExit {
        if let Some(join) = &mut self.join {
            let exit = join
                .await
                .unwrap_or_else(|failure| WorkerExit::CaptureFailed(failure.to_string()));
            self.join = None;
            self.exit = Some(exit);
        }
        self.exit.clone().unwrap_or(WorkerExit::Cancelled)
    }
}

/// Thread C before its session: it builds the backend at once, so that the
/// host can ask what it encodes before it answers the viewer, then waits for
/// `start`. Dropped unstarted, it drops the backend on its own thread.
pub(crate) struct Prepared {
    /// The backend's answer to `encodes_h265`, when it was asked.
    h265: Option<oneshot::Receiver<bool>>,
    start: oneshot::Sender<(Arc<SessionShared>, WorkerConfig)>,
    thread: tokio::task::JoinHandle<WorkerExit>,
}

/// Start thread C on `factory`'s backend, asking it whether it encodes H.265
/// when `probe_h265`.
pub(crate) fn prepare(factory: Factory, probe_h265: bool) -> Prepared {
    let (answer, h265) = oneshot::channel();
    let (start, started) = oneshot::channel::<(Arc<SessionShared>, WorkerConfig)>();
    let thread = tokio::task::spawn_blocking(move || {
        let backend = factory().map(|mut backend| {
            if probe_h265 {
                let _ = answer.send(backend.encodes_h265());
            }
            backend
        });
        let Ok((shared, config)) = started.blocking_recv() else {
            return WorkerExit::Cancelled;
        };
        run(&shared, backend, config)
    });
    Prepared {
        h265: probe_h265.then_some(h265),
        start,
        thread,
    }
}

impl Prepared {
    /// Whether the backend encodes H.265: false when it was not asked, could
    /// not be built, or has not answered within `wait`.
    pub async fn h265(&mut self, wait: Duration) -> bool {
        let Some(answer) = self.h265.take() else {
            return false;
        };
        matches!(tokio::time::timeout(wait, answer).await, Ok(Ok(true)))
    }

    /// Run the session. Whoever cancels `shared.cancel`, C is woken at once.
    pub fn start(self, shared: Arc<SessionShared>, config: WorkerConfig) -> WorkerHandle {
        let (cancel, wake) = (shared.cancel.clone(), shared.wake.clone());
        let (cancelled, woken) = (cancel.clone(), wake.clone());
        // Only a thread that already ended misses this, and then it reports
        // how it ended.
        let _ = self.start.send((shared, config));
        let mut thread = self.thread;
        let join = tokio::spawn(async move {
            let joined = tokio::select! {
                joined = &mut thread => joined,
                () = cancelled.cancelled() => {
                    woken.notify();
                    thread.await
                }
            };
            joined.unwrap_or_else(|failure| WorkerExit::CaptureFailed(failure.to_string()))
        });
        WorkerHandle {
            join: Some(join),
            exit: None,
            cancel,
            wake,
        }
    }
}

fn run(
    shared: &Arc<SessionShared>,
    backend: Result<Box<dyn Backend>>,
    config: WorkerConfig,
) -> WorkerExit {
    let clock: Clock = Arc::new(Instant::now);
    let started = Waiter::new(&shared.wake)
        .and_then(|waiter| Ok((waiter, Engine::start(shared, backend?, config, clock)?)));
    let (waiter, mut engine) = match started {
        Ok(started) => started,
        Err(failure) => {
            // Without a backend this session can never capture. Report why;
            // the viewer reconnects with backoff, which retries the backend.
            shared.state.send_modify(|state| {
                state.record.state = SessionState::CaptureUnavailable;
                state.record.reason = Some(unavailable_reason(&failure));
                state.record.message = failure.to_string();
            });
            return WorkerExit::CaptureFailed(failure.to_string());
        }
    };
    loop {
        match engine.iterate() {
            Step::Wait(deadline) => {
                if let Err(failure) = waiter.wait(deadline) {
                    return WorkerExit::CaptureFailed(failure.to_string());
                }
            }
            Step::Exit(exit) => return exit,
        }
    }
}

enum Step {
    Wait(Instant),
    Exit(WorkerExit),
}

/// The engine of thread C: its backend, the desktop generation and the
/// control grant, the pacer and the published state.
struct Engine {
    shared: Arc<SessionShared>,
    backend: Box<dyn Backend>,
    clock: Clock,
    codec: VideoCodec,
    limits: VideoLimits,
    nominal_fps: u32,
    admission: Option<Arc<dyn ControlAdmission>>,
    lease: Option<Box<dyn ControlLease>>,
    /// The copy published to `shared.state` after every iteration.
    state: EngineState,
    /// The policy epoch control was last checked against.
    policy: u64,
    context: Option<DesktopContext>,
    /// Why capture is unavailable, while it is.
    unavailable: Option<(StateReason, String)>,
    /// Why control last ended, kept until the viewer asks for control again.
    control_note: Option<(StateReason, String)>,
    /// A frame was produced since the last desktop change.
    streaming: bool,
    paused: bool,
    /// The last input sequence taken off the lane, applied or not.
    sequence: u64,
    frame_id: u64,
    /// The next submit must be an IDR (new encoder, pause); the submit that
    /// carries it clears it.
    keyframe: bool,
    pacer: Pacer,
    pace_fps: u32,
    /// The next context check, or the next retry while unavailable.
    context_due: Instant,
    cursor_due: Instant,
    cursor: CursorEcho,
    /// When to poll the encoder again while it holds units.
    drain_due: Instant,
    /// Held while this session captures.
    timer: Option<TimerResolution>,
    last_lease: Option<Instant>,
    /// Held input was released for a presentation gone stale; nothing new is
    /// admitted until a fresh one arrives.
    stale_released: bool,
    /// A sticky backend link failure: the session ends.
    fatal: Option<String>,
    window: Window,
    hold: DragHold,
    notice_serial: u64,
    notice_until: Option<Instant>,
}

impl Engine {
    fn start(
        shared: &Arc<SessionShared>,
        mut backend: Box<dyn Backend>,
        config: WorkerConfig,
        clock: Clock,
    ) -> Result<Self> {
        config.limits.validate()?;
        backend.configure_video(config.limits, config.nominal_fps)?;
        let now = clock();
        let pace_fps = shared.targets.pace_fps.load(Ordering::Acquire).max(1);
        Ok(Self {
            shared: shared.clone(),
            backend,
            clock,
            codec: config.codec,
            limits: config.limits,
            nominal_fps: config.nominal_fps,
            admission: config.admission,
            lease: None,
            state: shared.state.borrow().clone(),
            policy: shared.policy.get(),
            context: None,
            unavailable: None,
            control_note: None,
            streaming: false,
            paused: false,
            sequence: 0,
            frame_id: 0,
            keyframe: true,
            pacer: Pacer::new(pace_fps, now),
            pace_fps,
            context_due: now,
            cursor_due: now,
            cursor: CursorEcho::default(),
            drain_due: now,
            timer: None,
            last_lease: None,
            stale_released: false,
            fatal: None,
            window: Window::new(now),
            hold: DragHold::default(),
            notice_serial: 0,
            notice_until: None,
        })
    }

    fn iterate(&mut self) -> Step {
        let shared = self.shared.clone();
        let now = (self.clock)();
        if shared.cancel.is_cancelled() {
            return Step::Exit(WorkerExit::Cancelled);
        }
        let policy = shared.policy.get();
        if policy != self.policy {
            self.policy = policy;
            self.release_control();
        }
        self.state.record.control_allowed = shared.policy.allowed();
        if shared.lane.take_overflow() {
            for (command, _) in shared.lane.drain() {
                if let LaneCommand::Input(event) = command {
                    self.taken(&event);
                }
            }
            self.congested();
        }
        if shared.auth.lock().take_stale() {
            self.backend.release();
            self.drop_held();
        }
        self.release_if_stale(now);
        if self.hold.until.is_some_and(|until| now >= until) && !self.release_hold() {
            return Step::Exit(WorkerExit::Cancelled);
        }
        if !self.drain_lane() {
            return Step::Exit(WorkerExit::Cancelled);
        }
        self.renew_lease(now);
        self.capture_step(now);
        // Input queued during the backend call runs right after it.
        if !self.drain_lane() {
            return Step::Exit(WorkerExit::Cancelled);
        }
        if let Some(failure) = self.fatal.take() {
            return Step::Exit(WorkerExit::CaptureFailed(failure));
        }
        let now = (self.clock)();
        self.sample_cursor(now);
        self.window.roll(now, &mut self.state);
        // Fine process timers while capturing serve the network tasks too;
        // a paused or unavailable session gives them back.
        let capturing = !self.paused && self.unavailable.is_none();
        if capturing != self.timer.is_some() {
            self.timer = capturing.then(TimerResolution::hold);
        }
        if self.notice_until.is_some_and(|until| now >= until) {
            self.notice_until = None;
            self.state.notice = None;
        }
        self.publish();
        Step::Wait(self.deadline(now))
    }

    /// A full lane means input is late beyond repair: end control rather than
    /// inject a burst the user no longer expects.
    fn congested(&mut self) {
        self.release_control();
        self.control_note = Some((
            StateReason::InputCongestion,
            "input arrived faster than the host could apply it; request control again".into(),
        ));
    }

    /// Execute every queued command in order. False when cancelled: the rest
    /// of the lane is dropped, never executed.
    fn drain_lane(&mut self) -> bool {
        for (command, received) in self.shared.lane.drain() {
            if self.shared.cancel.is_cancelled() {
                return false;
            }
            let now = (self.clock)();
            self.window.wait(now.saturating_duration_since(received));
            self.execute(command, now);
        }
        true
    }

    fn execute(&mut self, command: LaneCommand, now: Instant) {
        if self.hold.until.is_some() {
            if !matches!(
                command,
                LaneCommand::Release
                    | LaneCommand::EndControl
                    | LaneCommand::Control { enabled: false }
            ) {
                if !self.hold.push(command) {
                    self.congested();
                }
                return;
            }
            // Ending control drops the whole hold: nothing in it was injected.
            self.drop_held();
        }
        match command {
            LaneCommand::Control { enabled } => self.control(enabled),
            LaneCommand::Release => self.backend.release(),
            LaneCommand::EndControl => self.release_control(),
            LaneCommand::Input(event) => match self.drag_guard(&event, now) {
                Some(until) => {
                    self.hold.until = Some(until);
                    self.hold.push(LaneCommand::Input(event));
                }
                None => self.input(event, now),
            },
        }
    }

    /// When an admitted left press that starts a drag follows a click within
    /// the OS double-click time and box, it must wait until the returned time
    /// so the two never merge into a double click.
    fn drag_guard(&mut self, event: &InputEvent, now: Instant) -> Option<Instant> {
        let Input::Button {
            button: Button::Left,
            down: true,
            drag_start: true,
        } = event.input
        else {
            return None;
        };
        if !self.state.record.controlling
            || !self.allowed()
            || !self
                .shared
                .auth
                .lock()
                .admits(event.generation, event.frame_id, now)
        {
            return None;
        }
        let guard = self.backend.double_click_guard()?;
        self.hold.until(guard, now)
    }

    /// The hold ended: each held command runs the full input path now, as if
    /// it had just left the lane. False when cancelled.
    fn release_hold(&mut self) -> bool {
        let held = self.hold.release();
        for (index, command) in held.into_iter().enumerate() {
            if self.shared.cancel.is_cancelled() {
                return false;
            }
            let now = (self.clock)();
            match command {
                // The press that started the hold has waited it out.
                LaneCommand::Input(event) if index == 0 => self.input(event, now),
                command => self.execute(command, now),
            }
        }
        true
    }

    fn notice(&mut self, code: NoticeCode, message: String, now: Instant) {
        self.notice_serial += 1;
        self.state.notice = Some((code, message, self.notice_serial));
        self.notice_until = Some(now + NOTICE_TTL);
    }

    fn allowed(&self) -> bool {
        self.shared.policy.allowed() && self.shared.policy.get() == self.policy
    }

    fn control(&mut self, enabled: bool) {
        let allowed = self.allowed();
        // A request repeated on a delayed link must not reset the desktop
        // generation, the decoder or an active lease.
        if enabled && allowed && self.state.record.controlling {
            return;
        }
        if !enabled && !self.state.record.controlling {
            self.backend.release();
            return;
        }
        self.release_control();
        self.control_note = None;
        if !enabled {
            return;
        }
        if !allowed {
            self.control_note = Some((
                StateReason::ControlDenied,
                "control is not authorized at the host".into(),
            ));
            return;
        }
        if let Some(admission) = &self.admission {
            match admission.acquire() {
                Ok(lease) => self.lease = Some(lease),
                Err(failure) => {
                    self.control_note = Some((StateReason::ControlDenied, failure.to_string()));
                    return;
                }
            }
        }
        self.state.record.controlling = true;
    }

    /// Count `event` as taken off the lane, applied or not. A cursor report
    /// carries the last sequence taken, which tells the viewer what the report
    /// already reflects. False for a sequence already taken.
    fn taken(&mut self, event: &InputEvent) -> bool {
        if event.sequence <= self.sequence {
            return false;
        }
        self.sequence = event.sequence;
        if matches!(event.input, Input::Move { .. }) {
            self.cursor.took_move();
        }
        true
    }

    /// Drop every held command, none of which was injected; held input
    /// counts as taken.
    fn drop_held(&mut self) {
        for command in self.hold.release() {
            if let LaneCommand::Input(event) = command {
                self.taken(&event);
            }
        }
    }

    fn input(&mut self, event: InputEvent, now: Instant) {
        if !self.taken(&event) {
            return;
        }
        // The injection boundary checks the desktop for every input; this
        // checks it only when a check is due.
        if (self.check_due(now) && !self.geometry(now))
            || !self.allowed()
            || !self.state.record.controlling
        {
            return;
        }
        if !self.live()
            || !self
                .shared
                .auth
                .lock()
                .admits(event.generation, event.frame_id, now)
        {
            // The screen changed or the presented frame is stale: wait for a
            // fresh frame, and hold nothing meanwhile.
            self.backend.release();
            return;
        }
        let Some(context) = self.context.clone() else {
            return;
        };
        let lock = matches!(event.input, Input::Lock {});
        match self.backend.input_in_context(&context, &event.input) {
            Ok(()) if lock => self.notice(
                NoticeCode::Locked,
                "The remote computer is locked".into(),
                now,
            ),
            Ok(()) => {
                self.hold.injected(&event.input, now);
                if let Input::Move { x, y } = event.input {
                    self.cursor.injected(x, y);
                }
            }
            Err(failure) => {
                if failure.kind() == io::ErrorKind::NotConnected {
                    self.fatal = Some(failure.to_string());
                } else {
                    // The boundary may have seen a desktop change this engine
                    // has not checked yet: the new generation admits nothing
                    // stale, so drop the event and keep control.
                    let generation = self.state.record.generation;
                    if !self.geometry(now) || self.state.record.generation != generation {
                        return;
                    }
                }
                if lock {
                    // Nothing was held down, so control stays.
                    self.notice(NoticeCode::LockFailed, failure.to_string(), now);
                    return;
                }
                // A live picture is not proof of working control. End the
                // grant and keep the failure until the viewer asks for control
                // again; queued presses are never replayed.
                self.release_control();
                self.control_note =
                    Some((StateReason::InputFailed, format!("input_failed: {failure}")));
            }
        }
    }

    /// Release held input once when the presented frame goes stale. Nothing
    /// is admitted again until a fresh presentation, so one release suffices.
    fn release_if_stale(&mut self, now: Instant) {
        if self.state.record.controlling && !self.shared.auth.lock().fresh(now) {
            self.drop_held();
            if !self.stale_released {
                self.backend.release();
                self.stale_released = true;
            }
        } else {
            self.stale_released = false;
        }
    }

    fn renew_lease(&mut self, now: Instant) {
        if !self.state.record.controlling
            || self
                .last_lease
                .is_some_and(|at| now.saturating_duration_since(at) < LEASE_RENEW_MIN)
            || !self.shared.auth.lock().take_lease_due()
        {
            return;
        }
        self.last_lease = Some(now);
        if let Some(context) = self.context.clone()
            && let Err(failure) = self.backend.presented(&context)
            && failure.kind() == io::ErrorKind::NotConnected
        {
            self.fatal = Some(failure.to_string());
        }
    }

    fn release_control(&mut self) {
        self.invalidate();
        self.state.record.controlling = false;
        self.lease = None;
    }

    /// A new desktop generation: nothing produced or presented before it
    /// admits input, older units are not sent, and the next unit is an IDR.
    fn invalidate(&mut self) {
        self.backend.release();
        self.drop_held();
        self.cursor.invalidate();
        self.state.record.generation += 1;
        let generation = self.state.record.generation;
        self.shared.auth.lock().on_invalidate(generation);
        self.state.dropped_frames += self.shared.outgoing.retain_generation(generation) as u64;
        self.shared.idr.lock().force(IdrReason::Generation);
    }

    /// A context check is due: there is no context yet, capture is
    /// unavailable, or `context_due` has passed.
    fn check_due(&self, now: Instant) -> bool {
        self.context.is_none() || self.unavailable.is_some() || now >= self.context_due
    }

    /// Check the desktop identity. A new context or a recovery rebuilds the
    /// encoder once; a new context also starts a new generation, while the
    /// failure a recovery follows already started one if anything had been
    /// produced. False while unavailable.
    fn geometry(&mut self, now: Instant) -> bool {
        let context = match self.backend.context() {
            Ok(context) => context,
            Err(failure) => {
                self.fail(failure, now);
                return false;
            }
        };
        self.context_due = now + CONTEXT_CHECK_INTERVAL;
        let recovered = self.unavailable.is_some();
        let changed = self.context.as_ref() != Some(&context);
        if !recovered && !changed {
            return true;
        }
        if self.context.is_some() || recovered {
            if changed && self.context.is_some() {
                self.invalidate();
            }
            self.keyframe = true;
            if let Err(failure) = self.backend.configure_video(self.limits, self.nominal_fps) {
                self.fail(failure, now);
                return false;
            }
        }
        self.state.record.display = Some(context.display.clone());
        self.state.record.desktop = context.desktop;
        self.context = Some(context);
        self.unavailable = None;
        self.streaming = false;
        true
    }

    /// Capture is unavailable until a retry succeeds. A failure that ends
    /// streaming may be a desktop switch: it starts a new generation and is
    /// retried at once. Any other, such as a capture failing again right
    /// after a recovery, is retried every `UNAVAILABLE_RETRY`, never at the
    /// capture pace. A sticky link failure ends the session instead, and so
    /// does an H.265 encoder that would not build or encode: H.265 is then
    /// off for the rest of the process, and the viewer reconnects to H.264.
    fn fail(&mut self, failure: io::Error, now: Instant) {
        let message = failure.to_string();
        if failure.kind() == io::ErrorKind::NotConnected {
            self.fatal = Some(message);
        } else if self.codec == VideoCodec::H265
            && failure_kind(&failure)
                == Some(FailureKind::EncoderFailed {
                    codec: VideoCodec::H265,
                })
        {
            H265_FAILED.store(true, Ordering::Release);
            tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "desktop_h265_failed",
                detail = ?message,
            );
            self.fatal = Some(message);
        }
        self.context_due = if self.unavailable.is_none() && self.streaming {
            self.invalidate();
            now
        } else {
            now + UNAVAILABLE_RETRY
        };
        self.unavailable = Some((unavailable_reason(&failure), failure.to_string()));
    }

    /// Capturing, not paused, with a frame produced in this generation.
    fn live(&self) -> bool {
        !self.paused && self.unavailable.is_none() && self.streaming
    }

    /// At most one backend capture or drain call.
    fn capture_step(&mut self, now: Instant) {
        let shared = self.shared.clone();
        let targets = &shared.targets;
        if targets.paused.load(Ordering::Acquire) {
            // Never send while paused, and resume with an IDR.
            self.paused = true;
            if self.backend.pending_video() > 0
                && let Some(context) = &self.context
            {
                let result = self.backend.drain_video(context, Duration::ZERO);
                self.drain_due = (self.clock)() + DRAIN_POLL;
                match result {
                    Ok(Some(frame)) => {
                        self.window.dwell(frame.format.encode_us);
                        self.state.dropped_frames += 1;
                    }
                    Ok(None) => {}
                    Err(failure) => self.fail(failure, now),
                }
            }
            self.state.dropped_frames += self.shared.outgoing.clear() as u64;
            self.keyframe = true;
            return;
        }
        self.paused = false;
        // A retry checks the context and then captures at once, so a failure
        // that persists never shows as recovered in between.
        let retry = self.unavailable.is_some();
        if retry && now < self.context_due {
            return;
        }
        if self.check_due(now) && !self.geometry(now) {
            return;
        }
        let Some(context) = self.context.clone() else {
            return;
        };
        let pace_fps = targets.pace_fps.load(Ordering::Acquire).max(1);
        if pace_fps != self.pace_fps {
            self.pace_fps = pace_fps;
            self.pacer.set_pace(pace_fps, now);
        }
        if retry || self.pacer.due(now) {
            if self.shared.outgoing.len() < TRANSPORT_GATE
                && self.backend.pending_video() < ENCODER_DEPTH
            {
                let keyframe = self.keyframe || self.shared.idr.lock().take_due(now);
                let rate = VideoRate {
                    bitrate: targets.encoder_bps.load(Ordering::Acquire),
                    pace_fps,
                };
                let result = self
                    .backend
                    .capture_video(&context, self.codec, keyframe, rate);
                if keyframe {
                    // Handed over: an IDR still in the encoder at the next
                    // tick must not be asked for again.
                    self.keyframe = false;
                    self.shared.idr.lock().answered(now);
                }
                let done = (self.clock)();
                self.drain_due = done + DRAIN_POLL;
                self.produced(result, done);
            } else {
                // Transport backpressure or a full encoder, never
                // presentation: skip the tick.
                self.state.skipped_ticks += 1;
            }
            self.pacer.consume(now);
        } else if self.backend.pending_video() > 0 {
            let wait = HELPER_DRAIN_WAIT_MAX.min(self.pacer.next().saturating_duration_since(now));
            let result = self.backend.drain_video(&context, wait);
            let done = (self.clock)();
            let drained = matches!(result, Ok(Some(_)));
            // The ordinary backend returns at once: poll it again shortly. A
            // helper that waited the whole time is polled again at once, and so
            // is a backend that just returned a unit.
            self.drain_due =
                if drained || (!wait.is_zero() && done.saturating_duration_since(now) >= wait) {
                    done
                } else {
                    done + DRAIN_POLL
                };
            self.produced(result, done);
        }
    }

    fn produced(&mut self, result: Result<Option<EncodedFrame>>, at: Instant) {
        let frame = match result {
            Ok(None) => return,
            Ok(Some(frame))
                if frame.bytes.is_empty() || frame.bytes.len() > crate::wire::MAX_PAYLOAD =>
            {
                return self.fail(error("encoded frame exceeds the transport limit"), at);
            }
            Ok(Some(frame)) => frame,
            Err(failure) => return self.fail(failure, at),
        };
        let format = frame.format;
        self.window.dwell(format.encode_us);
        if format.keyframe {
            self.shared.idr.lock().answered(at);
        }
        self.frame_id += 1;
        self.shared.auth.lock().on_produced(self.frame_id, at);
        if !format.duplicate {
            self.window.unique += 1;
        }
        self.state.record.video = Some(VideoSize {
            width: format.width,
            height: format.height,
        });
        self.state.gop = format.gop;
        self.streaming = true;
        // B-send writes the `State` of a generation before its first unit:
        // publish it before the unit can be taken.
        self.publish();
        let pushed = self.shared.outgoing.push(HostUnit {
            generation: self.state.record.generation,
            frame_id: self.frame_id,
            codec: format.codec,
            width: format.width,
            height: format.height,
            keyframe: format.keyframe,
            duplicate: format.duplicate,
            pts_us: format.pts_us,
            encode_us: format.encode_us,
            ready_at: at,
            bytes: bytes::Bytes::from(frame.bytes),
        });
        if let Push::Overflow { dropped } = pushed {
            self.state.dropped_frames += dropped as u64;
            self.shared.idr.lock().request(IdrReason::HostResync);
        }
    }

    /// Every `CURSOR_INTERVAL` while capturing; never while paused or
    /// unavailable. Reported when it differs from the last report, or, equal
    /// to it, once more after C takes a move, so the report carries that
    /// move's sequence; never when it is the echo of the controller's own
    /// move. `CursorEcho` holds the rule.
    fn sample_cursor(&mut self, now: Instant) {
        if self.paused || self.unavailable.is_some() || now < self.cursor_due {
            return;
        }
        let Some(context) = self.context.clone() else {
            return;
        };
        self.cursor_due = now + CURSOR_INTERVAL;
        match self.backend.cursor(&context) {
            Ok(position) => {
                let cursor = position.map(|position| CursorUpdate {
                    generation: self.state.record.generation,
                    x: position.x,
                    y: position.y,
                    visible: position.visible,
                });
                if let Some(report) = self.cursor.sample(cursor, self.sequence) {
                    self.shared.cursor.send_replace(Some(report));
                }
            }
            Err(failure) => self.fail(failure, now),
        }
    }

    fn publish(&mut self) {
        self.state.record.state = if self.paused {
            SessionState::Paused
        } else if self.unavailable.is_some() {
            SessionState::CaptureUnavailable
        } else if self.streaming {
            SessionState::Streaming
        } else {
            SessionState::Connecting
        };
        let (reason, message) = self
            .unavailable
            .as_ref()
            .or(self.control_note.as_ref())
            .map_or((None, String::new()), |(reason, message)| {
                (Some(*reason), message.clone())
            });
        self.state.record.reason = reason;
        self.state.record.message = message;
        self.shared.state.send_if_modified(|published| {
            let changed = *published != self.state;
            if changed {
                published.clone_from(&self.state);
            }
            changed
        });
    }

    /// The earliest of the next tick, cursor sample, context check, encoder
    /// poll, hold end and notice expiry; every wait also ends on a wake.
    fn deadline(&self, now: Instant) -> Instant {
        let deadline = [self.hold.until, self.notice_until]
            .into_iter()
            .flatten()
            .fold(now + CONTEXT_CHECK_INTERVAL, Instant::min);
        if self.unavailable.is_some() && !self.paused {
            return deadline.min(self.context_due);
        }
        let deadline = if self.backend.pending_video() > 0 {
            deadline.min(self.drain_due)
        } else {
            deadline
        };
        if self.paused {
            return deadline;
        }
        deadline
            .min(self.context_due)
            .min(self.pacer.next())
            .min(self.cursor_due)
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.backend.release();
    }
}

/// A left press that starts a drag right after a click would merge with it
/// into a double click on the host. It waits out the OS double-click time
/// instead, with every later command behind it. The hold only delays: held
/// commands carry no authorisation of their own, each runs the full input path
/// when the hold ends, and anything that ends control or changes the
/// generation drops them all, since none was injected yet.
#[derive(Default)]
struct DragHold {
    /// In order, with consecutive moves coalesced to the newest.
    held: VecDeque<LaneCommand>,
    /// Held commands other than moves.
    presses: usize,
    until: Option<Instant>,
    /// The last injected pointer position, in input space.
    pointer: Option<(u32, u32)>,
    /// The last injected left press, and where the pointer was.
    left_down: Option<(Instant, (u32, u32))>,
}
impl DragHold {
    /// When a drag-start press at `now` must wait: within the double-click
    /// time and box of the last left press.
    fn until(&self, guard: crate::DoubleClickGuard, now: Instant) -> Option<Instant> {
        let (pressed, (x, y)) = self.left_down?;
        let (px, py) = self.pointer?;
        let time = Duration::from_millis(guard.time_ms.into());
        let until = (pressed + time + DRAG_GUARD_MARGIN).min(now + DRAG_GUARD_MAX);
        (now.saturating_duration_since(pressed) < time
            && px.abs_diff(x) <= guard.half_box.0
            && py.abs_diff(y) <= guard.half_box.1
            && until > now)
            .then_some(until)
    }

    /// Hold `command` behind the press. False when that would hold more than
    /// a lane of commands.
    fn push(&mut self, command: LaneCommand) -> bool {
        let is_move = |command: &LaneCommand| {
            matches!(
                command,
                LaneCommand::Input(InputEvent {
                    input: Input::Move { .. },
                    ..
                })
            )
        };
        if is_move(&command) {
            if let Some(last) = self.held.back_mut().filter(|last| is_move(last)) {
                *last = command;
                return true;
            }
        } else if self.presses == LANE_CAPACITY {
            return false;
        } else {
            self.presses += 1;
        }
        self.held.push_back(command);
        true
    }

    fn release(&mut self) -> VecDeque<LaneCommand> {
        self.until = None;
        self.presses = 0;
        std::mem::take(&mut self.held)
    }

    fn injected(&mut self, input: &Input, now: Instant) {
        match *input {
            Input::Move { x, y } => self.pointer = Some((x, y)),
            Input::Button {
                button: Button::Left,
                down: true,
                ..
            } => self.left_down = self.pointer.map(|pointer| (now, pointer)),
            _ => {}
        }
    }
}

/// What C reports of the pointer: every sample unlike the last report, at once,
/// except the controller's own move coming back. The viewer draws its own
/// pointer where the controller put it; an echo of that move, late behind the
/// video, would drag it back. A shown pointer at the position C last injected,
/// in the same display pixels, is that echo, whatever it showed before, until
/// a report finds it elsewhere: then a return there is someone else's move. A
/// hidden pointer carries no position, so it is never an echo. Comparing with
/// the last report rather than the last sample is what reports a move the OS
/// stopped short (a `ClipCursor`, an edge): the pointer stays where it was, but
/// not where the controller put it. A sample equal to the last report has
/// nothing to tell a viewer, unless C took a move since. Every report carries
/// the last input sequence C took, and the controller sets aside a report older
/// than its newest move, which crossed it on the way; so after a move, a
/// sample that is not its echo is reported again at the newer sequence, and a
/// clamp or a pointer that stays hidden is still told. A backend observes the
/// pointer after every input it applied (see `Backend::cursor`), so the
/// helper's, observed during a capture call, is never older than the last
/// injection.
#[derive(Default)]
struct CursorEcho {
    /// The last report.
    reported: Option<CursorUpdate>,
    /// Where the last move in this generation put the pointer, until a report
    /// finds it elsewhere.
    injected: Option<(u32, u32)>,
    /// C took a move off the lane, applied or not, since the last report.
    moved: bool,
    serial: u64,
}
impl CursorEcho {
    fn injected(&mut self, x: u32, y: u32) {
        self.injected = Some((x, y));
    }

    fn took_move(&mut self) {
        self.moved = true;
    }

    /// A new generation: nothing in it was injected yet.
    fn invalidate(&mut self) {
        self.injected = None;
    }

    /// The report to send for `cursor`, sampled after the input of
    /// `input_sequence`, if any.
    fn sample(
        &mut self,
        cursor: Option<CursorUpdate>,
        input_sequence: u64,
    ) -> Option<CursorReport> {
        let cursor = cursor.filter(|cursor| self.moved || self.reported != Some(*cursor))?;
        let at = (u32::try_from(cursor.x).ok(), u32::try_from(cursor.y).ok());
        if cursor.visible && self.injected.is_some_and(|(x, y)| at == (Some(x), Some(y))) {
            return None;
        }
        self.reported = Some(cursor);
        self.injected = None;
        self.moved = false;
        self.serial += 1;
        Some(CursorReport {
            serial: self.serial,
            cursor,
            input_sequence,
        })
    }
}

/// A capture failure's reason for the viewer.
fn unavailable_reason(failure: &io::Error) -> StateReason {
    match failure_kind(failure) {
        Some(FailureKind::SecureDesktop) => StateReason::SecureDesktop,
        Some(FailureKind::DisplayLost) => StateReason::DisplayLost,
        _ => StateReason::CaptureFailed,
    }
}

/// The published one-second rates: unique captures, input wait and the
/// encoder's dwell.
struct Window {
    started: Instant,
    unique: u64,
    waits: Vec<u32>,
    max_wait: u32,
    /// The encoder's own submit to output time of each unit it returned.
    dwells: Vec<Duration>,
}
impl Window {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            unique: 0,
            waits: Vec::new(),
            max_wait: 0,
            dwells: Vec::new(),
        }
    }

    fn wait(&mut self, waited: Duration) {
        let waited = u32::try_from(waited.as_micros()).unwrap_or(u32::MAX);
        self.max_wait = self.max_wait.max(waited);
        if self.waits.len() < WAIT_SAMPLES {
            self.waits.push(waited);
        }
    }

    /// A unit's `VideoFormat::encode_us`, which the encoder measures itself:
    /// thread C cannot tell which unit a call returned, nor see one the
    /// encoder discards in the call that submits the next. Zero is no sample:
    /// JPEG, or a backend that measures none.
    fn dwell(&mut self, encode_us: u32) {
        if encode_us > 0 && self.dwells.len() < WAIT_SAMPLES {
            self.dwells.push(Duration::from_micros(encode_us.into()));
        }
    }

    fn roll(&mut self, now: Instant, state: &mut EngineState) {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed < STATS_WINDOW {
            return;
        }
        state.capture_fps = self.unique as f32 / elapsed.as_secs_f32();
        let middle = self.dwells.len() / 2;
        state.encoder_dwell =
            (!self.dwells.is_empty()).then(|| *self.dwells.select_nth_unstable(middle).1);
        self.waits.sort_unstable();
        let at = |quantile: usize| {
            self.waits
                .get((self.waits.len() * quantile / 100).min(self.waits.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        state.input_wait = InputWaitStats {
            p50_us: at(50),
            p99_us: at(99),
            max_us: self.max_wait,
        };
        *self = Self::new(now);
    }
}
