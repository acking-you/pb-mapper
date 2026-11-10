//! The network side of one host session, over the TLS stream split in two.
//! B-recv reads the viewer and never waits for thread C: it books receipts,
//! feeds the QoS controller, validates presentations and queues input on the
//! lane. B-send writes everything else: state, notices, stats, probes, the
//! cursor and the video that send credit admits. A supervisor ends both, and
//! B-send writes the final `Stopped` when the session ends for a reason the
//! viewer should hear.

mod sender;
#[cfg(test)]
mod tests;

use self::sender::Sender;
use crate::{
    QualityPreset, VideoCodec, VideoLimits, VideoSize,
    credit::{CreditLedger, ReceiptAhead},
    idr::IdrReason,
    lane::LaneCommand,
    modes::{Mode, ModeCommand},
    protocol::{
        CAPABILITY_CURSOR_SEQUENCE, CAPABILITY_DISPLAY_MODE, CAPABILITY_ENCODER_DWELL,
        CAPABILITY_FPS_OVERRIDE, StateReason, StateRecord, StopReason, ViewerMessage,
    },
    qos::{Qos, TargetValues},
    shared::{Policy, SessionShared, Targets},
    wire::{self, WireError},
    worker::WorkerExit,
};
use parking_lot::Mutex;
use std::{
    fmt,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
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
    /// An fps cap outside 1..=120.
    FpsCap(u32),
    /// A display mode request from a viewer this host offered none.
    DisplayModeNotOffered,
    /// A malformed or oversized message, as the framing reported it.
    Framing(&'static str),
}
impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthenticatedTwice => f.write_str("authenticated twice"),
            Self::Receipt(ahead) => ahead.fmt(f),
            Self::FpsCap(fps) => write!(f, "fps cap {fps} out of bounds"),
            Self::DisplayModeNotOffered => {
                f.write_str("a display mode request without the capability")
            }
            Self::Framing(framing) => f.write_str(framing),
        }
    }
}

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
    codec: VideoCodec,
    /// The viewer reads the encoder's dwell in `Stats`.
    encoder_dwell: bool,
    /// The viewer reads a `Cursor`'s input watermark.
    cursor_sequence: bool,
    /// Both ends listed `display_mode`: the viewer reads the modes and may
    /// change them.
    display_mode: bool,
    /// When the session was accepted, for display only.
    pub started_unix_ms: u64,
    /// Send credit and QoS, shared by B-recv and B-send only.
    credit: Mutex<CreditLedger>,
    qos: Mutex<Qos>,
    /// Wakes B-send: credit came back, or B-recv refused a control request.
    credit_changed: Notify,
    /// B-recv refused a control request at the edge; B-send reports it.
    control_denied: AtomicBool,
    ending: Mutex<Option<End>>,
}
impl Session {
    /// `capabilities` are the viewer's; `display_modes`, whether the host
    /// listed `display_mode` in `Accepted`.
    pub fn new(
        policy: Arc<Policy>,
        cancel: CancellationToken,
        codec: VideoCodec,
        limits: VideoLimits,
        quality: QualityPreset,
        capabilities: &[String],
        display_modes: bool,
    ) -> Arc<Self> {
        let now = Instant::now();
        let listed = |wanted: &str| capabilities.iter().any(|capability| capability == wanted);
        let qos = Qos::new(limits, codec, quality, listed(CAPABILITY_FPS_OVERRIDE), now);
        let targets = qos.targets(now);
        Arc::new(Self {
            shared: Arc::new(SessionShared::new(
                policy,
                cancel,
                Targets::new(targets.pace_fps, targets.encoder_bps),
            )),
            codec,
            encoder_dwell: listed(CAPABILITY_ENCODER_DWELL),
            cursor_sequence: listed(CAPABILITY_CURSOR_SEQUENCE),
            display_mode: display_modes && listed(CAPABILITY_DISPLAY_MODE),
            started_unix_ms: pb_mapper_core::time::unix_millis(),
            credit: Mutex::new(CreditLedger::new(now)),
            qos: Mutex::new(qos),
            credit_changed: Notify::new(),
            control_denied: AtomicBool::new(false),
            ending: Mutex::new(None),
        })
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
    fn outcome(&self) -> Option<(StopReason, String)> {
        match self.ending.lock().clone() {
            Some(End::Stopped(reason, message)) => Some((reason, message)),
            Some(End::Closed) => None,
            None => Some((
                StopReason::HostStopped,
                stop_message(StopReason::HostStopped).into(),
            )),
        }
    }

    /// The viewer is hidden: no capture and no control until it resumes.
    pub fn paused(&self) -> bool {
        self.shared.targets.paused.load(Ordering::Acquire)
    }

    /// The preset the viewer chose last.
    pub fn quality(&self) -> QualityPreset {
        self.qos.lock().preset()
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
    fn update_qos(&self, now: Instant, change: impl FnOnce(&mut Qos)) {
        let changed = {
            let mut qos = self.qos.lock();
            change(&mut qos);
            self.store_targets(qos.targets(now))
        };
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
            | (targets.paused.swap(values.paused, Ordering::AcqRel) != values.paused)
    }

    /// The QoS tick: unresponsiveness, startup expiry and the 3 s window.
    fn tick(&self, now: Instant) {
        let credit = self.credit.lock().view();
        let produced = self.shared.outgoing.produced();
        self.update_qos(now, |qos| qos.tick(now, &credit, produced));
    }

    /// Whether a unit fits the send window now.
    fn admissible(&self, len: usize, idr: bool, now: Instant) -> bool {
        let (pace, expected, base) = {
            let qos = self.qos.lock();
            (
                qos.targets(now).pace_fps,
                qos.expected_bps(now),
                qos.base_rtt(),
            )
        };
        let mut credit = self.credit.lock();
        let window = credit.window(self.codec, pace, expected, base, now);
        credit.fits(len, idr, &window)
    }

    /// B-recv's handling of one message: inline, never waiting for thread C.
    /// An error is a protocol violation.
    fn handle(&self, message: ViewerMessage, now: Instant) -> Result<(), Violation> {
        let shared = &self.shared;
        match message {
            ViewerMessage::Authenticate { .. } => return Err(Violation::AuthenticatedTwice),
            ViewerMessage::Received { seq } => {
                self.credit
                    .lock()
                    .on_receipt(seq, now)
                    .map_err(Violation::Receipt)?;
                self.update_qos(now, Qos::on_receipt);
                self.credit_changed.notify_one();
            }
            ViewerMessage::ProbeEcho { nonce } => {
                // The receipts' own queue, which a cut needs as its witness
                // unless the probes' delay persists.
                let base = self.qos.lock().base_rtt();
                let ack_queue = base.map(|base| self.credit.lock().ack_queue(base, now));
                self.update_qos(now, |qos| qos.on_probe_echo(nonce, now, ack_queue));
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
            ViewerMessage::Keyframe { reason } => {
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
                self.update_qos(now, |qos| qos.set_preset(preset, now, produced));
            }
            ViewerMessage::Pause {} => {
                self.update_qos(now, Qos::set_paused);
                shared.lane.push(LaneCommand::EndControl);
            }
            ViewerMessage::Resume {} => {
                self.update_qos(now, |qos| qos.resume(now));
                shared.idr.lock().force(IdrReason::Resume);
                shared.wake.notify();
            }
            ViewerMessage::Path { .. } => self.qos.lock().reset_baseline(),
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
    fn for_viewer(&self, record: &StateRecord) -> StateRecord {
        let mut record = record.clone();
        if !self.display_mode {
            record.modes = None;
            if record.reason == Some(StateReason::DisplayChanging) {
                record.reason = Some(StateReason::CaptureFailed);
            }
        }
        record
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
    loop {
        match wire::read_header::<_, ViewerMessage>(&mut reader, HOST_READ_TIMEOUT).await {
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
