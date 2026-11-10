//! Thread C: the one desktop-bound thread of a host session. It owns the
//! backend (capture, encoder, input injection and the desktop binding), the
//! capture pacer and the cursor sampler. Every iteration drains the input lane
//! ahead of and after its single backend capture call, so input never waits
//! behind capture or encode, and nothing queued before a stop is executed.
//!
//! Input is authorised only by `AuthLedger`, which only a validated
//! presentation advances. Capture, receipts and cursor sampling renew nothing.

mod capture;
mod control;
mod cursor;
mod hold;
mod modes;
#[cfg(test)]
mod tests;
mod window;

use self::{capture::unavailable_reason, cursor::CursorEcho, hold::DragHold, window::Window};
use crate::{
    Backend, ControlAdmission, ControlLease, Description, DesktopContext, Result, VideoCodec,
    VideoLimits, Waker,
    lane::LaneCommand,
    pacer::Pacer,
    protocol::{NoticeCode, SessionState, StateReason},
    scheduler::Waiter,
    shared::{EngineState, SessionShared},
    timer::TimerResolution,
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

/// Thread C's time. Sessions run on `Monotonic`; tests step their own.
pub(crate) trait Clock: Send + 'static {
    fn now(&self) -> Instant;
}
/// The monotonic clock.
pub(crate) struct Monotonic;
impl Clock for Monotonic {
    #[inline]
    fn now(&self) -> Instant {
        Instant::now()
    }
}

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
const CONTEXT_CHECK_INTERVAL: Duration = Duration::from_millis(100);
/// How long a lock notice stays in the published state.
const NOTICE_TTL: Duration = Duration::from_secs(5);

/// What a session's thread C is started with.
pub(crate) struct WorkerConfig {
    pub codec: VideoCodec,
    /// The viewer's bounds; `max_fps` is the encoder's nominal rate for the
    /// whole session.
    pub limits: VideoLimits,
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
/// host can ask what it captures and encodes before it answers the viewer,
/// then waits for `start`. Dropped unstarted, it drops the backend on its own
/// thread.
pub(crate) struct Prepared {
    /// The backend's description, H.265 unprobed: the display that sizes the
    /// first IDR, and the capabilities. Nothing arrives when it could not be
    /// built.
    pub description: oneshot::Receiver<Result<Description>>,
    /// Whether it encodes H.265, when asked: probed after the description,
    /// so a slow probe never costs the display.
    pub h265: Option<oneshot::Receiver<bool>>,
    pub start: oneshot::Sender<(Arc<SessionShared>, WorkerConfig)>,
    pub thread: tokio::task::JoinHandle<WorkerExit>,
}

/// Start thread C on `factory`'s backend, which describes itself at once, and
/// then, when `probe_h265`, says whether it encodes H.265.
pub(crate) fn prepare<B, F>(factory: F, probe_h265: bool) -> Prepared
where
    B: Backend,
    F: FnOnce() -> Result<B> + Send + 'static,
{
    let (described, description) = oneshot::channel();
    let (probed, h265) = oneshot::channel();
    let (start, started) = oneshot::channel::<(Arc<SessionShared>, WorkerConfig)>();
    let thread = tokio::task::spawn_blocking(move || {
        let backend = factory().map(|mut backend| {
            let _ = described.send(backend.describe(false));
            if probe_h265 {
                let _ = probed.send(backend.describe(true).is_ok_and(|probed| probed.caps.h265));
            }
            backend
        });
        let Ok((shared, config)) = started.blocking_recv() else {
            return WorkerExit::Cancelled;
        };
        run(&shared, backend, config)
    });
    Prepared {
        description,
        h265: probe_h265.then_some(h265),
        start,
        thread,
    }
}

impl Prepared {
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

fn run<B: Backend>(
    shared: &Arc<SessionShared>,
    backend: Result<B>,
    config: WorkerConfig,
) -> WorkerExit {
    let started = Waiter::new(&shared.wake)
        .and_then(|waiter| Ok((waiter, Engine::start(shared, backend?, config, Monotonic)?)));
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

/// A sticky backend link failure (`Backend`'s `NotConnected`): the session
/// ends instead of retrying.
fn link_lost(failure: &io::Error) -> bool {
    failure.kind() == io::ErrorKind::NotConnected
}

/// The engine of thread C: its backend, the desktop generation and the
/// control grant, the pacer and the published state.
struct Engine<B: Backend, K: Clock = Monotonic> {
    shared: Arc<SessionShared>,
    backend: B,
    clock: K,
    codec: VideoCodec,
    limits: VideoLimits,
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
    /// Until when the display is still changing after a mode change: a
    /// capture failure is `DisplayChanging` then, and no other change is
    /// taken.
    settle_until: Option<Instant>,
}

impl<B: Backend, K: Clock> Engine<B, K> {
    fn start(
        shared: &Arc<SessionShared>,
        mut backend: B,
        config: WorkerConfig,
        clock: K,
    ) -> Result<Self> {
        config.limits.validate()?;
        backend.bind(Waker(shared.wake.clone()));
        backend.configure_video(config.limits, config.limits.max_fps)?;
        let now = clock.now();
        let pace_fps = shared.targets.pace_fps.load(Ordering::Acquire).max(1);
        Ok(Self {
            shared: shared.clone(),
            backend,
            clock,
            codec: config.codec,
            limits: config.limits,
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
            settle_until: None,
        })
    }

    fn iterate(&mut self) -> Step {
        let shared = self.shared.clone();
        let now = self.clock.now();
        if shared.cancel.is_cancelled() {
            return Step::Exit(WorkerExit::Cancelled);
        }
        let policy = shared.policy.get();
        if policy != self.policy {
            self.policy = policy;
            self.release_control();
            // Revoking control gives the owner their display back; a grant,
            // which moves the epoch too, does not.
            if !shared.policy.allowed() {
                self.restore_mode(now);
            }
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
        let now = self.clock.now();
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

    fn notice(&mut self, code: NoticeCode, message: String, now: Instant) {
        self.notice_serial += 1;
        self.state.notice = Some((code, message, self.notice_serial));
        self.notice_until = Some(now + NOTICE_TTL);
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
impl<B: Backend, K: Clock> Drop for Engine<B, K> {
    /// The session ends, however it ends (a panic on thread C unwinds here
    /// too): held input is released, and the owner's display gets its own
    /// mode back. A failed restore stays journalled for the host to retry.
    fn drop(&mut self) {
        self.backend.release();
        if let Err(failure) = self.backend.restore_display_mode() {
            tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "display_mode_not_restored",
                detail = %failure,
            );
        }
    }
}
