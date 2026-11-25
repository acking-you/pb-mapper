//! The network side of one host session, over the TLS stream split in two.
//! B-recv reads the viewer and never waits for thread C: it books receipts,
//! feeds the QoS controller, validates presentations and queues input on the
//! lane. B-send writes everything else: state, notices, stats, probes, the
//! cursor and the video that send credit admits. A supervisor ends both, and
//! B-send writes the final `Stopped` when the session ends for a reason the
//! viewer should hear.

mod sender;
mod shapes;
#[cfg(test)]
mod tests;

use self::sender::Sender;
use crate::{
    HostPath, QualityPreset, VideoCodec, VideoLimits, VideoSize,
    credit::{CreditLedger, KeyframeAhead, ReceiptAhead},
    idr::IdrReason,
    lane::LaneCommand,
    modes::{Mode, ModeCommand},
    protocol::{
        CAPABILITY_ARRIVAL_TIME, CAPABILITY_CURSOR_SEQUENCE, CAPABILITY_CURSOR_SHAPE,
        CAPABILITY_DISPLAY_MODE, CAPABILITY_ENCODER_DWELL, CAPABILITY_FPS_OVERRIDE,
        CAPABILITY_H265, CAPABILITY_KEYFRAME_SEQ, CAPABILITY_PEER_CURSORS, CAPABILITY_UNLOCK,
        PeerSlot, RateModel, Route, StateReason, StateRecord, StopReason, ViewerMessage,
    },
    qos::{ClosingRate, Qos, TargetValues},
    scheduler::Wake,
    shared::{PeerCursorSnapshot, Policy, SessionShared, Targets},
    watch::ChangeSignal,
    wire::{self, WireError},
    worker::WorkerExit,
};
use parking_lot::Mutex;
use std::{
    borrow::Cow,
    fmt,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::Notify,
};
use tokio_util::sync::CancellationToken;

/// The viewer writes at least a `Ping` this often; silence this long is a
/// dead transport.
pub(crate) const HOST_READ_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const STATS_INTERVAL: Duration = Duration::from_secs(1);
/// After a session is cancelled, the message in flight, the final `Stopped`
/// and the wait for the viewer to close share this.
pub(crate) const STOP_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// What a viewer is told when the host ends its session.
pub(crate) fn stop_message(reason: StopReason) -> &'static str {
    match reason {
        StopReason::HostStopped => "The host stopped sharing",
        StopReason::HostEnded => "The host ended the session",
        StopReason::Restarting => "The host is restarting sharing",
        StopReason::Revoked => "The host withdrew this viewer's access",
        StopReason::CaptureFailed => "The host could not capture its screen",
        StopReason::ProtocolError => "The desktop protocol was broken",
    }
}

/// Why B-recv ended a session as a protocol error. Its text is the message
/// of the viewer's `Stopped{protocol_error}`.
#[derive(Debug)]
enum Violation {
    AuthenticatedTwice,
    Receipt(ReceiptAhead),
    Keyframe(KeyframeAhead),
    /// An fps cap outside 1..=120.
    FpsCap(u32),
    /// A display mode request from a viewer this host offered none.
    DisplayModeNotOffered,
    /// An unlock request from a viewer this host offered none.
    UnlockNotOffered,
    /// A malformed or oversized message, as the framing reported it.
    Framing(&'static str),
}
impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthenticatedTwice => f.write_str("authenticated twice"),
            Self::Receipt(ahead) => ahead.fmt(f),
            Self::Keyframe(ahead) => ahead.fmt(f),
            Self::FpsCap(fps) => write!(f, "fps cap {fps} out of bounds"),
            Self::DisplayModeNotOffered => {
                f.write_str("a display mode request without the capability")
            }
            Self::UnlockNotOffered => f.write_str("an unlock request without the capability"),
            Self::Framing(framing) => f.write_str(framing),
        }
    }
}

/// What a standby's `Resume` does first: it takes its pair's place, unless
/// it no longer stands by. Its room's, under the room's lock.
pub(crate) type Promotion = Box<dyn FnOnce() -> bool + Send>;

/// How a session ended; the first decision wins.
#[derive(Clone)]
enum End {
    /// The viewer hears why, in a final `Stopped`.
    Stopped(StopReason, String),
    /// The transport is gone or failed: nothing more is written.
    Closed,
}

/// One accepted viewer's session. The host keeps it while it holds the viewer
/// slot; the tasks of `run` drive it.
pub(crate) struct Session {
    pub shared: Arc<SessionShared>,
    pub codec: VideoCodec,
    /// Non-H.265 codecs this viewer offered in `Authenticate`.
    pub offered_codecs: Vec<VideoCodec>,
    /// Whether this viewer offered `h265`.
    pub offered_h265: bool,
    /// The viewer reads the encoder's dwell in `Stats`.
    encoder_dwell: bool,
    /// The viewer reads a `Cursor`'s input watermark.
    cursor_sequence: bool,
    /// Both ends listed `display_mode`: the viewer reads the modes and may
    /// change them.
    display_mode: bool,
    /// Both ends listed `unlock`: the viewer may ask to wake and unlock the
    /// lock screen, and reads `NoticeCode::UnlockFailed`.
    pub(super) unlock: bool,
    /// Both ends listed `cursor_shape`: the viewer reads the pointer's shape.
    pub cursor_shape: bool,
    /// Both ends listed `peer_cursors`: the viewer reads other peers' pointers.
    pub peer_cursors: bool,
    /// Both ends listed `arrival_time`: the viewer stamps receipts and probe
    /// echoes, and reads the one-way queues in `Stats`.
    arrival_time: bool,
    /// Both ends listed `keyframe_seq`: a keyframe request may name the last
    /// seq the viewer read.
    keyframe_seq: bool,
    /// When the session was accepted, for display only.
    pub started_unix_ms: u64,
    /// 0 unknown, 1 direct, 2 relay (`ViewerMessage::Path`).
    route: AtomicU8,
    /// Send credit and QoS, shared by B-recv and B-send only.
    credit: Mutex<CreditLedger>,
    qos: Mutex<Qos>,
    /// Wakes B-send: credit came back, or B-recv refused a control request.
    credit_changed: Notify,
    /// A clamped window refused a unit, with no IDR in flight, since the last
    /// receipt: congestion the receipts' controller reads with a small queue.
    blocked: AtomicBool,
    /// The newest rebase generation handed to the credit ledger.
    rebase_generation: AtomicU64,
    /// B-recv refused a control request at the edge; B-send reports it.
    control_denied: AtomicBool,
    ending: Mutex<Option<End>>,
    /// While this session stands by for a pair: what its `Resume` does.
    standby: Mutex<Option<Promotion>>,
    /// The host's: bumped when what its status reads of this session
    /// changed (the record, pause, quality, path).
    changes: ChangeSignal,
}
impl Session {
    /// `capabilities` are the viewer's; `offered`, what the host listed in
    /// `Accepted`.
    #[cfg(test)]
    pub fn new(
        policy: Arc<Policy>,
        cancel: CancellationToken,
        codec: VideoCodec,
        limits: VideoLimits,
        quality: QualityPreset,
        capabilities: &[String],
        offered: &[String],
    ) -> Arc<Self> {
        Self::new_in_room(
            PeerSlot::FIRST,
            Wake::new(),
            Arc::new(tokio::sync::watch::Sender::new(
                PeerCursorSnapshot::default(),
            )),
            policy,
            cancel,
            codec,
            vec![codec],
            limits,
            quality,
            RateModel::Nominal,
            None,
            capabilities,
            offered,
            ChangeSignal::new(),
        )
    }

    /// A session attached to a shared multi-viewer room. `previous` is where
    /// the last session its transport ended left its rate, which starts this
    /// one no higher (`Qos::new`); the host passes it only to a session that
    /// opens a room.
    #[expect(
        clippy::too_many_arguments,
        reason = "each room and handshake parameter is distinct"
    )]
    pub fn new_in_room(
        slot: PeerSlot,
        wake: Wake,
        peer_cursors_tx: Arc<tokio::sync::watch::Sender<PeerCursorSnapshot>>,
        policy: Arc<Policy>,
        cancel: CancellationToken,
        codec: VideoCodec,
        offered_codecs: Vec<VideoCodec>,
        limits: VideoLimits,
        quality: QualityPreset,
        rate_model: RateModel,
        previous: Option<ClosingRate>,
        capabilities: &[String],
        offered: &[String],
        changes: ChangeSignal,
    ) -> Arc<Self> {
        let now = Instant::now();
        let listed = |wanted: &str| capabilities.iter().any(|capability| capability == wanted);
        let both = |wanted: &str| listed(wanted) && offered.iter().any(|value| value == wanted);
        let arrival_time = both(CAPABILITY_ARRIVAL_TIME);
        let mut qos = Qos::new(
            limits,
            codec,
            quality,
            listed(CAPABILITY_FPS_OVERRIDE),
            rate_model,
            previous,
            now,
        );
        if arrival_time {
            qos.use_one_way();
        }
        let targets = qos.targets(now);
        Arc::new(Self {
            shared: Arc::new(SessionShared::new_in_room(
                slot,
                wake,
                peer_cursors_tx,
                policy,
                cancel,
                Targets::new(targets.pace_fps, targets.encoder_bps, targets.min_fps),
            )),
            codec,
            offered_codecs,
            offered_h265: listed(CAPABILITY_H265),
            encoder_dwell: listed(CAPABILITY_ENCODER_DWELL),
            cursor_sequence: listed(CAPABILITY_CURSOR_SEQUENCE),
            display_mode: both(CAPABILITY_DISPLAY_MODE),
            unlock: both(CAPABILITY_UNLOCK),
            cursor_shape: both(CAPABILITY_CURSOR_SHAPE),
            peer_cursors: both(CAPABILITY_PEER_CURSORS),
            arrival_time,
            keyframe_seq: both(CAPABILITY_KEYFRAME_SEQ),
            started_unix_ms: pb_mapper_core::time::unix_millis(),
            route: AtomicU8::new(0),
            credit: Mutex::new(CreditLedger::new(now, arrival_time)),
            qos: Mutex::new(qos),
            credit_changed: Notify::new(),
            blocked: AtomicBool::new(false),
            rebase_generation: AtomicU64::new(0),
            control_denied: AtomicBool::new(false),
            ending: Mutex::new(None),
            standby: Mutex::new(None),
            changes,
        })
    }

    /// Stand by for a pair, paused: `promote` is what its `Resume` does
    /// first. The room calls this under its lock, before the session runs.
    pub fn stand_by(&self, promote: Promotion) {
        *self.standby.lock() = Some(promote);
        self.update_qos(Instant::now(), Qos::set_paused);
    }

    /// The session holds its pair's place now: a `Resume` is a resume.
    pub fn leave_standby(&self) {
        self.standby.lock().take();
    }

    /// Whether the session stands by for a pair.
    pub fn standing_by(&self) -> bool {
        self.standby.lock().is_some()
    }

    /// Whether its transport ended it: the viewer closed it, the connection
    /// failed or went silent, or its standby took its place.
    pub fn transport_ended(&self) -> bool {
        matches!(*self.ending.lock(), Some(End::Closed))
    }

    /// End the session; B-send tells the viewer `reason`.
    pub fn end(&self, reason: StopReason, message: impl Into<String>) {
        self.finish(End::Stopped(reason, message.into()));
    }

    /// End the session without a word: the transport is gone.
    pub fn close(&self) {
        self.finish(End::Closed);
    }

    fn finish(&self, end: End) {
        self.ending.lock().get_or_insert(end);
        self.shared.cancel.cancel();
        self.shared.wake.notify();
    }

    /// What B-send tells the viewer at the end. A session cancelled with no
    /// decision of its own was cancelled by its host stopping.
    pub(crate) fn outcome(&self) -> Option<(StopReason, String)> {
        match self.ending.lock().clone() {
            Some(End::Stopped(reason, message)) => Some((reason, message)),
            Some(End::Closed) => None,
            None => Some((
                StopReason::HostStopped,
                stop_message(StopReason::HostStopped).into(),
            )),
        }
    }

    /// Where this session's rate stood at `now`, if its transport ended it:
    /// the viewer closed it, the connection failed, or the viewer went silent
    /// for `HOST_READ_TIMEOUT`. `None` while it runs, when the host, the
    /// capture or a protocol error ended it, and when its QoS leaves none
    /// (`Qos::closing_rate`).
    pub fn closing_rate(&self, now: Instant) -> Option<ClosingRate> {
        let closed = matches!(*self.ending.lock(), Some(End::Closed));
        closed.then(|| self.qos.lock().closing_rate(now)).flatten()
    }

    /// The viewer is hidden: no capture and no control until it resumes.
    pub fn paused(&self) -> bool {
        self.shared.targets.paused.load(Ordering::Acquire)
    }

    /// The preset the viewer chose last.
    pub fn quality(&self) -> QualityPreset {
        self.qos.lock().preset()
    }

    /// The transport path the viewer reported, if any.
    pub fn reported_path(&self) -> Option<HostPath> {
        match self.route.load(Ordering::Acquire) {
            1 => Some(HostPath::Direct),
            2 => Some(HostPath::Relay),
            _ => None,
        }
    }

    /// The size the encoder is expected to produce, told before thread C
    /// starts: the first IDR gets that size's budget rather than the viewer's
    /// bounds', and the first unit's size corrects it, as every later one does.
    pub fn expect_video_size(&self, video: VideoSize) {
        self.update_qos(Instant::now(), |qos| {
            qos.set_video_size(video.width, video.height);
        });
    }

    /// Change the controller and publish its targets to C. The targets are
    /// stored under the `qos` lock, so the newest decision is the one C reads.
    /// The controller first takes C's newest cap at its encoder's throughput,
    /// so every change, the QoS tick's included, sees it within a tick.
    /// A baseline a rebase moved then reaches the credit ledger, once the
    /// `qos` lock is released; the ledger keeps the newest generation, so of
    /// B-send's and B-recv's writes the older never lands last.
    fn update_qos(&self, now: Instant, change: impl FnOnce(&mut Qos)) {
        let encoder_cap = self.shared.encoder_cap();
        let (changed, (generation, rebase)) = {
            let mut qos = self.qos.lock();
            qos.set_encoder_cap(encoder_cap);
            change(&mut qos);
            (self.store_targets(qos.targets(now)), qos.rebase_state())
        };
        if self
            .rebase_generation
            .fetch_max(generation, Ordering::AcqRel)
            < generation
        {
            self.credit.lock().set_rebase(generation, rebase);
        }
        if changed {
            self.shared.wake.notify();
        }
    }

    fn store_targets(&self, values: TargetValues) -> bool {
        let targets = &self.shared.targets;
        (targets.pace_fps.swap(values.pace_fps, Ordering::AcqRel) != values.pace_fps)
            | (targets
                .encoder_bps
                .swap(values.encoder_bps, Ordering::AcqRel)
                != values.encoder_bps)
            | (targets.min_fps.swap(values.min_fps, Ordering::AcqRel) != values.min_fps)
            | (targets.paused.swap(values.paused, Ordering::AcqRel) != values.paused)
    }

    /// The QoS tick: the rate-model latch, unresponsiveness, startup expiry
    /// and the 3 s window.
    fn tick(&self, now: Instant) {
        let credit = self.credit.lock().view();
        let produced = self.shared.outgoing.produced();
        self.update_qos(now, |qos| qos.tick(now, &credit, produced));
    }

    /// Whether a unit fits the send window now. A clamped window that refuses
    /// one with no IDR in flight marks the session blocked until the next
    /// receipt. The `qos` lock is released before `credit` is taken.
    fn admissible(&self, len: usize, idr: bool, now: Instant) -> bool {
        let (pace, expected, base, clamp) = {
            let qos = self.qos.lock();
            (
                qos.pace_fps(now),
                qos.expected_bps(now),
                qos.base_rtt(),
                qos.clamp_rate(now),
            )
        };
        let mut credit = self.credit.lock();
        let window = credit.window(self.codec, pace, expected, base, clamp, now);
        let fits = credit.fits(len, idr, &window);
        if !fits && window.clamped && credit.idr_inflight().is_none() {
            self.blocked.store(true, Ordering::Release);
        }
        fits
    }

    /// B-recv's handling of one message: inline, never waiting for thread C.
    /// An error is a protocol violation.
    fn handle(&self, message: ViewerMessage, now: Instant) -> Result<(), Violation> {
        let shared = &self.shared;
        match message {
            ViewerMessage::Authenticate { .. } => return Err(Violation::AuthenticatedTwice),
            ViewerMessage::Received { seq, at_us } => {
                // A stamp from a viewer that was not asked for one is ignored.
                let at_us = at_us.filter(|_| self.arrival_time);
                let receipt = self
                    .credit
                    .lock()
                    .on_receipt(seq, now, at_us)
                    .map_err(Violation::Receipt)?;
                let blocked = self.blocked.swap(false, Ordering::AcqRel);
                self.update_qos(now, |qos| qos.on_receipt(receipt, blocked, now));
                self.credit_changed.notify_one();
            }
            ViewerMessage::ProbeEcho { nonce, at_us } => {
                let (base, sent, floor_gap) = {
                    let qos = self.qos.lock();
                    (qos.base_rtt(), qos.probe_sent_at(nonce), qos.floor_gap())
                };
                // The probe's own queue toward the viewer, and the receipts',
                // which a cut needs as its witness unless the probes' delay
                // persists.
                let (forward, ack_queue) = {
                    let mut credit = self.credit.lock();
                    credit.set_floor_gap(floor_gap);
                    let forward = at_us
                        .filter(|_| self.arrival_time)
                        .zip(sent)
                        .and_then(|(at_us, sent)| credit.probe_queue(at_us, sent, now));
                    (forward, base.map(|base| credit.ack_queue(base, now)))
                };
                self.update_qos(now, |qos| {
                    qos.on_probe_echo(nonce, now, forward, ack_queue);
                });
            }
            ViewerMessage::Presented {
                generation,
                frame_id,
            } => {
                // A stale presentation releases held input on C, fail closed.
                if !shared.auth.lock().on_presented(generation, frame_id, now) {
                    shared.wake.notify();
                }
            }
            ViewerMessage::Keyframe { reason, after_seq } => {
                // A viewer that drops predicted frames until the IDR names
                // the last seq it read: B-send stops sending them, unless an
                // IDR already followed that seq. Every JPEG picture stands
                // alone.
                if let Some(after_seq) = after_seq.filter(|_| self.keyframe_seq)
                    && self.codec != VideoCodec::Jpeg
                    && self
                        .credit
                        .lock()
                        .skip_to_keyframe(after_seq, now)
                        .map_err(Violation::Keyframe)?
                {
                    // A head that waits for credit is dropped now.
                    self.credit_changed.notify_one();
                }
                shared.idr.lock().request(IdrReason::Viewer(reason));
                shared.wake.notify();
            }
            ViewerMessage::FpsCap { fps } => {
                if !(1..=120).contains(&fps) {
                    return Err(Violation::FpsCap(fps));
                }
                self.update_qos(now, |qos| qos.set_viewer_cap(fps));
            }
            ViewerMessage::Quality { preset } => {
                let produced = shared.outgoing.produced();
                let before = self.quality();
                self.update_qos(now, |qos| qos.set_preset(preset, now, produced));
                if self.quality() != before {
                    self.changes.bump();
                }
            }
            ViewerMessage::Pause {} => {
                let paused = self.paused();
                self.update_qos(now, Qos::set_paused);
                shared.lane.push(LaneCommand::EndControl);
                if !paused {
                    self.changes.bump();
                }
            }
            ViewerMessage::Resume {} => {
                let paused = self.paused();
                self.update_qos(now, |qos| qos.resume(now));
                self.credit.lock().reset_one_way();
                shared.idr.lock().force(IdrReason::Resume);
                // A standby, resumed, takes its pair's place: thread C then
                // knows it unpaused, and its first unit is the IDR.
                let promote = self.standby.lock().take();
                if let Some(promote) = promote
                    && promote()
                {
                    tracing::info!(
                        target: "pb_mapper_desktop::session",
                        event = "desktop_host_promoted",
                        slot = shared.slot.index(),
                    );
                }
                shared.wake.notify();
                self.credit_changed.notify_one();
                if paused {
                    self.changes.bump();
                }
            }
            ViewerMessage::Path { route } => {
                let code = match route {
                    Route::Direct => 1,
                    Route::RelayTcp | Route::RelayQuic => 2,
                };
                // Only a new path resets the baseline: a standby that took
                // its pair's place measured its own path all along.
                if self.route.swap(code, Ordering::AcqRel) != code {
                    self.changes.bump();
                    self.update_qos(now, Qos::reset_baseline);
                    self.credit.lock().reset_one_way();
                }
            }
            ViewerMessage::Control { enabled } => {
                if enabled && !shared.policy.allowed() {
                    self.control_denied.store(true, Ordering::Release);
                    self.credit_changed.notify_one();
                } else {
                    shared.lane.push(LaneCommand::Control { enabled });
                }
            }
            ViewerMessage::Release {} => {
                shared.lane.push(LaneCommand::Release);
            }
            ViewerMessage::Input { event } => {
                // A revoked policy drops input at the edge; C releases held
                // input on its next iteration.
                if shared.policy.allowed() {
                    shared.lane.push(LaneCommand::Input(event));
                }
            }
            ViewerMessage::Ping {} => {}
            // Thread C decides, and answers a refusal with a notice.
            ViewerMessage::DisplayMode { width, height } => {
                self.display_mode(ModeCommand::Set(Mode { width, height }))?;
            }
            ViewerMessage::RestoreDisplayMode {} => self.display_mode(ModeCommand::Restore)?,
            ViewerMessage::Unlock { password } => {
                if !self.unlock {
                    return Err(Violation::UnlockNotOffered);
                }
                if !shared.policy.allowed() {
                    self.control_denied.store(true, Ordering::Release);
                    self.credit_changed.notify_one();
                } else {
                    shared.lane.push(LaneCommand::Unlock(password));
                }
            }
        }
        Ok(())
    }

    /// Only from a viewer this host offered display modes.
    fn display_mode(&self, command: ModeCommand) -> Result<(), Violation> {
        if !self.display_mode {
            return Err(Violation::DisplayModeNotOffered);
        }
        self.shared.lane.push(LaneCommand::DisplayMode(command));
        Ok(())
    }

    /// What a viewer that does not read display modes is told instead: no
    /// modes, and a display still changing as the capture failure it is.
    /// Borrowed when that changes nothing.
    fn for_viewer<'a>(&self, record: &'a StateRecord) -> Cow<'a, StateRecord> {
        if self.display_mode
            || (record.modes.is_none() && record.reason != Some(StateReason::DisplayChanging))
        {
            return Cow::Borrowed(record);
        }
        let mut record = record.clone();
        record.modes = None;
        if record.reason == Some(StateReason::DisplayChanging) {
            record.reason = Some(StateReason::CaptureFailed);
        }
        Cow::Owned(record)
    }
}

/// Run the session until it ends, then until thread C has exited, so the
/// backend is gone before the viewer slot frees. `worker` resolves when C
/// exits.
pub(crate) async fn run<S>(
    session: Arc<Session>,
    stream: S,
    worker: impl Future<Output = WorkerExit>,
) where
    S: AsyncRead + AsyncWrite,
{
    let started = Instant::now();
    let (mut reader, writer) = tokio::io::split(stream);
    let send = Sender::new(&session, writer).run();
    tokio::pin!(send, worker);
    let mut sent = None;
    let mut exited = false;
    {
        let receive = receive(&session, &mut reader);
        tokio::pin!(receive);
        tokio::select! {
            violation = &mut receive => match violation {
                Some(violation) => session.end(StopReason::ProtocolError, violation.to_string()),
                None => session.close(),
            },
            stopped = &mut send => sent = Some(stopped),
            exit = &mut worker => {
                exited = true;
                match exit {
                    WorkerExit::CaptureEnded(message) => session.end(StopReason::HostEnded, message),
                    WorkerExit::CaptureFailed(failure) => session.end(StopReason::CaptureFailed, failure),
                    WorkerExit::Cancelled => session.close(),
                }
            }
        }
    }
    // B-send ended on its own only when the transport failed.
    session.close();
    let stopped = match sent {
        Some(stopped) => stopped,
        None => send.await,
    };
    if !exited {
        worker.await;
    }
    // No reason: the transport closed or the viewer left.
    let (reason, detail) = session.outcome().unzip();
    let produced = session.shared.outgoing.produced();
    tracing::info!(
        target: "pb_mapper_desktop::session",
        event = "desktop_host_session_ended",
        reason = ?reason,
        detail = ?detail.unwrap_or_default(),
        seconds = started.elapsed().as_secs(),
        frames = produced.frames,
        keyframes = produced.keyframes,
    );
    // The viewer closes once it has read `Stopped`; until then, within the
    // stop budget, its input is read and dropped. Closing with input unread
    // resets the connection, and a reset can discard the `Stopped` still on
    // its way through the tunnel.
    if let Some(deadline) = stopped {
        let _ = tokio::time::timeout_at(
            deadline.into(),
            tokio::io::copy(&mut reader, &mut tokio::io::sink()),
        )
        .await;
    }
}

/// B-recv. Returns the protocol violation that ended it, or `None` when the
/// transport closed, failed or went silent.
async fn receive<R: AsyncRead + Unpin>(session: &Session, mut reader: R) -> Option<Violation> {
    let mut record = Vec::new();
    loop {
        match wire::read_header_with::<_, ViewerMessage>(
            &mut reader,
            &mut record,
            HOST_READ_TIMEOUT,
        )
        .await
        {
            Ok(Some(message)) => {
                if let Err(violation) = session.handle(message, Instant::now()) {
                    return Some(violation);
                }
            }
            Ok(None) | Err(WireError::TimedOut | WireError::Io(_)) => return None,
            Err(WireError::Framing(framing)) => return Some(Violation::Framing(framing)),
        }
    }
}
