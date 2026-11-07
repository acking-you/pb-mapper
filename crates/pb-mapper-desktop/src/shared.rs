//! State shared by one host session's tasks: B-recv and B-send (tokio) and the
//! desktop-bound worker thread C.
//!
//! Lock discipline: never hold two of `auth`, `idr`, the lane's queue or the
//! network task's credit and QoS locks at once; never hold any of them across
//! an `.await` or a backend call; keep each critical section O(1) or O(256).
use crate::{
    Display,
    auth::AuthLedger,
    idr::IdrPolicy,
    lane::InputLane,
    outgoing::Outgoing,
    protocol::{DesktopKind, NoticeCode, SessionState, StateReason, VideoSize},
    scheduler::Wake,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio_util::sync::CancellationToken;

/// The host's control permission: bit 0 allows control, and every change
/// advances the epoch above it.
pub(crate) struct Policy(AtomicU64);
impl Policy {
    pub fn new(allow: bool) -> Self {
        Self(AtomicU64::new(u64::from(allow)))
    }
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
    pub fn allowed(&self) -> bool {
        self.get() & 1 != 0
    }
    pub fn set(&self, allow: bool) {
        // A revoke followed immediately by a grant still invalidates the old
        // input generation, even if both occur between capture iterations.
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some(((old & !1) + 2) | u64::from(allow))
            });
    }
}

pub(crate) struct SessionShared {
    /// Session start, for the host status's `since_ms`.
    pub epoch: Instant,
    pub cancel: CancellationToken,
    pub policy: Arc<Policy>,
    pub auth: parking_lot::Mutex<AuthLedger>,
    pub lane: InputLane,
    pub outgoing: Outgoing,
    pub idr: parking_lot::Mutex<IdrPolicy>,
    /// B tasks to C.
    pub targets: Targets,
    /// C to B-send and `Host::status`.
    pub state: tokio::sync::watch::Sender<EngineState>,
    /// C to B-send.
    pub cursor: tokio::sync::watch::Sender<Option<CursorUpdate>>,
    /// Microseconds since `epoch` of the last pointer input (B-recv), for the
    /// cursor echo suppression.
    pub pointer_input_at_us: AtomicU64,
    /// Wakes C's waiter.
    pub wake: Wake,
}
impl SessionShared {
    pub fn new(policy: Arc<Policy>, cancel: CancellationToken, targets: Targets) -> Self {
        let wake = Wake::new();
        let control_allowed = policy.allowed();
        Self {
            epoch: Instant::now(),
            cancel,
            policy,
            auth: parking_lot::Mutex::new(AuthLedger::new(EngineState::FIRST_GENERATION)),
            lane: InputLane::new(wake.clone()),
            outgoing: Outgoing::new(),
            idr: parking_lot::Mutex::new(IdrPolicy::new()),
            targets,
            state: tokio::sync::watch::Sender::new(EngineState::connecting(control_allowed)),
            cursor: tokio::sync::watch::Sender::new(None),
            pointer_input_at_us: AtomicU64::new(0),
            wake,
        }
    }
}

/// What the QoS controller asks of C, published after every change.
pub(crate) struct Targets {
    pub pace_fps: AtomicU32,
    pub encoder_bps: AtomicU32,
    pub paused: AtomicBool,
}
impl Targets {
    pub fn new(pace_fps: u32, encoder_bps: u32) -> Self {
        Self {
            pace_fps: AtomicU32::new(pace_fps),
            encoder_bps: AtomicU32::new(encoder_bps),
            paused: AtomicBool::new(false),
        }
    }
}

/// C's published engine state. B-send writes a `State` when its visible subset
/// changes and each notice serial once.
#[derive(Clone, PartialEq)]
pub(crate) struct EngineState {
    pub state: SessionState,
    pub reason: Option<StateReason>,
    pub message: String,
    pub control_allowed: bool,
    pub controlling: bool,
    pub generation: u64,
    pub display: Option<Display>,
    pub desktop: DesktopKind,
    pub video: Option<VideoSize>,
    pub capture_fps: f32,
    pub skipped_ticks: u64,
    pub dropped_frames: u64,
    pub gop: u32,
    /// `(code, message, serial)`; B-send writes each serial once.
    pub notice: Option<(NoticeCode, String, u64)>,
    pub input_wait: InputWaitStats,
}
impl EngineState {
    pub const FIRST_GENERATION: u64 = 1;

    pub fn connecting(control_allowed: bool) -> Self {
        Self {
            state: SessionState::Connecting,
            reason: None,
            message: String::new(),
            control_allowed,
            controlling: false,
            generation: Self::FIRST_GENERATION,
            display: None,
            desktop: DesktopKind::Ordinary,
            video: None,
            capture_fps: 0.0,
            skipped_ticks: 0,
            dropped_frames: 0,
            gop: 0,
            notice: None,
            input_wait: InputWaitStats::default(),
        }
    }
}

/// `input_wait` (B-recv enqueue to execution on C) over the last second.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InputWaitStats {
    pub p50_us: u32,
    pub p99_us: u32,
    pub max_us: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorUpdate {
    pub generation: u64,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}
