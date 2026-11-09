//! The network side of one host session, over the TLS stream split in two.
//! B-recv reads the viewer and never waits for thread C: it books receipts,
//! feeds the QoS controller, validates presentations and queues input on the
//! lane. B-send writes everything else: state, notices, stats, probes, the
//! cursor and the video that send credit admits. A supervisor ends both, and
//! B-send writes the final `Stopped` when the session ends for a reason the
//! viewer should hear.
use crate::{
    QualityPreset, VideoCodec, VideoLimits, VideoSize,
    credit::CreditLedger,
    idr::IdrReason,
    lane::LaneCommand,
    modes::{Mode, ModeCommand},
    outgoing::OUTGOING_MAX_AGE,
    protocol::{
        CAPABILITY_CURSOR_SEQUENCE, CAPABILITY_DISPLAY_MODE, CAPABILITY_ENCODER_DWELL,
        CAPABILITY_FPS_OVERRIDE, HostMessage, NoticeCode, StateReason, StateRecord, StopReason,
        ViewerMessage,
    },
    qos::{QOS_TICK, Qos, TargetValues},
    shared::{CursorReport, EngineState, Policy, SessionShared, Targets},
    wire::{self, MAX_PAYLOAD},
    worker::WorkerExit,
};
use parking_lot::Mutex;
use serde::Serialize;
use std::{
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    sync::{Notify, watch},
};
use tokio_util::sync::CancellationToken;

/// The viewer writes at least a `Ping` this often; silence this long is a
/// dead transport.
pub(crate) const HOST_READ_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const STATS_INTERVAL: Duration = Duration::from_secs(1);
/// After a session is cancelled, the message in flight, the final `Stopped`
/// and the wait for the viewer to close share this.
pub(crate) const STOP_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// How soon B-send looks again at a head that waits for credit.
const CREDIT_RECHECK: Duration = Duration::from_millis(20);
const CONTROL_DENIED: &str = "Control is not allowed on this computer";

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
            started_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                }),
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
        let credit = self.credit.lock();
        let window = credit.window(self.codec, pace, expected, base, now);
        credit.fits(len, idr, &window)
    }

    /// B-recv's handling of one message: inline, never waiting for thread C.
    /// An error is a protocol violation.
    fn handle(&self, message: ViewerMessage, now: Instant) -> Result<(), String> {
        let shared = &self.shared;
        match message {
            ViewerMessage::Authenticate { .. } => return Err("authenticated twice".into()),
            ViewerMessage::Received { seq } => {
                self.credit
                    .lock()
                    .on_receipt(seq, now)
                    .map_err(|e| e.to_string())?;
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
                    return Err(format!("fps cap {fps} out of bounds"));
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
    fn display_mode(&self, command: ModeCommand) -> Result<(), String> {
        if !self.display_mode {
            return Err("a display mode request without the capability".into());
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
                Some(violation) => session.end(StopReason::ProtocolError, violation),
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
async fn receive<R: AsyncRead + Unpin>(session: &Session, mut reader: R) -> Option<String> {
    loop {
        match wire::read_header::<_, ViewerMessage>(&mut reader, HOST_READ_TIMEOUT).await {
            Ok(Some(message)) => {
                if let Err(violation) = session.handle(message, Instant::now()) {
                    return Some(violation);
                }
            }
            Ok(None) => return None,
            // The framing reports a malformed or oversized message as `Other`;
            // every other kind is the transport failing or timing out.
            Err(failure) if failure.kind() == io::ErrorKind::Other => {
                return Some(failure.to_string());
            }
            Err(_) => return None,
        }
    }
}

/// A write could not finish: the transport failed, or the stop budget ran out.
struct Abandoned;

/// Write one message. Once the session is cancelled, the message in flight and
/// the final `Stopped` share `STOP_WRITE_TIMEOUT`, so a local stop never waits
/// for a slow viewer to drain video.
async fn write<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    cancel: &CancellationToken,
    stop_at: &mut Option<Instant>,
    header: &T,
    payload: Option<&[u8]>,
) -> Result<(), Abandoned> {
    let write = wire::write_message(writer, header, payload);
    tokio::pin!(write);
    loop {
        if let Some(deadline) = *stop_at {
            return match tokio::time::timeout_at(deadline.into(), &mut write).await {
                Ok(Ok(())) => Ok(()),
                _ => Err(Abandoned),
            };
        }
        tokio::select! {
            biased;
            written = &mut write => return written.map_err(|_| Abandoned),
            () = cancel.cancelled() => *stop_at = Some(Instant::now() + STOP_WRITE_TIMEOUT),
        }
    }
}

#[derive(Default)]
struct Counters {
    frames: u64,
    bytes: u64,
    /// Units B-send dropped: of an old generation, or aged in the hand-off.
    dropped: u64,
    /// Video payload bytes since the last `Stats`.
    interval_bytes: u64,
}

/// B-send. One pass writes, in order: state, notice, QoS tick, stats, probe,
/// cursor and admitted video; then it waits for the next reason to write.
struct Sender<'a, W> {
    session: &'a Session,
    writer: W,
    state: watch::Receiver<EngineState>,
    cursor: watch::Receiver<Option<CursorReport>>,
    /// The engine state last written as `State`.
    written: Option<StateRecord>,
    notice_serial: u64,
    /// The last cursor report written, or dropped as stale.
    cursor_serial: u64,
    next_tick: Instant,
    next_stats: Instant,
    stats_since: Instant,
    counters: Counters,
    stop_at: Option<Instant>,
}
impl<'a, W: AsyncWrite + Unpin> Sender<'a, W> {
    fn new(session: &'a Session, writer: W) -> Self {
        let now = Instant::now();
        Self {
            session,
            writer,
            state: session.shared.state.subscribe(),
            cursor: session.shared.cursor.subscribe(),
            written: None,
            notice_serial: 0,
            cursor_serial: 0,
            next_tick: now + QOS_TICK,
            next_stats: now + STATS_INTERVAL,
            stats_since: now,
            counters: Counters::default(),
            stop_at: None,
        }
    }

    /// Returns the stop budget's deadline when the final `Stopped` went out.
    async fn run(mut self) -> Option<Instant> {
        // The first probe goes out before anything is captured, so the
        // baseline starts clean.
        if self.probe(Instant::now()).await.is_err() {
            return None;
        }
        while !self.session.shared.cancel.is_cancelled() {
            match self.pass().await {
                Ok(wake_at) => self.wait(wake_at).await,
                Err(Abandoned) => return None,
            }
        }
        self.stop().await
    }

    async fn send<T: Serialize>(
        &mut self,
        header: &T,
        payload: Option<&[u8]>,
    ) -> Result<(), Abandoned> {
        write(
            &mut self.writer,
            &self.session.shared.cancel,
            &mut self.stop_at,
            header,
            payload,
        )
        .await
    }

    /// The final `Stopped`, then a TLS close, within what is left of the stop
    /// budget. Returns that budget's deadline once both went out.
    async fn stop(mut self) -> Option<Instant> {
        let (reason, message) = self.session.outcome()?;
        let deadline = *self
            .stop_at
            .get_or_insert_with(|| Instant::now() + STOP_WRITE_TIMEOUT);
        let stopped = HostMessage::Stopped { reason, message };
        tokio::time::timeout_at(deadline.into(), async {
            wire::write_message(&mut self.writer, &stopped, None).await?;
            self.writer.shutdown().await
        })
        .await
        .ok()?
        .ok()?;
        Some(deadline)
    }

    /// One pass. Returns when to look again if nothing wakes B-send first.
    async fn pass(&mut self) -> Result<Instant, Abandoned> {
        let now = Instant::now();
        let engine = self.write_engine().await?;
        if now >= self.next_tick {
            self.next_tick = now + QOS_TICK;
            self.session.tick(now);
        }
        if now >= self.next_stats {
            self.next_stats = now + STATS_INTERVAL;
            self.stats(&engine, now).await?;
        }
        self.probe(now).await?;
        let probe_due = self.session.qos.lock().probe_due_at(now);
        let mut wake_at = self.next_tick.min(self.next_stats);
        if let Some(due) = probe_due {
            wake_at = wake_at.min(due);
        }
        // Paused: no cursor and no video, while probes, state and stats go on.
        if !self.session.shared.targets.paused.load(Ordering::Acquire) {
            self.cursor().await?;
            if self.video().await? {
                wake_at = wake_at.min(Instant::now() + CREDIT_RECHECK);
            }
        }
        Ok(wake_at)
    }

    async fn wait(&mut self, wake_at: Instant) {
        let session = self.session;
        tokio::select! {
            () = session.shared.cancel.cancelled() => {}
            () = session.shared.outgoing.notified() => {}
            () = session.credit_changed.notified() => {}
            _ = self.state.changed() => {}
            _ = self.cursor.changed() => {}
            () = tokio::time::sleep_until(wake_at.into()) => {}
        }
    }

    /// What the engine published last, after writing its `State` and its
    /// notice, each only if new.
    async fn write_engine(&mut self) -> Result<EngineState, Abandoned> {
        let engine = self.state.borrow_and_update().clone();
        self.write_state(&engine).await?;
        if let Some((code, message, serial)) = &engine.notice
            && *serial > self.notice_serial
        {
            self.notice_serial = *serial;
            // A display mode notice only to a viewer that reads them.
            if *code != NoticeCode::DisplayModeFailed || self.session.display_mode {
                let notice = HostMessage::Notice {
                    code: *code,
                    message: message.clone(),
                };
                self.send(&notice, None).await?;
            }
        }
        Ok(engine)
    }

    /// Write `State` when the engine's record changed, or to report a refused
    /// control request: that shows as its reason until the engine state
    /// changes.
    async fn write_state(&mut self, engine: &EngineState) -> Result<(), Abandoned> {
        let record = &self.session.for_viewer(&engine.record);
        let denied = self.session.control_denied.swap(false, Ordering::AcqRel);
        if !denied && self.written.as_ref() == Some(record) {
            return Ok(());
        }
        if let Some(video) = record.video
            && self.written.as_ref().and_then(|written| written.video) != Some(video)
        {
            self.session.update_qos(Instant::now(), |qos| {
                qos.set_video_size(video.width, video.height);
            });
        }
        self.written = Some(record.clone());
        let mut sent = record.clone();
        if denied {
            sent.reason = Some(StateReason::ControlDenied);
            sent.message = CONTROL_DENIED.into();
        }
        self.send(&HostMessage::State(sent), None).await
    }

    async fn probe(&mut self, now: Instant) -> Result<(), Abandoned> {
        let Some(nonce) = self.session.qos.lock().probe_due(now) else {
            return Ok(());
        };
        // A probe behind an IDR in flight measures that IDR's transfer too.
        let tainted = self.session.credit.lock().idr_inflight().is_some();
        self.session.qos.lock().probe_sent(nonce, now, tainted);
        self.send(&HostMessage::Probe { nonce }, None).await
    }

    /// Write C's newest cursor report, once. It waits for its generation's
    /// `State`, and one of an older generation is stale. C never reports the
    /// controller's own move coming back, so nothing here holds one back. Its
    /// input watermark goes only to a viewer that reads it: an older one
    /// rejects a field it does not know.
    async fn cursor(&mut self) -> Result<(), Abandoned> {
        let Some(report) = *self.cursor.borrow_and_update() else {
            return Ok(());
        };
        let stated = self
            .written
            .as_ref()
            .map_or(0, |written| written.generation);
        let cursor = report.cursor;
        if report.serial <= self.cursor_serial || cursor.generation > stated {
            return Ok(());
        }
        self.cursor_serial = report.serial;
        if cursor.generation < stated {
            return Ok(());
        }
        let message = HostMessage::Cursor {
            generation: cursor.generation,
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible,
            input_sequence: self
                .session
                .cursor_sequence
                .then_some(report.input_sequence),
        };
        self.send(&message, None).await
    }

    /// Write every unit credit admits. Returns whether the head waits for
    /// credit. Units of a generation older than the last `State` are dropped
    /// without a seq, so the seq stays contiguous. A head that ages past
    /// `OUTGOING_MAX_AGE` is replaced by an IDR, but never while one is in
    /// flight: on a link where an IDR takes longer than that to cross, the
    /// P-frames behind it would age out into another IDR, and that one into
    /// the next.
    async fn video(&mut self) -> Result<bool, Abandoned> {
        let shared = self.session.shared.clone();
        loop {
            let Some(head) = shared.outgoing.peek_meta() else {
                return Ok(false);
            };
            // C publishes a unit's state before the unit can be taken, which
            // may be after this pass wrote its own. That state goes first: a
            // unit behind an older `State`, one that still says connecting,
            // is no IDR the viewer waits for.
            if self.state.has_changed().unwrap_or(false) {
                self.write_engine().await?;
                continue;
            }
            let now = Instant::now();
            let stated = self
                .written
                .as_ref()
                .map_or(0, |written| written.generation);
            if head.generation < stated {
                if shared.outgoing.pop_if(head.frame_id).is_some() {
                    self.counters.dropped += 1;
                }
                continue;
            }
            let aging = self.session.credit.lock().aging_since(head.ready_at);
            if aging.is_some_and(|since| now.saturating_duration_since(since) > OUTGOING_MAX_AGE) {
                self.counters.dropped += shared.outgoing.clear() as u64;
                shared.idr.lock().request(IdrReason::HostResync);
                shared.wake.notify();
                continue;
            }
            if head.generation > stated {
                // C has yet to publish this generation's state; that change
                // wakes B-send again.
                return Ok(false);
            }
            // Every JPEG picture is independent; only an H.264 or H.265 IDR
            // uses the IDR headroom and taints probes.
            let idr = head.keyframe && self.session.codec != VideoCodec::Jpeg;
            if !self.session.admissible(head.len, idr, now) {
                return Ok(true);
            }
            let Some(unit) = shared.outgoing.pop_if(head.frame_id) else {
                continue;
            };
            if unit.codec != self.session.codec || !(1..=MAX_PAYLOAD).contains(&unit.bytes.len()) {
                self.session.end(
                    StopReason::CaptureFailed,
                    format!(
                        "the capture backend produced an unusable {:?} picture of {} bytes",
                        unit.codec,
                        unit.bytes.len()
                    ),
                );
                return Ok(false);
            }
            let now = Instant::now();
            let seq = self.session.credit.lock().book(unit.bytes.len(), idr, now);
            let waited = now.saturating_duration_since(unit.ready_at).as_micros();
            let video = HostMessage::Video {
                seq,
                generation: unit.generation,
                frame_id: unit.frame_id,
                keyframe: unit.keyframe,
                duplicate: unit.duplicate,
                width: unit.width,
                height: unit.height,
                pts_us: unit.pts_us,
                host_us: unit
                    .encode_us
                    .saturating_add(u32::try_from(waited).unwrap_or(u32::MAX)),
            };
            self.send(&video, Some(&unit.bytes)).await?;
            let len = unit.bytes.len() as u64;
            self.counters.frames += 1;
            self.counters.bytes += len;
            self.counters.interval_bytes += len;
            let epoch = shared.epoch;
            tracing::trace!(
                target: "pb_mapper_desktop::frames",
                event = "desktop_frame",
                seq,
                frame_id = unit.frame_id,
                keyframe = unit.keyframe,
                bytes = len,
                ready_us = micros(unit.ready_at.saturating_duration_since(epoch)),
                written_us = micros(Instant::now().saturating_duration_since(epoch)),
            );
        }
    }

    /// `Stats`, which is also the host's liveness, and the 1 Hz trace.
    async fn stats(&mut self, engine: &EngineState, now: Instant) -> Result<(), Abandoned> {
        let session = self.session;
        let qos = session.qos.lock().view(now);
        let (credit, ack_queue, window) = {
            let credit = session.credit.lock();
            (
                credit.view(),
                qos.base_rtt
                    .map_or(Duration::ZERO, |base| credit.ack_queue(base, now)),
                credit.window(
                    session.codec,
                    qos.pace_fps,
                    qos.expected_bps,
                    qos.base_rtt,
                    now,
                ),
            )
        };
        let produced = session.shared.outgoing.produced();
        let interval = now
            .saturating_duration_since(self.stats_since)
            .as_secs_f64();
        let sent_bitrate = if interval > 0.0 {
            (self.counters.interval_bytes as f64 * 8.0 / interval) as u32
        } else {
            0
        };
        self.stats_since = now;
        self.counters.interval_bytes = 0;
        let ms = |duration: Duration| u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
        let stats = HostMessage::Stats {
            capture_fps: engine.capture_fps,
            pace_fps: qos.pace_fps,
            target_fps: qos.fps,
            fps_cap: qos.viewer_cap.unwrap_or(0),
            bitrate: u32::try_from(qos.expected_bps).unwrap_or(u32::MAX),
            encoder_bitrate: qos.encoder_bps,
            sent_bitrate,
            rtt_ms: qos.rtt.map(ms),
            base_rtt_ms: qos.base_rtt.map(ms),
            queue_delay_ms: qos.delay_ms,
            ack_queue_ms: ms(ack_queue),
            encoder_dwell_ms: engine
                .encoder_dwell
                .filter(|_| session.encoder_dwell)
                .map(ms),
            dropped_frames: engine.dropped_frames + self.counters.dropped,
            skipped_ticks: engine.skipped_ticks,
            keyframes: produced.keyframes,
            frames: self.counters.frames,
            bytes: self.counters.bytes,
            adaptation: qos.adaptation,
            rate_model: qos.rate_model,
            gop: engine.gop,
        };
        tracing::debug!(
            target: "pb_mapper_desktop::feedback",
            event = "desktop_feedback_interval",
            action = ?qos.adaptation,
            fps = qos.fps,
            pace_fps = qos.pace_fps,
            viewer_cap = qos.viewer_cap,
            paused = qos.paused,
            unresponsive = qos.unresponsive,
            bpf = qos.bpf,
            bitrate = qos.expected_bps,
            encoder_bitrate = qos.encoder_bps,
            sent_bitrate,
            rate_model = ?qos.rate_model,
            rtt_ms = qos.rtt.map(ms),
            baseline_ms = qos.base_rtt.map(ms),
            delay_ms = qos.delay_ms,
            tainted = qos.tainted,
            ack_queue_ms = ms(ack_queue),
            window_frames = window.frames,
            window_bytes = window.bytes,
            window_clamped = window.clamped,
            outstanding_frames = credit.outstanding_frames,
            outstanding_bytes = credit.outstanding_bytes,
            idr_inflight = credit.idr_inflight,
            skipped_ticks = engine.skipped_ticks,
            encoder_dwell_ms = engine.encoder_dwell.map(ms),
            keyframes = produced.keyframes,
            gop = engine.gop,
            input_wait_p50_us = engine.input_wait.p50_us,
            input_wait_p99_us = engine.input_wait.p99_us,
            input_wait_max_us = engine.input_wait.max_us,
        );
        self.send(&stats, None).await
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        Display,
        outgoing::HostUnit,
        protocol::{SessionState, VideoSize},
        shared::CursorUpdate,
    };
    use tokio::io::{AsyncReadExt, DuplexStream, ReadHalf, WriteHalf};

    fn streaming(generation: u64) -> EngineState {
        let mut engine = EngineState::connecting(true);
        engine.record = StateRecord {
            state: SessionState::Streaming,
            generation,
            display: Some(Display {
                id: 1,
                name: "test".into(),
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            }),
            video: Some(VideoSize {
                width: 64,
                height: 64,
            }),
            ..engine.record
        };
        engine.encoder_dwell = Some(Duration::from_millis(3));
        engine
    }

    fn unit(generation: u64, frame_id: u64, keyframe: bool) -> HostUnit {
        HostUnit {
            generation,
            frame_id,
            codec: VideoCodec::H264,
            width: 64,
            height: 64,
            keyframe,
            duplicate: false,
            pts_us: frame_id * 16_667,
            encode_us: 0,
            ready_at: Instant::now(),
            bytes: bytes::Bytes::from(vec![frame_id as u8; 100]),
        }
    }

    /// A scripted viewer over an in-memory stream.
    struct Viewer {
        read: ReadHalf<DuplexStream>,
        write: WriteHalf<DuplexStream>,
    }
    impl Viewer {
        async fn send(&mut self, message: ViewerMessage) {
            wire::write_message(&mut self.write, &message, None)
                .await
                .unwrap();
        }

        /// The next message other than a probe or stats, with its payload.
        async fn next(&mut self) -> (HostMessage, Option<bytes::Bytes>) {
            loop {
                let message = wire::read_header(&mut self.read, Duration::from_secs(5))
                    .await
                    .unwrap()
                    .unwrap();
                match message {
                    HostMessage::Probe { .. } | HostMessage::Stats { .. } => {}
                    HostMessage::Video { .. } => {
                        let payload = wire::read_payload(&mut self.read).await.unwrap();
                        return (message, Some(payload));
                    }
                    message => return (message, None),
                }
            }
        }

        /// The next `Stats`, skipping everything else.
        async fn stats(&mut self) -> HostMessage {
            loop {
                let message = wire::read_header(&mut self.read, Duration::from_secs(5))
                    .await
                    .unwrap()
                    .unwrap();
                match message {
                    HostMessage::Stats { .. } => return message,
                    HostMessage::Video { .. } => {
                        wire::read_payload(&mut self.read).await.unwrap();
                    }
                    _ => {}
                }
            }
        }
    }

    fn video(message: &HostMessage) -> (u64, u64, u64) {
        match message {
            HostMessage::Video {
                seq,
                generation,
                frame_id,
                ..
            } => (*seq, *generation, *frame_id),
            other => panic!("expected video, got {other:?}"),
        }
    }

    fn state_generation(message: &HostMessage) -> u64 {
        match message {
            HostMessage::State(record) => record.generation,
            other => panic!("expected state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stale_generation_units_are_dropped_before_admission() {
        let (viewer, host) = tokio::io::duplex(1 << 20);
        let (read, write) = tokio::io::split(viewer);
        let mut viewer = Viewer { read, write };
        let session = Session::new(
            Arc::new(Policy::new(true)),
            CancellationToken::new(),
            VideoCodec::H264,
            VideoLimits::for_viewer(60.0),
            QualityPreset::Balanced,
            &[],
            false,
        );
        let shared = session.shared.clone();
        shared.state.send_replace(streaming(1));
        let cancel = shared.cancel.clone();
        let running = tokio::spawn(run(session.clone(), host, async move {
            cancel.cancelled().await;
            WorkerExit::Cancelled
        }));

        assert_eq!(state_generation(&viewer.next().await.0), 1);
        shared.outgoing.push(unit(1, 1, true));
        let (first, payload) = viewer.next().await;
        assert_eq!(video(&first), (1, 1, 1));
        assert_eq!(payload.unwrap()[..], [1; 100]);

        // A control grant invalidates generation 1 while two of its units
        // wait in the hand-off: they are dropped without a seq. The new
        // generation's cursor waits for its `State`.
        let report = |serial| {
            Some(CursorReport {
                serial,
                cursor: CursorUpdate {
                    generation: 2,
                    x: 5,
                    y: 6,
                    visible: true,
                },
                input_sequence: 0,
            })
        };
        let cursor = |message: &HostMessage| {
            matches!(
                message,
                HostMessage::Cursor {
                    generation: 2,
                    x: 5,
                    y: 6,
                    visible: true,
                    ..
                }
            )
        };
        shared.outgoing.push(unit(1, 2, false));
        shared.outgoing.push(unit(1, 3, false));
        shared.cursor.send_replace(report(1));
        shared.state.send_replace(streaming(2));
        shared.outgoing.push(unit(2, 4, true));
        assert_eq!(state_generation(&viewer.next().await.0), 2);
        assert!(cursor(&viewer.next().await.0));
        assert_eq!(video(&viewer.next().await.0), (2, 2, 4));

        // The session continues, and the seq stays contiguous. Each report
        // goes out once, even at a position written before.
        viewer.send(ViewerMessage::Received { seq: 2 }).await;
        shared.cursor.send_replace(report(2));
        shared.outgoing.push(unit(2, 5, false));
        assert!(cursor(&viewer.next().await.0));
        assert_eq!(video(&viewer.next().await.0), (3, 2, 5));
        let HostMessage::Stats {
            dropped_frames,
            frames,
            encoder_dwell_ms,
            ..
        } = viewer.stats().await
        else {
            unreachable!()
        };
        assert_eq!((dropped_frames, frames), (2, 3));
        // A viewer that did not list `encoder_dwell` would reject the field.
        assert_eq!(encoder_dwell_ms, None);

        // A receipt for video never sent is a protocol error, and the viewer
        // hears so before the close.
        viewer.send(ViewerMessage::Received { seq: 9 }).await;
        let (stopped, _) = viewer.next().await;
        assert!(
            matches!(
                stopped,
                HostMessage::Stopped {
                    reason: StopReason::ProtocolError,
                    ..
                }
            ),
            "{stopped:?}"
        );
        let mut rest = vec![];
        viewer.read.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        drop(viewer);
        running.await.unwrap();
    }

    /// C publishes `streaming` and its first unit while B-send is in the
    /// middle of a pass that already wrote `connecting`. The unit still
    /// follows its own `State`: a viewer that saw it behind `connecting`
    /// would wait for another IDR, its decoder recovering.
    #[tokio::test]
    async fn a_unit_follows_the_state_it_was_published_with() {
        // A stream this small makes every write wait for the viewer's read,
        // so the pass stops in its cursor, between its `State` and its video.
        let (viewer, host) = tokio::io::duplex(16);
        let (read, write) = tokio::io::split(viewer);
        let mut viewer = Viewer { read, write };
        let session = Session::new(
            Arc::new(Policy::new(true)),
            CancellationToken::new(),
            VideoCodec::H264,
            VideoLimits::for_viewer(60.0),
            QualityPreset::Balanced,
            &[],
            false,
        );
        let shared = session.shared.clone();
        shared.cursor.send_replace(Some(CursorReport {
            serial: 1,
            cursor: CursorUpdate {
                generation: 1,
                x: 5,
                y: 6,
                visible: true,
            },
            input_sequence: 0,
        }));
        let cancel = shared.cancel.clone();
        let running = tokio::spawn(run(session.clone(), host, async move {
            cancel.cancelled().await;
            WorkerExit::Cancelled
        }));
        let state = |message: HostMessage| match message {
            HostMessage::State(record) => record.state,
            other => panic!("expected state, got {other:?}"),
        };

        assert_eq!(state(viewer.next().await.0), SessionState::Connecting);
        shared.state.send_replace(streaming(1));
        shared.outgoing.push(unit(1, 1, true));
        assert!(matches!(
            viewer.next().await.0,
            HostMessage::Cursor { generation: 1, .. }
        ));
        assert_eq!(state(viewer.next().await.0), SessionState::Streaming);
        assert_eq!(video(&viewer.next().await.0), (1, 1, 1));

        shared.cancel.cancel();
        drop(viewer);
        running.await.unwrap();
    }

    /// A cursor report carries the input watermark only to a viewer that
    /// listed `cursor_sequence`: an older one rejects a field it does not know.
    #[tokio::test]
    async fn the_cursor_watermark_goes_only_to_a_viewer_that_reads_it() {
        for listed in [false, true] {
            let (viewer, host) = tokio::io::duplex(1 << 20);
            let (read, write) = tokio::io::split(viewer);
            let mut viewer = Viewer { read, write };
            let capabilities = if listed {
                vec![CAPABILITY_CURSOR_SEQUENCE.to_owned()]
            } else {
                vec![]
            };
            let session = Session::new(
                Arc::new(Policy::new(true)),
                CancellationToken::new(),
                VideoCodec::H264,
                VideoLimits::for_viewer(60.0),
                QualityPreset::Balanced,
                &capabilities,
                false,
            );
            let shared = session.shared.clone();
            shared.state.send_replace(streaming(1));
            shared.cursor.send_replace(Some(CursorReport {
                serial: 1,
                cursor: CursorUpdate {
                    generation: 1,
                    x: 5,
                    y: 6,
                    visible: true,
                },
                input_sequence: 9,
            }));
            let cancel = shared.cancel.clone();
            let running = tokio::spawn(run(session, host, async move {
                cancel.cancelled().await;
                WorkerExit::Cancelled
            }));
            assert_eq!(state_generation(&viewer.next().await.0), 1);
            let (cursor, _) = viewer.next().await;
            assert!(
                matches!(
                    cursor,
                    HostMessage::Cursor { input_sequence, .. }
                        if input_sequence == listed.then_some(9)
                ),
                "{cursor:?}"
            );
            drop(viewer);
            running.await.unwrap();
        }
    }

    /// A viewer that did not list `display_mode` gets a `State` a stage6
    /// viewer's strict record parses (no modes; a display still changing is
    /// the capture failure it is) and never the display mode notice; a
    /// request it sends anyway breaks the protocol. One that listed it gets
    /// all of it, and its request reaches thread C.
    #[tokio::test]
    async fn display_modes_reach_only_a_viewer_that_listed_them() {
        /// What a stage6 viewer accepted as a `State`, as strictly.
        #[derive(Debug, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Stage6Record {
            t: String,
            state: SessionState,
            #[serde(default)]
            reason: Option<Stage6Reason>,
            message: String,
            control_allowed: bool,
            controlling: bool,
            generation: u64,
            #[serde(default)]
            display: Option<Display>,
            desktop: crate::DesktopKind,
            #[serde(default)]
            video: Option<VideoSize>,
        }
        #[derive(Debug, PartialEq, serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Stage6Reason {
            SecureDesktop,
            HelperUnavailable,
            CaptureFailed,
            InputFailed,
            ControlDenied,
            InputCongestion,
            StalePresentation,
            DisplayLost,
        }
        let mode = |width, height| Mode { width, height };
        for listed in [false, true] {
            let (viewer, host) = tokio::io::duplex(1 << 20);
            let (mut read, mut write) = tokio::io::split(viewer);
            let capabilities = if listed {
                vec![CAPABILITY_DISPLAY_MODE.to_owned()]
            } else {
                vec![]
            };
            let session = Session::new(
                Arc::new(Policy::new(true)),
                CancellationToken::new(),
                VideoCodec::H264,
                VideoLimits::for_viewer(60.0),
                QualityPreset::Balanced,
                &capabilities,
                true,
            );
            let shared = session.shared.clone();
            let mut engine = streaming(1);
            engine.record.state = SessionState::CaptureUnavailable;
            engine.record.reason = Some(StateReason::DisplayChanging);
            engine.record.modes = Some(crate::ModeList {
                current: mode(1920, 1080),
                original: mode(2560, 1440),
                supported: vec![mode(2560, 1440), mode(1920, 1080)],
            });
            engine.notice = Some((NoticeCode::DisplayModeFailed, "still changing".into(), 1));
            shared.state.send_replace(engine);
            let cancel = shared.cancel.clone();
            let running = tokio::spawn(run(session, host, async move {
                cancel.cancelled().await;
                WorkerExit::Cancelled
            }));
            // Everything up to the first `Stats`, as JSON.
            let mut seen = Vec::new();
            loop {
                let message: serde_json::Value =
                    wire::read_header(&mut read, Duration::from_secs(5))
                        .await
                        .unwrap()
                        .unwrap();
                if message["t"] == "stats" {
                    break;
                }
                seen.push(message);
            }
            let state = seen.iter().find(|m| m["t"] == "state").unwrap();
            let notice = seen.iter().any(|m| m["t"] == "notice");
            if listed {
                assert_eq!(state["reason"], "display_changing");
                assert_eq!(state["modes"]["original"]["width"], 2560);
                assert!(notice);
            } else {
                let stage6: Stage6Record = serde_json::from_value(state.clone()).unwrap();
                assert_eq!(stage6.reason, Some(Stage6Reason::CaptureFailed));
                assert!(!notice);
            }

            let request = ViewerMessage::DisplayMode {
                width: 1920,
                height: 1080,
            };
            wire::write_message(&mut write, &request, None)
                .await
                .unwrap();
            if listed {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !shared.lane.drain().iter().any(|(command, _)| {
                        matches!(
                            command,
                            LaneCommand::DisplayMode(ModeCommand::Set(set)) if *set == mode(1920, 1080)
                        )
                    }) {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                shared.cancel.cancel();
            } else {
                let stopped = loop {
                    let message: HostMessage = wire::read_header(&mut read, Duration::from_secs(5))
                        .await
                        .unwrap()
                        .unwrap();
                    if let HostMessage::Stopped { reason, .. } = message {
                        break reason;
                    }
                    if let HostMessage::Video { .. } = message {
                        wire::read_payload(&mut read).await.unwrap();
                    }
                };
                assert_eq!(stopped, StopReason::ProtocolError);
            }
            drop((read, write));
            running.await.unwrap();
        }
    }

    /// A viewer still writing when the host stops it hears why. Input that
    /// reaches a closed socket resets the connection, and the reset discards
    /// a `Stopped` the viewer has not read yet; so the host reads on until the
    /// viewer closes.
    #[tokio::test]
    async fn stopped_survives_input_sent_after_the_stop() {
        use tokio::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut viewer = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (host, _) = listener.accept().await.unwrap();
        let session = Session::new(
            Arc::new(Policy::new(false)),
            CancellationToken::new(),
            VideoCodec::Jpeg,
            VideoLimits::for_viewer(60.0),
            QualityPreset::Balanced,
            &[],
            false,
        );
        let cancel = session.shared.cancel.clone();
        let running = tokio::spawn(run(session.clone(), host, async move {
            cancel.cancelled().await;
            WorkerExit::Cancelled
        }));
        session.end(StopReason::Revoked, stop_message(StopReason::Revoked));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = wire::write_message(&mut viewer, &ViewerMessage::Ping {}, None).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        loop {
            match wire::read_header(&mut viewer, Duration::from_secs(1)).await {
                Ok(Some(HostMessage::Stopped { reason, .. })) => {
                    assert_eq!(reason, StopReason::Revoked);
                    break;
                }
                Ok(Some(_)) => {}
                other => panic!("closed without Stopped: {other:?}"),
            }
        }
        drop(viewer);
        running.await.unwrap();
    }
}
