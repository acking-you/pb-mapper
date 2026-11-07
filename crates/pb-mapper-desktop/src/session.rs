//! The network side of one host session, over the TLS stream split in two.
//! B-recv reads the viewer and never waits for thread C: it books receipts,
//! feeds the QoS controller, validates presentations and queues input on the
//! lane. B-send writes everything else: state, notices, stats, probes, the
//! cursor and the video that send credit admits. A supervisor ends both, and
//! B-send writes the final `Stopped` when the session ends for a reason the
//! viewer should hear.
use crate::{
    Display, Input, QualityPreset, VideoCodec, VideoLimits,
    credit::CreditLedger,
    idr::IdrReason,
    lane::LaneCommand,
    outgoing::OUTGOING_MAX_AGE,
    protocol::{
        DesktopKind, HostMessage, SessionState, StateReason, StopReason, VideoSize, ViewerMessage,
    },
    qos::{QOS_TICK, Qos, TargetValues},
    shared::{CursorUpdate, EngineState, Policy, SessionShared, Targets},
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
/// After a session is cancelled, the message in flight and the final
/// `Stopped` share this.
pub(crate) const STOP_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// The controller's own pointer input is not echoed back within this.
pub(crate) const CURSOR_ECHO_SUPPRESS: Duration = Duration::from_millis(300);
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
    pub fn new(
        policy: Arc<Policy>,
        cancel: CancellationToken,
        codec: VideoCodec,
        limits: VideoLimits,
        quality: QualityPreset,
    ) -> Arc<Self> {
        let now = Instant::now();
        let qos = Qos::new(limits, codec, quality, now);
        let targets = qos.targets(now);
        Arc::new(Self {
            shared: Arc::new(SessionShared::new(
                policy,
                cancel,
                Targets::new(limits.max_fps, targets.pace_fps, targets.encoder_bps),
            )),
            codec,
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
                self.update_qos(now, |qos| qos.on_probe_echo(nonce, now));
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
                    if matches!(
                        event.input,
                        Input::Move { .. } | Input::Button { .. } | Input::Wheel { .. }
                    ) {
                        let since = now.saturating_duration_since(shared.epoch).as_micros();
                        shared.pointer_input_at_us.store(
                            u64::try_from(since).unwrap_or(u64::MAX).max(1),
                            Ordering::Release,
                        );
                    }
                    shared.lane.push(LaneCommand::Input(event));
                }
            }
            ViewerMessage::Ping {} => {}
        }
        Ok(())
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
    let (reader, writer) = tokio::io::split(stream);
    let receive = receive(&session, reader);
    let send = Sender::new(&session, writer).run();
    tokio::pin!(receive, send, worker);
    let mut sent = false;
    let mut exited = false;
    tokio::select! {
        violation = &mut receive => match violation {
            Some(violation) => session.end(StopReason::ProtocolError, violation),
            None => session.close(),
        },
        () = &mut send => sent = true,
        exit = &mut worker => {
            exited = true;
            match exit {
                WorkerExit::CaptureFailed(failure) => session.end(StopReason::CaptureFailed, failure),
                WorkerExit::Cancelled => session.close(),
            }
        }
    }
    // B-send ended on its own only when the transport failed.
    session.close();
    if !sent {
        send.await;
    }
    if !exited {
        worker.await;
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

/// Holds back the echo of the controller's own pointer input: the viewer
/// already draws its local pointer, and a late echo would drag it back.
#[derive(Default)]
pub(crate) struct CursorGate {
    last_sent: Option<CursorUpdate>,
}
impl CursorGate {
    /// The cursor to write now, if any, and when to look again at one held
    /// back. A cursor waits for its generation's `State`; a generation change
    /// is never held back; the newest held value goes out when the hold ends.
    pub fn poll(
        &mut self,
        latest: Option<CursorUpdate>,
        stated_generation: u64,
        controlling: bool,
        pointer_input_at: Option<Instant>,
        now: Instant,
    ) -> (Option<CursorUpdate>, Option<Instant>) {
        let Some(cursor) = latest else {
            return (None, None);
        };
        if cursor.generation != stated_generation || self.last_sent == Some(cursor) {
            return (None, None);
        }
        let same_generation = self
            .last_sent
            .is_some_and(|sent| sent.generation == cursor.generation);
        if same_generation
            && controlling
            && let Some(input) = pointer_input_at
        {
            let until = input + CURSOR_ECHO_SUPPRESS;
            if now < until {
                return (None, Some(until));
            }
        }
        self.last_sent = Some(cursor);
        (Some(cursor), None)
    }
}

/// The part of the engine state a `State` message carries.
#[derive(Clone, PartialEq)]
struct StateView {
    state: SessionState,
    reason: Option<StateReason>,
    message: String,
    control_allowed: bool,
    controlling: bool,
    generation: u64,
    display: Option<Display>,
    desktop: DesktopKind,
    video: Option<VideoSize>,
}
impl StateView {
    fn of(engine: &EngineState) -> Self {
        Self {
            state: engine.state,
            reason: engine.reason,
            message: engine.message.clone(),
            control_allowed: engine.control_allowed,
            controlling: engine.controlling,
            generation: engine.generation,
            display: engine.display.clone(),
            desktop: engine.desktop,
            video: engine.video,
        }
    }

    /// The message; a refused control request shows as its reason until the
    /// engine state changes.
    fn message(&self, denied: bool) -> HostMessage {
        let (reason, message) = if denied {
            (Some(StateReason::ControlDenied), CONTROL_DENIED.into())
        } else {
            (self.reason, self.message.clone())
        };
        HostMessage::State {
            state: self.state,
            reason,
            message,
            control_allowed: self.control_allowed,
            controlling: self.controlling,
            generation: self.generation,
            display: self.display.clone(),
            desktop: self.desktop,
            video: self.video,
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
    cursor: watch::Receiver<Option<CursorUpdate>>,
    /// The engine state last written as `State`.
    written: Option<StateView>,
    notice_serial: u64,
    gate: CursorGate,
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
            gate: CursorGate::default(),
            next_tick: now + QOS_TICK,
            next_stats: now + STATS_INTERVAL,
            stats_since: now,
            counters: Counters::default(),
            stop_at: None,
        }
    }

    async fn run(mut self) {
        // The first probe goes out before anything is captured, so the
        // baseline starts clean.
        if self.probe(Instant::now()).await.is_err() {
            return;
        }
        while !self.session.shared.cancel.is_cancelled() {
            match self.pass().await {
                Ok(wake_at) => self.wait(wake_at).await,
                Err(Abandoned) => return,
            }
        }
        self.stop().await;
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
    /// budget.
    async fn stop(mut self) {
        let Some((reason, message)) = self.session.outcome() else {
            return;
        };
        let deadline = *self
            .stop_at
            .get_or_insert_with(|| Instant::now() + STOP_WRITE_TIMEOUT);
        let stopped = HostMessage::Stopped { reason, message };
        let _ = tokio::time::timeout_at(deadline.into(), async {
            wire::write_message(&mut self.writer, &stopped, None).await?;
            self.writer.shutdown().await
        })
        .await;
    }

    /// One pass. Returns when to look again if nothing wakes B-send first.
    async fn pass(&mut self) -> Result<Instant, Abandoned> {
        let now = Instant::now();
        let engine = self.state.borrow_and_update().clone();
        self.write_state(&engine).await?;
        if let Some((code, message, serial)) = &engine.notice
            && *serial > self.notice_serial
        {
            self.notice_serial = *serial;
            let notice = HostMessage::Notice {
                code: *code,
                message: message.clone(),
            };
            self.send(&notice, None).await?;
        }
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
            if let Some(recheck) = self.cursor(&engine, now).await? {
                wake_at = wake_at.min(recheck);
            }
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

    /// Write `State` when its visible part changed, or to report a refused
    /// control request.
    async fn write_state(&mut self, engine: &EngineState) -> Result<(), Abandoned> {
        let view = StateView::of(engine);
        let denied = self.session.control_denied.swap(false, Ordering::AcqRel);
        if !denied && self.written.as_ref() == Some(&view) {
            return Ok(());
        }
        if let Some(video) = view.video
            && self.written.as_ref().and_then(|written| written.video) != Some(video)
        {
            self.session.update_qos(Instant::now(), |qos| {
                qos.set_video_size(video.width, video.height);
            });
        }
        let state = view.message(denied);
        self.written = Some(view);
        self.send(&state, None).await
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

    async fn cursor(
        &mut self,
        engine: &EngineState,
        now: Instant,
    ) -> Result<Option<Instant>, Abandoned> {
        let latest = *self.cursor.borrow_and_update();
        let shared = &self.session.shared;
        let input_us = shared.pointer_input_at_us.load(Ordering::Acquire);
        let input_at = (input_us > 0).then(|| shared.epoch + Duration::from_micros(input_us));
        let stated = self
            .written
            .as_ref()
            .map_or(0, |written| written.generation);
        let (cursor, recheck) = self
            .gate
            .poll(latest, stated, engine.controlling, input_at, now);
        if let Some(cursor) = cursor {
            let message = HostMessage::Cursor {
                generation: cursor.generation,
                x: cursor.x,
                y: cursor.y,
                visible: cursor.visible,
            };
            self.send(&message, None).await?;
        }
        Ok(recheck)
    }

    /// Write every unit credit admits. Returns whether the head waits for
    /// credit. Units of a generation older than the last `State` are dropped
    /// without a seq, so the seq stays contiguous.
    async fn video(&mut self) -> Result<bool, Abandoned> {
        let shared = self.session.shared.clone();
        loop {
            let Some(head) = shared.outgoing.peek_meta() else {
                return Ok(false);
            };
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
            if now.saturating_duration_since(head.ready_at) > OUTGOING_MAX_AGE {
                self.counters.dropped += shared.outgoing.clear() as u64;
                shared.idr.lock().request(IdrReason::HostResync);
                shared.wake.notify();
                continue;
            }
            if head.generation > stated {
                let engine = self.state.borrow_and_update().clone();
                if engine.generation < head.generation {
                    // C publishes the new generation's state after the unit;
                    // that change wakes B-send again.
                    return Ok(false);
                }
                self.write_state(&engine).await?;
                continue;
            }
            // Every JPEG picture is independent; only an H.264 IDR uses the
            // IDR headroom and taints probes.
            let idr = head.keyframe && self.session.codec == VideoCodec::H264;
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
    use crate::outgoing::HostUnit;
    use tokio::io::{AsyncReadExt, DuplexStream, ReadHalf, WriteHalf};

    #[test]
    fn cursor_is_not_echoed_within_300ms_but_generation_changes_pass() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let at = |generation, x| {
            Some(CursorUpdate {
                generation,
                x,
                y: 0,
                visible: true,
            })
        };
        let mut gate = CursorGate::default();
        assert_eq!(gate.poll(at(1, 1), 1, true, None, t0), (at(1, 1), None));
        // An unchanged cursor is not written again.
        assert_eq!(gate.poll(at(1, 1), 1, true, None, ms(1)), (None, None));

        // The controller moved the pointer at 100 ms: its echo is held for
        // 300 ms, and the newest held value goes out when the hold ends.
        let input = Some(ms(100));
        assert_eq!(
            gate.poll(at(1, 2), 1, true, input, ms(110)),
            (None, Some(ms(400)))
        );
        assert_eq!(
            gate.poll(at(1, 3), 1, true, input, ms(399)),
            (None, Some(ms(400)))
        );
        assert_eq!(
            gate.poll(at(1, 3), 1, true, input, ms(400)),
            (at(1, 3), None)
        );
        // A viewer that is not controlling sees every move at once.
        assert_eq!(
            gate.poll(at(1, 4), 1, false, input, ms(150)),
            (at(1, 4), None)
        );

        // A generation change is never held back, but waits for its `State`.
        assert_eq!(
            gate.poll(at(2, 4), 1, true, Some(ms(500)), ms(510)),
            (None, None)
        );
        assert_eq!(
            gate.poll(at(2, 4), 2, true, Some(ms(500)), ms(510)),
            (at(2, 4), None)
        );
        // A cursor of an older generation is stale.
        assert_eq!(gate.poll(at(1, 5), 2, false, None, ms(600)), (None, None));
    }

    fn streaming(generation: u64) -> EngineState {
        EngineState {
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
            ..EngineState::connecting(true)
        }
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
            HostMessage::State { generation, .. } => *generation,
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
        // wait in the hand-off: they are dropped without a seq.
        shared.outgoing.push(unit(1, 2, false));
        shared.outgoing.push(unit(1, 3, false));
        shared.state.send_replace(streaming(2));
        shared.outgoing.push(unit(2, 4, true));
        assert_eq!(state_generation(&viewer.next().await.0), 2);
        assert_eq!(video(&viewer.next().await.0), (2, 2, 4));

        // The session continues, and the seq stays contiguous.
        viewer.send(ViewerMessage::Received { seq: 2 }).await;
        shared.outgoing.push(unit(2, 5, false));
        assert_eq!(video(&viewer.next().await.0), (3, 2, 5));
        let HostMessage::Stats {
            dropped_frames,
            frames,
            ..
        } = viewer.stats().await
        else {
            unreachable!()
        };
        assert_eq!((dropped_frames, frames), (2, 3));

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
        running.await.unwrap();
    }
}
