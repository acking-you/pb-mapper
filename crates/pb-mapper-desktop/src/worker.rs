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

use self::{
    capture::unavailable_reason,
    cursor::CursorEcho,
    hold::DragHold,
    window::{Window, WindowStats},
};
use crate::{
    Backend, ControlAdmission, ControlLease, CursorShape, Description, DesktopContext, ModeList,
    Result, VideoCodec, VideoLimits, VideoSize, Waker,
    input::held::Held,
    lane::LaneCommand,
    modes::ModeCommand,
    pacer::Pacer,
    protocol::{MAX_VIEWERS, NoticeCode, PeerColor, PeerSlot, SessionState, StateReason},
    scheduler::{Waiter, Wake},
    shared::{EngineState, PeerCursorSnapshot, Policy, SessionShared},
    timer::TimerResolution,
};
use std::{
    collections::VecDeque,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

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

/// One viewer slot's configuration inside a shared room.
#[derive(Clone)]
pub(crate) struct RoomSlotConfig {
    pub shared: Arc<SessionShared>,
    pub color: PeerColor,
    pub limits: VideoLimits,
    pub cursor_shapes: bool,
    pub peer_cursors: bool,
}

/// Mutable slot membership and negotiated codec published from the acceptor to thread C.
pub(crate) struct RoomWorkerState {
    pub codec: VideoCodec,
    /// `(conn_id, config)` per slot; monotonic `conn_id` ensures thread C
    /// notices a slot replacement even between two iterations.
    pub slots: [Option<(u64, RoomSlotConfig)>; MAX_VIEWERS],
}

/// State shared between the host's room coordinator and thread C.
pub(crate) struct RoomShared {
    pub cancel: CancellationToken,
    pub wake: Wake,
    pub policy: Arc<Policy>,
    pub peer_cursors: Arc<tokio::sync::watch::Sender<PeerCursorSnapshot>>,
    pub owner: parking_lot::Mutex<VecDeque<ModeCommand>>,
    pub epoch: AtomicU64,
    pub state: parking_lot::Mutex<RoomWorkerState>,
}
impl RoomShared {
    pub fn new(policy: Arc<Policy>, codec: VideoCodec) -> Arc<Self> {
        Arc::new(Self {
            cancel: CancellationToken::new(),
            wake: Wake::new(),
            policy,
            peer_cursors: Arc::new(tokio::sync::watch::Sender::new(
                PeerCursorSnapshot::default(),
            )),
            owner: parking_lot::Mutex::new(VecDeque::new()),
            epoch: AtomicU64::new(1),
            state: parking_lot::Mutex::new(RoomWorkerState {
                codec,
                slots: std::array::from_fn(|_| None),
            }),
        })
    }

    pub fn set_codec(&self, codec: VideoCodec) {
        let mut state = self.state.lock();
        state.codec = codec;
        self.epoch.fetch_add(1, Ordering::Release);
    }

    pub fn set_slot(&self, slot: PeerSlot, conn_id: u64, config: RoomSlotConfig) {
        let mut state = self.state.lock();
        state.slots[slot.index()] = Some((conn_id, config));
        self.epoch.fetch_add(1, Ordering::Release);
    }

    pub fn clear_slot(&self, slot: PeerSlot, conn_id: u64) {
        let mut state = self.state.lock();
        if state.slots[slot.index()]
            .as_ref()
            .is_some_and(|(id, _)| *id == conn_id)
        {
            state.slots[slot.index()] = None;
            self.epoch.fetch_add(1, Ordering::Release);
        }
    }

    #[cfg(test)]
    pub fn single(shared: &Arc<SessionShared>, config: &WorkerConfig) -> Arc<Self> {
        let mut slots: [Option<(u64, RoomSlotConfig)>; MAX_VIEWERS] = std::array::from_fn(|_| None);
        slots[shared.slot.index()] = Some((
            1,
            RoomSlotConfig {
                shared: shared.clone(),
                color: PeerColor::default(),
                limits: config.limits,
                cursor_shapes: config.cursor_shapes,
                peer_cursors: false,
            },
        ));
        Arc::new(Self {
            cancel: shared.cancel.clone(),
            wake: shared.wake.clone(),
            policy: shared.policy.clone(),
            peer_cursors: shared.peer_cursors.clone(),
            owner: parking_lot::Mutex::new(VecDeque::new()),
            epoch: AtomicU64::new(1),
            state: parking_lot::Mutex::new(RoomWorkerState {
                codec: config.codec,
                slots,
            }),
        })
    }
}

/// What a session's thread C is started with.
#[cfg(test)]
pub(crate) struct WorkerConfig {
    pub codec: VideoCodec,
    /// The viewer's bounds; `max_fps` is the encoder's nominal rate for the
    /// whole session.
    pub limits: VideoLimits,
    pub admission: Option<Arc<dyn ControlAdmission>>,
    /// Both ends listed `cursor_shape`: C asks the backend for the shape.
    pub cursor_shapes: bool,
}

pub(crate) struct RoomStart {
    pub room: Arc<RoomShared>,
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
    /// The operating system withdrew capture consent. Ends this session without reconnecting.
    CaptureEnded(String),
}

impl WorkerExit {
    fn capture_error(failure: io::Error) -> Self {
        if capture_consent_ended(&failure) {
            Self::CaptureEnded(failure.to_string())
        } else {
            Self::CaptureFailed(failure.to_string())
        }
    }
}

fn capture_consent_ended(failure: &io::Error) -> bool {
    crate::failure_kind(failure) == Some(crate::FailureKind::ConsentRevoked)
        || crate::missing_permission(failure) == Some(crate::Permission::ScreenCapture)
}

/// Thread C of one session. It exits on `shared.cancel` or fatally; `exited`
/// reports which, once it has also dropped its backend. Dropping the handle
/// ends the session, so thread C never outlives it.
pub(crate) struct WorkerHandle {
    join: Option<tokio::task::JoinHandle<WorkerExit>>,
    exit: Option<WorkerExit>,
    cancel: CancellationToken,
    wake: Wake,
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
    pub start: oneshot::Sender<RoomStart>,
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
    let (start, started) = oneshot::channel::<RoomStart>();
    let thread = tokio::task::spawn_blocking(move || {
        let backend = factory().map(|mut backend| {
            let _ = described.send(backend.describe(false));
            if probe_h265 {
                let _ = probed.send(backend.describe(true).is_ok_and(|probed| probed.caps.h265));
            }
            backend
        });
        let Ok(RoomStart { room, admission }) = started.blocking_recv() else {
            return WorkerExit::Cancelled;
        };
        run(&room, backend, admission)
    });
    Prepared {
        description,
        h265: probe_h265.then_some(h265),
        start,
        thread,
    }
}

impl Prepared {
    /// Run a single-viewer session. Whoever cancels `shared.cancel`, C is woken at once.
    #[cfg(test)]
    pub fn start(self, shared: Arc<SessionShared>, config: WorkerConfig) -> WorkerHandle {
        let room = RoomShared::single(&shared, &config);
        self.start_room(room, config.admission)
    }

    /// Run a shared multi-viewer room. Whoever cancels `room.cancel`, C is woken at once.
    pub fn start_room(
        self,
        room: Arc<RoomShared>,
        admission: Option<Arc<dyn ControlAdmission>>,
    ) -> WorkerHandle {
        let (cancel, wake) = (room.cancel.clone(), room.wake.clone());
        let (cancelled, woken) = (cancel.clone(), wake.clone());
        // Only a thread that already ended misses this, and then it reports
        // how it ended.
        let _ = self.start.send(RoomStart { room, admission });
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
    room: &Arc<RoomShared>,
    backend: Result<B>,
    admission: Option<Arc<dyn ControlAdmission>>,
) -> WorkerExit {
    let started = Waiter::new(&room.wake).and_then(|waiter| {
        Ok((
            waiter,
            Engine::start_room(room, backend?, admission, Monotonic)?,
        ))
    });
    let (waiter, mut engine) = match started {
        Ok(started) => started,
        Err(failure) => {
            // Without a backend this session can never capture. Report why;
            // the viewer reconnects with backoff, which retries the backend.
            let reason = unavailable_reason(&failure);
            let message = failure.to_string();
            let state = room.state.lock();
            for (_, cfg) in state.slots.iter().flatten() {
                cfg.shared.state.send_modify(|published| {
                    published.record.state = SessionState::CaptureUnavailable;
                    published.record.reason = Some(reason);
                    published.record.message = message.clone();
                });
            }
            drop(state);
            return WorkerExit::capture_error(failure);
        }
    };
    loop {
        match engine.iterate() {
            Step::Wait(deadline) => {
                if let Err(failure) = waiter.wait(deadline) {
                    return WorkerExit::capture_error(failure);
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

/// Per-viewer state owned by thread C.
struct ViewerSlot {
    pub conn_id: u64,
    pub slot: PeerSlot,
    pub color: PeerColor,
    pub shared: Arc<SessionShared>,
    pub limits: VideoLimits,
    pub cursor_shapes: bool,
    pub peer_cursors: bool,
    /// The copy published to `shared.state` after every iteration.
    pub state: EngineState,
    /// Why control last ended, kept until the viewer asks for control again.
    pub control_note: Option<(StateReason, String)>,
    /// A frame was produced for this slot since the last desktop change.
    pub streaming: bool,
    pub paused: bool,
    /// The last input sequence taken off the lane, applied or not.
    pub sequence: u64,
    pub cursor: CursorEcho,
    pub last_lease: Option<Instant>,
    /// Held input was released for a presentation gone stale; nothing new is
    /// admitted until a fresh one arrives.
    pub stale_released: bool,
    /// The lane's commands while they run, reused by every drain.
    pub drained: VecDeque<(LaneCommand, Instant)>,
    pub hold: DragHold,
    pub held: Held,
    /// Last pointer coordinates injected by this viewer in the current generation.
    pub pointer: Option<(u32, u32)>,
    pub notice_serial: u64,
    pub notice_until: Option<Instant>,
}

/// The engine of thread C: its backend, the desktop generation and the
/// control grant, the pacer and the published state.
struct Engine<B: Backend, K: Clock = Monotonic> {
    room: Arc<RoomShared>,
    slots: [Option<ViewerSlot>; MAX_VIEWERS],
    backend: B,
    clock: K,
    codec: VideoCodec,
    limits: VideoLimits,
    admission: Option<Arc<dyn ControlAdmission>>,
    lease: Option<Box<dyn ControlLease>>,
    /// The policy epoch control was last checked against.
    policy: u64,
    context: Option<DesktopContext>,
    modes: Option<ModeList>,
    video: Option<VideoSize>,
    gop: u32,
    /// Why capture is unavailable, while it is.
    unavailable: Option<(StateReason, String)>,
    /// A frame was produced since the last desktop change.
    streaming: bool,
    paused: bool,
    frame_id: u64,
    /// The next submit must be an IDR (new encoder, pause); the submit that
    /// carries it clears it.
    keyframe: bool,
    pacer: Pacer,
    pace_fps: u32,
    /// The next context check, or the next retry while unavailable.
    context_due: Instant,
    cursor_due: Instant,
    /// At least one attached viewer reads shapes, and the backend reports them.
    cursor_shapes: bool,
    cursor_shape: Option<CursorShape>,
    /// When to poll the encoder again while it holds units.
    drain_due: Instant,
    /// Held while this session captures.
    timer: Option<TimerResolution>,
    /// A terminal backend error, kept typed until the session chooses its stop reason.
    fatal: Option<io::Error>,
    window: Window,
    stats: WindowStats,
    /// Until when the display is still changing after a mode change: a
    /// capture failure is `DisplayChanging` then, and no other change is
    /// taken.
    settle_until: Option<Instant>,
    /// Which viewer's move last positioned the OS cursor, and where.
    last_pointer_owner: Option<(PeerSlot, u32, u32)>,
    mover_moved_since_sample: bool,
    peer_cursors_local: PeerCursorSnapshot,
    peer_cursor_seq: u64,
    room_epoch: u64,
}

impl<B: Backend, K: Clock> Engine<B, K> {
    #[cfg(test)]
    fn start(
        shared: &Arc<SessionShared>,
        backend: B,
        config: WorkerConfig,
        clock: K,
    ) -> Result<Self> {
        config.limits.validate()?;
        let room = RoomShared::single(shared, &config);
        Self::start_room(&room, backend, config.admission, clock)
    }

    fn start_room(
        room: &Arc<RoomShared>,
        mut backend: B,
        admission: Option<Arc<dyn ControlAdmission>>,
        clock: K,
    ) -> Result<Self> {
        let (codec, initial_limits, pace_fps) = {
            let state = room.state.lock();
            let mut limits: Option<VideoLimits> = None;
            let mut pace_fps: Option<u32> = None;
            for (_, cfg) in state.slots.iter().flatten() {
                cfg.limits.validate()?;
                limits = Some(match limits {
                    None => cfg.limits,
                    Some(cur) => VideoLimits {
                        max_width: cur.max_width.min(cfg.limits.max_width),
                        max_height: cur.max_height.min(cfg.limits.max_height),
                        max_fps: cur.max_fps.max(cfg.limits.max_fps),
                    },
                });
                let slot_pace = cfg.shared.targets.pace_fps.load(Ordering::Acquire).max(1);
                pace_fps = Some(pace_fps.map_or(slot_pace, |cur| cur.max(slot_pace)));
            }
            let limits = limits.unwrap_or_else(|| VideoLimits::for_viewer(60.0));
            let pace_fps = pace_fps.unwrap_or_else(|| limits.max_fps.max(1));
            (state.codec, limits, pace_fps)
        };
        initial_limits.validate()?;
        backend.bind(Waker(room.wake.clone()));
        backend.configure_video(initial_limits, initial_limits.max_fps)?;
        let now = clock.now();
        let mut engine = Self {
            room: room.clone(),
            slots: std::array::from_fn(|_| None),
            backend,
            clock,
            codec,
            limits: initial_limits,
            admission,
            lease: None,
            policy: room.policy.get(),
            context: None,
            modes: None,
            video: None,
            gop: 0,
            unavailable: None,
            streaming: false,
            paused: false,
            frame_id: 0,
            keyframe: true,
            pacer: Pacer::new(pace_fps, now),
            pace_fps,
            context_due: now,
            cursor_due: now,
            cursor_shapes: false,
            cursor_shape: None,
            drain_due: now,
            timer: None,
            fatal: None,
            window: Window::new(now),
            stats: WindowStats::default(),
            settle_until: None,
            last_pointer_owner: None,
            mover_moved_since_sample: false,
            peer_cursors_local: PeerCursorSnapshot::default(),
            peer_cursor_seq: 0,
            room_epoch: 0,
        };
        engine.sync_slots();
        Ok(engine)
    }

    /// Synchronize attached viewer slots and negotiated codec from `room.state`.
    pub(super) fn sync_slots(&mut self) {
        if self.room.epoch.load(Ordering::Acquire) == self.room_epoch {
            return;
        }
        let (codec, incoming) = {
            let state = self.room.state.lock();
            self.room_epoch = self.room.epoch.load(Ordering::Acquire);
            (state.codec, state.slots.clone())
        };
        if codec != self.codec {
            self.codec = codec;
            self.keyframe = true;
            if let Err(failure) = self
                .backend
                .configure_video(self.limits, self.limits.max_fps)
            {
                self.fail(failure, self.clock.now());
            }
        }
        let allowed = self.room.policy.allowed();
        let mut added_peer_reader = false;
        for (idx, next) in incoming.into_iter().enumerate() {
            let detach_current = match (&self.slots[idx], &next) {
                (Some(cur), Some((conn_id, cfg))) => {
                    cur.conn_id != *conn_id || cfg.shared.cancel.is_cancelled()
                }
                (Some(_), None) => true,
                (None, _) => false,
            };
            if detach_current {
                self.detach_slot(idx);
            }
            if self.slots[idx].is_none()
                && let Some((conn_id, cfg)) = next
                && !cfg.shared.cancel.is_cancelled()
                && cfg.limits.validate().is_ok()
            {
                let mut state = cfg.shared.state.borrow().clone();
                state.record.control_allowed = allowed;
                if let Some(context) = &self.context {
                    state.record.display = Some(context.display.clone());
                    state.record.desktop = context.desktop;
                }
                state.record.modes = self.modes.clone();
                state.record.video = self.video;
                state.gop = self.gop;
                state.capture_fps = self.stats.capture_fps;
                state.encoder_dwell = self.stats.encoder_dwell;
                state.input_wait = self.stats.input_wait;
                let mut cursor = CursorEcho::default();
                if cfg.cursor_shapes
                    && let Some(shape) = &self.cursor_shape
                {
                    cursor.reshape(shape.clone());
                }
                if cfg.peer_cursors {
                    added_peer_reader = true;
                }
                self.slots[idx] = Some(ViewerSlot {
                    conn_id,
                    slot: cfg.shared.slot,
                    color: cfg.color,
                    shared: cfg.shared,
                    limits: cfg.limits,
                    cursor_shapes: cfg.cursor_shapes,
                    peer_cursors: cfg.peer_cursors,
                    state,
                    control_note: None,
                    streaming: false,
                    paused: false,
                    sequence: 0,
                    cursor,
                    last_lease: None,
                    stale_released: false,
                    drained: VecDeque::new(),
                    hold: DragHold::default(),
                    held: Held::default(),
                    pointer: None,
                    notice_serial: 0,
                    notice_until: None,
                });
                self.keyframe = true;
            }
        }
        self.cursor_shapes = self
            .slots
            .iter()
            .flatten()
            .any(|slot| !slot.paused && slot.cursor_shapes);
        if added_peer_reader {
            let snapshot = self.peer_cursors_local;
            self.room.peer_cursors.send_if_modified(|published| {
                let changed = *published != snapshot;
                if changed {
                    *published = snapshot;
                }
                changed
            });
        }
    }

    pub(super) fn detach_slot(&mut self, idx: usize) {
        if self.slots[idx].is_none() {
            return;
        }
        self.release_slot_inputs(idx);
        self.drop_slot_held(idx);
        if let Some(slot) = &self.slots[idx]
            && self
                .last_pointer_owner
                .is_some_and(|(owner, _, _)| owner == slot.slot)
        {
            self.last_pointer_owner = None;
        }
        self.hide_peer_cursor(idx);
        self.slots[idx] = None;
        self.sync_lease();
        self.cursor_shapes = self
            .slots
            .iter()
            .flatten()
            .any(|slot| !slot.paused && slot.cursor_shapes);
    }

    pub(super) fn has_peer_cursor_observer(&self, mover_idx: usize) -> bool {
        self.slots
            .iter()
            .enumerate()
            .any(|(idx, slot)| idx != mover_idx && slot.as_ref().is_some_and(|s| s.peer_cursors))
    }

    fn any_peer_cursor_reader(&self) -> bool {
        self.slots.iter().flatten().any(|slot| slot.peer_cursors)
    }

    pub(super) fn publish_peer_cursor(&mut self, idx: usize, x: i32, y: i32) {
        let Some(slot) = &self.slots[idx] else {
            return;
        };
        let color = slot.color;
        let prev = self.peer_cursors_local.entries[idx];
        if prev.visible && prev.color == color && prev.x == x && prev.y == y {
            return;
        }
        self.peer_cursor_seq += 1;
        self.peer_cursors_local.entries[idx] = crate::shared::PeerCursorEntry {
            seq: self.peer_cursor_seq,
            color,
            x,
            y,
            visible: true,
        };
        if self.has_peer_cursor_observer(idx) {
            self.room.peer_cursors.send_replace(self.peer_cursors_local);
        }
    }

    pub(super) fn hide_peer_cursor(&mut self, idx: usize) {
        if !self.peer_cursors_local.entries[idx].visible {
            return;
        }
        self.peer_cursor_seq += 1;
        self.peer_cursors_local.entries[idx].seq = self.peer_cursor_seq;
        self.peer_cursors_local.entries[idx].visible = false;
        if self.any_peer_cursor_reader() {
            self.room.peer_cursors.send_replace(self.peer_cursors_local);
        }
    }

    pub(super) fn hide_all_peer_cursors(&mut self) {
        let mut changed = false;
        for entry in &mut self.peer_cursors_local.entries {
            if entry.visible {
                self.peer_cursor_seq += 1;
                entry.seq = self.peer_cursor_seq;
                entry.visible = false;
                changed = true;
            }
        }
        if changed && self.any_peer_cursor_reader() {
            self.room.peer_cursors.send_replace(self.peer_cursors_local);
        }
    }

    fn iterate(&mut self) -> Step {
        let now = self.clock.now();
        if self.room.cancel.is_cancelled() {
            return Step::Exit(WorkerExit::Cancelled);
        }
        self.sync_slots();
        let policy = self.room.policy.get();
        let allowed = self.room.policy.allowed();
        if policy != self.policy {
            self.policy = policy;
            self.release_all_control();
            // Revoking control gives the owner their display back; a grant,
            // which moves the epoch too, does not.
            if !allowed {
                self.restore_mode(now);
            }
        }
        for slot in self.slots.iter_mut().flatten() {
            slot.state.record.control_allowed = allowed;
        }
        loop {
            let command = self.room.owner.lock().pop_front();
            let Some(command) = command else {
                break;
            };
            if self.room.cancel.is_cancelled() {
                return Step::Exit(WorkerExit::Cancelled);
            }
            let now = self.clock.now();
            self.display_mode(None, command, now);
        }
        for idx in 0..MAX_VIEWERS {
            let Some(cancelled) = self.slots[idx]
                .as_ref()
                .map(|slot| slot.shared.cancel.is_cancelled())
            else {
                continue;
            };
            if cancelled {
                if self.room.cancel.is_cancelled() {
                    return Step::Exit(WorkerExit::Cancelled);
                }
                self.detach_slot(idx);
                continue;
            }
            if self.slots[idx]
                .as_ref()
                .is_some_and(|slot| slot.shared.lane.take_overflow())
            {
                self.slot_overflowed(idx);
            }
            if self.slots[idx]
                .as_ref()
                .is_some_and(|slot| slot.shared.auth.lock().take_stale())
            {
                self.release_slot_inputs(idx);
                self.drop_slot_held(idx);
            }
            self.release_slot_if_stale(idx, now);
            if self.slots[idx]
                .as_ref()
                .and_then(|slot| slot.hold.until)
                .is_some_and(|until| now >= until)
                && !self.release_slot_hold(idx)
            {
                if self.room.cancel.is_cancelled() {
                    return Step::Exit(WorkerExit::Cancelled);
                }
                self.detach_slot(idx);
                continue;
            }
            if !self.drain_slot_lane(idx) {
                if self.room.cancel.is_cancelled() {
                    return Step::Exit(WorkerExit::Cancelled);
                }
                self.detach_slot(idx);
            }
        }
        if self.fatal.is_none() {
            self.renew_leases(now);
            self.capture_step(now);
        }
        // Only a live capture may execute input queued during its backend call.
        if self.fatal.is_none() {
            if self.room.cancel.is_cancelled() {
                return Step::Exit(WorkerExit::Cancelled);
            }
            self.sync_slots();
            for idx in 0..MAX_VIEWERS {
                if self.slots[idx].is_some() && !self.drain_slot_lane(idx) {
                    if self.room.cancel.is_cancelled() {
                        return Step::Exit(WorkerExit::Cancelled);
                    }
                    self.detach_slot(idx);
                }
            }
        }
        if let Some(failure) = self.fatal.take() {
            return Step::Exit(WorkerExit::capture_error(failure));
        }
        let now = self.clock.now();
        self.sample_cursor(now);
        if let Some(rolled) = self.window.roll(now) {
            self.stats = rolled;
            for slot in self.slots.iter_mut().flatten() {
                slot.state.capture_fps = rolled.capture_fps;
                slot.state.encoder_dwell = rolled.encoder_dwell;
                slot.state.input_wait = rolled.input_wait;
            }
        }
        // Fine process timers while capturing serve the network tasks too;
        // a paused or unavailable session gives them back.
        let has_slots = self.slots.iter().any(Option::is_some);
        let capturing = !self.paused && self.unavailable.is_none() && has_slots;
        if capturing != self.timer.is_some() {
            self.timer = capturing.then(TimerResolution::hold);
        }
        for slot in self.slots.iter_mut().flatten() {
            if slot.notice_until.is_some_and(|until| now >= until) {
                slot.notice_until = None;
                slot.state.notice = None;
            }
        }
        self.publish_all();
        Step::Wait(self.deadline(now))
    }

    pub(super) fn notice_slot(
        &mut self,
        idx: usize,
        code: NoticeCode,
        message: String,
        now: Instant,
    ) {
        let Some(slot) = &mut self.slots[idx] else {
            return;
        };
        slot.notice_serial += 1;
        slot.state.notice = Some((code, message, slot.notice_serial));
        slot.notice_until = Some(now + NOTICE_TTL);
    }

    pub(super) fn notice_all(&mut self, code: NoticeCode, message: String, now: Instant) {
        for idx in 0..MAX_VIEWERS {
            if self.slots[idx].is_some() {
                self.notice_slot(idx, code, message.clone(), now);
            }
        }
    }

    pub(super) fn publish_slot(&mut self, idx: usize) {
        let unavailable = self.unavailable.clone();
        let Some(slot) = &mut self.slots[idx] else {
            return;
        };
        slot.state.record.state = if slot.paused {
            SessionState::Paused
        } else if unavailable.is_some() {
            SessionState::CaptureUnavailable
        } else if slot.streaming {
            SessionState::Streaming
        } else {
            SessionState::Connecting
        };
        let (reason, message) = unavailable
            .as_ref()
            .or(slot.control_note.as_ref())
            .map_or((None, String::new()), |(reason, message)| {
                (Some(*reason), message.clone())
            });
        slot.state.record.reason = reason;
        slot.state.record.message = message;
        let next = &slot.state;
        slot.shared.state.send_if_modified(|published| {
            let changed = *published != *next;
            if changed {
                published.clone_from(next);
            }
            changed
        });
    }

    pub(super) fn publish_all(&mut self) {
        for idx in 0..MAX_VIEWERS {
            if self.slots[idx].is_some() {
                self.publish_slot(idx);
            }
        }
    }

    /// The earliest of the next tick, cursor sample, context check, encoder
    /// poll, hold end and notice expiry; every wait also ends on a wake.
    fn deadline(&self, now: Instant) -> Instant {
        let mut deadline = now + CONTEXT_CHECK_INTERVAL;
        for slot in self.slots.iter().flatten() {
            if let Some(until) = slot.hold.until {
                deadline = deadline.min(until);
            }
            if let Some(until) = slot.notice_until {
                deadline = deadline.min(until);
            }
        }
        if self.unavailable.is_some() && !self.paused {
            return deadline.min(self.context_due);
        }
        if self.backend.pending_video() > 0 {
            deadline = deadline.min(self.drain_due);
        }
        if self.paused || !self.slots.iter().any(Option::is_some) {
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
