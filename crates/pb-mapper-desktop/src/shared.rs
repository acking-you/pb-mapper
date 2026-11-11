//! State shared by one host session's tasks: B-recv and B-send (tokio) and the
//! desktop-bound worker thread C.
//!
//! Lock discipline: never hold two of `auth`, `idr`, the lane's queue or the
//! network task's credit and QoS locks at once; never hold any of them across
//! an `.await` or a backend call; keep each critical section O(1) or O(256).
use crate::{
    CursorShape,
    auth::AuthLedger,
    idr::IdrPolicy,
    lane::InputLane,
    outgoing::Outgoing,
    protocol::{DesktopKind, NoticeCode, SessionState, StateRecord},
    scheduler::Wake,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
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
    /// C to B-send: the newest pointer report.
    pub cursor: tokio::sync::watch::Sender<Option<CursorReport>>,
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

/// C's published engine state. B-send writes a `State` when `record` changes,
/// and each notice serial once.
#[derive(Clone, PartialEq)]
pub(crate) struct EngineState {
    /// What `State` carries.
    pub record: StateRecord,
    pub capture_fps: f32,
    /// Median of the encoder's own submit to output time
    /// (`VideoFormat::encode_us`) of the units it returned in the last second;
    /// `None` when it returned none or measured none (JPEG).
    pub encoder_dwell: Option<Duration>,
    pub skipped_ticks: u64,
    pub dropped_frames: u64,
    /// The last unit's `VideoFormat::gop`; 0 when unknown (JPEG, or an
    /// encoder that reports none).
    pub gop: u32,
    /// `(code, message, serial)`; B-send writes each serial once.
    pub notice: Option<(NoticeCode, String, u64)>,
    pub input_wait: InputWaitStats,
}
impl EngineState {
    pub const FIRST_GENERATION: u64 = 1;

    pub fn connecting(control_allowed: bool) -> Self {
        Self {
            record: StateRecord {
                state: SessionState::Connecting,
                reason: None,
                message: String::new(),
                control_allowed,
                controlling: false,
                generation: Self::FIRST_GENERATION,
                display: None,
                desktop: DesktopKind::Ordinary,
                video: None,
                modes: None,
            },
            capture_fps: 0.0,
            encoder_dwell: None,
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

/// A pointer sample, in the display's physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorUpdate {
    pub generation: u64,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    /// Which shape C knew then, counted per session: 0 none, and each new
    /// one the next number. Only in a session whose viewer reads shapes.
    pub shape: u64,
}

/// A sample C reports. The serial counts reports, so that B-send writes each
/// one once, even one at a position it wrote before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CursorReport {
    pub serial: u64,
    pub cursor: CursorUpdate,
    /// The last input sequence C had taken off the lane when it sampled.
    pub input_sequence: u64,
    /// The shape `cursor.shape` counts, sharing its bitmap.
    pub shape: Option<CursorShape>,
}
