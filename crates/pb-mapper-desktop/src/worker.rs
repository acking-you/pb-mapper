use crate::{
    Backend, Frame, InputEvent, Result, SessionStatus, VideoCodec, VideoLimits, error,
    shared::{EngineState, Policy, SessionShared},
    wire::Command,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

pub(crate) type Factory = Arc<dyn Fn() -> Result<Box<dyn Backend>> + Send + Sync>;
type Request = (Command, oneshot::Sender<Result<()>>);

// The v2 host session spawns thread C through these once it lands; until the
// G1 cleanup removes the allows, nothing else uses them.

/// What a v2 session's thread C is started with.
#[allow(dead_code)]
pub(crate) struct WorkerConfig {
    pub codec: VideoCodec,
    pub limits: VideoLimits,
    /// `limits.max_fps`: the encoder's nominal rate for the whole session.
    pub nominal_fps: u32,
    pub admission: Option<Arc<dyn crate::ControlAdmission>>,
}

/// How thread C ended.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerExit {
    /// `shared.cancel` ended it.
    Cancelled,
    /// The backend failed for good (a lost helper or an unrecoverable
    /// encoder); the session ends with `Stopped{capture_failed}`.
    CaptureFailed(String),
}

/// Thread C of one v2 session. It exits on `shared.cancel` (after a wake) or
/// fatally; `exited` reports which, once it has also dropped its backend.
#[allow(dead_code)]
pub(crate) struct WorkerHandle {
    join: Option<tokio::task::JoinHandle<WorkerExit>>,
    exit: Option<WorkerExit>,
}
#[allow(dead_code)]
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

/// Start thread C for a v2 session. Until the capture worker lands it only
/// publishes `connecting` and waits for the session to be cancelled.
#[allow(dead_code)]
pub(crate) fn spawn_session(
    shared: Arc<SessionShared>,
    _factory: Factory,
    _config: WorkerConfig,
) -> WorkerHandle {
    let join = tokio::spawn(async move {
        shared
            .state
            .send_replace(EngineState::connecting(shared.policy.allowed()));
        shared.cancel.cancelled().await;
        WorkerExit::Cancelled
    });
    WorkerHandle {
        join: Some(join),
        exit: None,
    }
}

pub(crate) struct Worker {
    requests: mpsc::SyncSender<Request>,
    pub frames: watch::Receiver<Option<Frame>>,
    pub cursor: watch::Receiver<Option<SessionStatus>>,
    pub media: Arc<crate::media::Queue>,
    pub feedback: Option<Arc<crate::congestion::Feedback>>,
    bitrate: Arc<AtomicU32>,
    cancel: CancellationToken,
    wake: crate::scheduler::Wake,
    join: Option<tokio::task::JoinHandle<()>>,
}
impl Worker {
    #[cfg(test)]
    pub fn start(
        factory: Factory,
        allowed: Arc<Policy>,
        status: Arc<parking_lot::Mutex<SessionStatus>>,
    ) -> Self {
        Self::start_video(
            factory,
            allowed,
            status,
            crate::VideoCodec::Jpeg,
            crate::VideoPreferences::default(),
            false,
            None,
        )
    }
    pub fn start_video(
        factory: Factory,
        allowed: Arc<Policy>,
        status: Arc<parking_lot::Mutex<SessionStatus>>,
        codec: crate::VideoCodec,
        preferences: crate::VideoPreferences,
        presentation_feedback: bool,
        admission: Option<Arc<dyn crate::ControlAdmission>>,
    ) -> Self {
        let (requests, rx) = mpsc::sync_channel::<Request>(16);
        let (frames_tx, frames) = watch::channel(None);
        let (cursor_tx, cursor) = watch::channel(None);
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let media = Arc::new(crate::media::Queue::default());
        let outgoing = media.clone();
        let feedback =
            presentation_feedback.then(|| Arc::new(crate::congestion::Feedback::new(preferences)));
        let renderer = feedback.clone();
        let bitrate = Arc::new(AtomicU32::new(preferences.bitrate));
        let rate = bitrate.clone();
        let wake = crate::scheduler::Wake::new();
        let signal = wake.clone();
        let join = tokio::task::spawn_blocking(move || {
            let (waiter, backend) =
                match crate::scheduler::Waiter::new(&signal).and_then(|waiter| {
                    preferences.validate()?;
                    let mut backend = factory()?;
                    backend.configure_video(preferences.limits(), preferences.max_fps)?;
                    Ok((waiter, backend))
                }) {
                    Ok(ready) => ready,
                    Err(e) => {
                        let unavailable = SessionStatus {
                            state: "capture_unavailable".into(),
                            message: e.to_string(),
                            generation: 1,
                            ..Default::default()
                        };
                        *status.lock() = unavailable.clone();
                        // Keep reporting the actual capture failure, rather than
                        // converting it into an unexplained EOF at the viewer.
                        while !stop.is_cancelled() {
                            let frame = Frame {
                                status: unavailable.clone(),
                                bytes: Arc::from([]),
                            };
                            outgoing.push(frame.clone());
                            frames_tx.send_replace(Some(frame));
                            match rx.recv_timeout(Duration::from_millis(125)) {
                                Ok((_, reply)) => {
                                    let _ = reply.send(Err(error(&unavailable.message)));
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                            }
                        }
                        status.lock().state = "waiting".into();
                        return;
                    }
                };
            let mut engine = Engine::new(backend);
            engine.codec = codec;
            engine.bitrate = preferences.bitrate;
            engine.pace_fps = preferences.max_fps;
            engine.admission = admission;
            let mut policy = allowed.get();
            let mut next_frame = Instant::now();
            let mut next_cursor = next_frame;
            let mut observed_cursor = None;
            let mut configured = preferences;
            let mut feedback_context = (0, false);
            while !stop.is_cancelled() {
                let current_policy = allowed.get();
                if current_policy != policy {
                    engine.release_control();
                    policy = current_policy;
                }
                engine.state.control_allowed = allowed.allowed();
                engine.release_if_stale();
                let received = waiter.receive(
                    &rx,
                    next_frame
                        .min(next_cursor)
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
                // A stop wake must never execute an input that happened to be
                // queued before cancellation. Release is handled below the loop.
                if stop.is_cancelled() {
                    break;
                }
                match received {
                    Ok(Some((command, reply))) => {
                        let result = match command {
                            Command::Control { enabled } => {
                                engine.control(enabled, allowed.allowed())
                            }
                            Command::Input { event } => {
                                engine.input(event, allowed.allowed() && allowed.get() == policy)
                            }
                            Command::Release => {
                                engine.backend.release();
                                Ok(())
                            }
                            Command::Ping => Ok(()),
                            Command::Keyframe => {
                                if renderer
                                    .as_ref()
                                    .is_none_or(|feedback| feedback.request_keyframe())
                                {
                                    outgoing.resynchronize();
                                    engine.keyframe = true;
                                }
                                Ok(())
                            }
                            Command::Presented {
                                generation,
                                frame_id,
                            } => {
                                if let Some(feedback) = &renderer {
                                    feedback.presented(generation, frame_id);
                                }
                                engine.presentation(generation, frame_id)
                            }
                            Command::Authenticate { .. } | Command::Probe { .. } => {
                                Err(error("unexpected worker command"))
                            }
                        };
                        if let Err(e) = &result {
                            engine.report_command_failure(e);
                        }
                        let _ = reply.send(result);
                    }
                    Err(e) => {
                        status.lock().message = e.to_string();
                        break;
                    }
                    Ok(None) => {}
                }
                if Instant::now() >= next_cursor {
                    engine.sample_cursor();
                    let current = (engine.state.generation, engine.state.cursor);
                    if engine.state.state == "streaming" && observed_cursor != Some(current) {
                        observed_cursor = Some(current);
                        *status.lock() = engine.state.clone();
                        cursor_tx.send_replace(Some(engine.state.clone()));
                    }
                    next_cursor = Instant::now()
                        + if engine.state.state == "capture_unavailable" {
                            Duration::from_millis(200)
                        } else {
                            Duration::from_millis(16)
                        };
                }
                if Instant::now() >= next_frame {
                    let mut paused = false;
                    if let Some(feedback) = &renderer {
                        engine.sync_feedback(feedback, &mut feedback_context);
                        let (preferences, bitrate, waiting) = feedback.budget();
                        engine.bitrate = bitrate;
                        paused = waiting;
                        if configured != preferences {
                            match engine
                                .backend
                                .configure_video(preferences.limits(), preferences.max_fps)
                            {
                                Ok(()) => {
                                    configured = preferences;
                                    engine.pace_fps = preferences.max_fps;
                                    outgoing.resynchronize();
                                    engine.keyframe = true;
                                }
                                Err(e) => {
                                    engine.state.message = e.to_string();
                                    break;
                                }
                            }
                        }
                    } else {
                        engine.bitrate = rate.load(Ordering::Relaxed);
                    }
                    engine.state.bitrate = engine.bitrate;
                    // Flow control pauses new submissions, not completion of
                    // asynchronous encoder work already in flight. Holding MFT
                    // output until the next ACK makes a healthy codec look stale.
                    // Only actual queue loss, stale output or a context change
                    // requires an IDR; restarting every credit window otherwise
                    // spends the available bandwidth repeatedly on keyframes.
                    engine.keyframe |= outgoing.take_keyframe_request();
                    engine.state.dropped_frames = outgoing.dropped();
                    let started = Instant::now();
                    if let Some(frame) = engine.capture_video(!paused) {
                        if let Some(feedback) = &renderer {
                            engine.sync_feedback(feedback, &mut feedback_context);
                            feedback.captured(&frame);
                        }
                        outgoing.push(frame.clone());
                        frames_tx.send_replace(Some(frame));
                    }
                    // Schedule from capture start, not after encoding. No frame
                    // jobs accumulate when the encoder or desktop is slow.
                    let period = if engine.state.state == "capture_unavailable" {
                        Duration::from_millis(200)
                    } else if codec == crate::VideoCodec::H264 {
                        // Asynchronous codecs need output/event polling between
                        // capture deadlines. The encoder owns the negotiated cap.
                        Duration::from_millis(2)
                    } else {
                        Duration::from_millis(125)
                    };
                    next_frame = (started + period).max(Instant::now());
                }
                *status.lock() = engine.state.clone();
            }
            engine.backend.release();
            let mut state = status.lock();
            state.controlling = false;
            state.state = "waiting".into();
        });
        Self {
            requests,
            frames,
            cursor,
            media,
            feedback,
            bitrate,
            cancel,
            wake,
            join: Some(join),
        }
    }
    pub fn sent(&self, bytes: usize, elapsed: Duration) {
        if self.feedback.is_some() {
            return;
        }
        if elapsed > Duration::from_millis(60) && bytes > 0 {
            let capacity = ((bytes as f64 * 8.0 / elapsed.as_secs_f64()) * 0.8) as u32;
            let current = self.bitrate.load(Ordering::Relaxed);
            self.bitrate
                .store(capacity.clamp(1_000_000, current), Ordering::Relaxed);
        }
    }
    pub async fn command(&self, command: Command) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .try_send((command, tx))
            .map_err(|_| error("desktop input worker unavailable"))?;
        self.wake.notify();
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .map_err(error)?
            .map_err(error)?
    }
    pub async fn stop(&mut self) {
        self.cancel.cancel();
        self.wake.notify();
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.wake.notify();
    }
}

struct Engine {
    backend: Box<dyn Backend>,
    state: SessionStatus,
    sequence: u64,
    recent: VecDeque<(u64, Instant)>,
    presented: Option<(u64, u64)>,
    acknowledged: Option<(u64, u64)>,
    context: Option<crate::DesktopContext>,
    codec: crate::VideoCodec,
    keyframe: bool,
    bitrate: u32,
    pace_fps: u32,
    sample_at: Instant,
    sample_frames: u64,
    admission: Option<Arc<dyn crate::ControlAdmission>>,
    lease: Option<Box<dyn crate::ControlLease>>,
    input_failure: Option<String>,
}
impl Engine {
    fn report_command_failure(&mut self, failure: &std::io::Error) {
        // Input failure ends the grant and invalidates already queued releases
        // and acknowledgements. Their secondary errors must not hide why
        // control ended, otherwise a viewer may automatically request it again.
        self.state.message = self
            .input_failure
            .clone()
            .unwrap_or_else(|| failure.to_string());
    }

    fn sync_feedback(&self, feedback: &crate::congestion::Feedback, observed: &mut (u64, bool)) {
        let context = (
            self.state.generation,
            self.state.state == "capture_unavailable",
        );
        if *observed != context {
            feedback.context_changed(context.1);
            *observed = context;
        }
    }
    fn new(backend: Box<dyn Backend>) -> Self {
        Self {
            backend,
            state: SessionStatus {
                state: "connecting".into(),
                generation: 1,
                ..Default::default()
            },
            sequence: 0,
            recent: VecDeque::new(),
            presented: None,
            acknowledged: None,
            context: None,
            codec: crate::VideoCodec::Jpeg,
            keyframe: true,
            bitrate: 16_000_000,
            pace_fps: crate::VideoPreferences::default().max_fps,
            sample_at: Instant::now(),
            sample_frames: 0,
            admission: None,
            lease: None,
            input_failure: None,
        }
    }
    fn invalidate(&mut self) {
        self.backend.release();
        self.state.generation += 1;
        self.recent.clear();
        self.presented = None;
        self.acknowledged = None;
        self.state.cursor = None;
        self.state.frame_ack_ms = 0.0;
        self.keyframe = true;
    }
    fn release_if_stale(&mut self) {
        if self.state.controlling
            && self.presented.is_none_or(|(generation, frame_id)| {
                generation != self.state.generation
                    || !self
                        .recent
                        .iter()
                        .any(|(id, at)| *id == frame_id && at.elapsed() < Duration::from_secs(2))
            })
        {
            self.backend.release();
        }
    }
    fn release_control(&mut self) {
        self.invalidate();
        self.state.controlling = false;
        self.lease = None;
    }
    fn control(&mut self, enabled: bool, allowed: bool) -> Result<()> {
        // Retrying the same request on a delayed link must not reset the
        // desktop generation, decoder or active lease on every UI poll.
        if enabled && allowed && self.state.controlling {
            return Ok(());
        }
        if !enabled && !self.state.controlling {
            self.backend.release();
            return Ok(());
        }
        self.release_control();
        self.input_failure = None;
        if enabled {
            if !allowed {
                return Err(error("control is not authorized at the host"));
            }
            if let Some(admission) = &self.admission {
                self.lease = Some(admission.acquire()?);
            }
            self.state.controlling = true;
        }
        Ok(())
    }
    fn presentation(&mut self, generation: u64, frame_id: u64) -> Result<()> {
        let advances = self.presented.is_none_or(|(old_generation, old_id)| {
            old_generation != generation || frame_id > old_id
        });
        let ready_at = self
            .recent
            .iter()
            .find(|(id, _)| *id == frame_id)
            .map(|(_, at)| *at);
        if generation != self.state.generation
            || ready_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(2))
        {
            self.backend.release();
            return Err(error("stale presentation"));
        }
        if self.acknowledged.is_none_or(|(old_generation, old_id)| {
            old_generation != generation || frame_id > old_id
        }) {
            self.state.frame_ack_ms =
                ready_at.map_or(0.0, |at| at.elapsed().as_secs_f32() * 1000.0);
            self.acknowledged = Some((generation, frame_id));
        }
        self.presented = Some((generation, frame_id));
        if advances
            && self.state.controlling
            && let Some(context) = &self.context
        {
            self.backend.presented(context)?;
        }
        Ok(())
    }
    fn geometry(&mut self) -> Result<()> {
        match self.backend.context() {
            Ok(context) => {
                if self.context.as_ref() != Some(&context)
                    || self.state.state == "capture_unavailable"
                {
                    self.invalidate();
                    self.state.display = Some(context.display.clone());
                    self.state.desktop = context.desktop.clone();
                    self.context = Some(context);
                    self.state.state = "connecting".into();
                }
                Ok(())
            }
            Err(e) => {
                if self.state.state != "capture_unavailable" {
                    self.invalidate();
                }
                self.state.state = "capture_unavailable".into();
                Err(e)
            }
        }
    }
    #[cfg(test)]
    fn capture(&mut self) -> Frame {
        self.capture_video(true)
            .expect("fake capture returns an image")
    }
    fn sample_cursor(&mut self) {
        let result = self.geometry().and_then(|()| {
            let context = self
                .context
                .as_ref()
                .ok_or_else(|| error("missing desktop context"))?;
            self.backend.cursor(context)
        });
        match result {
            Ok(cursor) => self.state.cursor = cursor,
            Err(e) => {
                if self.state.state != "capture_unavailable" {
                    self.invalidate();
                }
                self.state.state = "capture_unavailable".into();
                self.state.message = e.to_string();
            }
        }
    }
    fn capture_video(&mut self, submit: bool) -> Option<Frame> {
        let image = self.geometry().and_then(|()| {
            let context = self
                .context
                .as_ref()
                .ok_or_else(|| error("missing desktop context"))?;
            let frame = if submit {
                let rate = crate::VideoRate {
                    bitrate: self.bitrate,
                    pace_fps: self.pace_fps,
                };
                self.backend
                    .capture_video(context, self.codec, self.keyframe, rate)?
            } else {
                self.backend.drain_video(context, Duration::ZERO)?
            };
            frame
                .map(|frame| self.backend.cursor(context).map(|cursor| (frame, cursor)))
                .transpose()
        });
        let jpeg = match image {
            Ok(None) => return None,
            Ok(Some((image, cursor)))
                if !image.bytes.is_empty() && image.bytes.len() <= crate::wire::MAX_FRAME =>
            {
                if image.format.keyframe {
                    self.keyframe = false;
                }
                self.state.video = image.format;
                self.state.cursor = cursor;
                let jpeg = image.bytes;
                self.state.state = "streaming".into();
                self.state.message = self.input_failure.clone().unwrap_or_default();
                self.state.frame_id += 1;
                self.state.frames += 1;
                self.state.bytes += jpeg.len() as u64;
                self.recent.push_back((self.state.frame_id, Instant::now()));
                while self.recent.len() > 256 {
                    self.recent.pop_front();
                }
                if !self.state.video.duplicate {
                    self.sample_frames += 1;
                }
                let elapsed = self.sample_at.elapsed();
                if elapsed >= Duration::from_secs(1) {
                    self.state.capture_fps = self.sample_frames as f32 / elapsed.as_secs_f32();
                    self.sample_frames = 0;
                    self.sample_at = Instant::now();
                }
                self.state.bitrate = self.bitrate;
                jpeg
            }
            result => {
                if self.state.state != "capture_unavailable" {
                    self.invalidate();
                }
                self.state.state = "capture_unavailable".into();
                self.state.message = result.err().map_or_else(
                    || "frame exceeds the preview limit".into(),
                    |e| e.to_string(),
                );
                Vec::new()
            }
        };
        Some(Frame {
            status: self.state.clone(),
            bytes: jpeg.into(),
        })
    }
    fn input(&mut self, event: InputEvent, allowed: bool) -> Result<()> {
        if event.sequence <= self.sequence {
            return Err(error("duplicate or reordered input"));
        }
        self.sequence = event.sequence;
        self.geometry()?;
        if !allowed || !self.state.controlling {
            return Err(error("view-only session"));
        }
        if self.state.state != "streaming"
            || event.generation != self.state.generation
            || !self
                .recent
                .iter()
                .any(|(id, at)| *id == event.frame_id && at.elapsed() < Duration::from_secs(2))
        {
            self.backend.release();
            return Err(error("screen changed or is stale; wait for a fresh frame"));
        }
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| error("missing desktop context"))?;
        self.presented = Some((event.generation, event.frame_id));
        if let Err(failure) = self.backend.input_in_context(context, &event.input) {
            // A live picture is not proof of working control. End the grant and
            // retain this failure across successful captures until the viewer
            // explicitly requests control again; queued presses are not replayed.
            self.release_control();
            let failure = format!("input_failed: {failure}");
            self.state.message.clone_from(&failure);
            self.input_failure = Some(failure.clone());
            return Err(error(failure));
        }
        Ok(())
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.backend.release();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{Display, Input};
    use std::sync::atomic::AtomicU64;
    struct Fake(Arc<parking_lot::Mutex<(u32, u32, u32, u64)>>);
    impl Backend for Fake {
        fn display(&mut self) -> Result<Display> {
            Ok(Display {
                id: 1,
                name: "test".into(),
                x: 0,
                y: 0,
                width: self.0.lock().0,
                height: 10,
            })
        }
        fn capture(&mut self) -> Result<Vec<u8>> {
            Ok(vec![1])
        }
        fn context(&mut self) -> Result<crate::DesktopContext> {
            Ok(crate::DesktopContext {
                display: self.display()?,
                generation: self.0.lock().3,
                desktop: "test".into(),
            })
        }
        fn input(&mut self, _: &Input) -> Result<()> {
            self.0.lock().1 += 1;
            Ok(())
        }
        fn cursor(
            &mut self,
            expected: &crate::DesktopContext,
        ) -> Result<Option<crate::CursorPosition>> {
            if &self.context()? != expected {
                return Err(error("test desktop changed"));
            }
            Ok(Some(crate::CursorPosition {
                x: 3,
                y: 3,
                visible: true,
            }))
        }
        fn release(&mut self) {
            self.0.lock().2 += 1;
        }
    }
    #[test]
    fn input_requires_control_current_geometry_and_unique_sequence() {
        let counts = Arc::new(parking_lot::Mutex::new((10, 0, 0, 0)));
        let mut engine = Engine::new(Box::new(Fake(counts.clone())));
        let frame = engine.capture();
        assert!(frame.status.cursor.is_some_and(|cursor| cursor.visible));
        let mut event = InputEvent {
            generation: frame.status.generation,
            frame_id: frame.status.frame_id,
            sequence: 1,
            input: Input::Move { x: 3, y: 3 },
        };
        assert!(engine.input(event.clone(), true).is_err());
        engine.state.controlling = true;
        event.sequence = 2;
        engine.input(event.clone(), true).unwrap();
        assert!(engine.input(event.clone(), true).is_err());
        // Input is not a renderer acknowledgment. Only a new valid Presented
        // event may update the frame-ready-to-ACK diagnostic.
        assert_eq!(engine.state.frame_ack_ms, 0.0);
        engine.recent.front_mut().unwrap().1 = Instant::now() - Duration::from_millis(100);
        engine
            .presentation(event.generation, event.frame_id)
            .unwrap();
        let measured = engine.state.frame_ack_ms;
        assert!(measured >= 100.0);
        engine.recent.front_mut().unwrap().1 = Instant::now() - Duration::from_millis(300);
        engine
            .presentation(event.generation, event.frame_id)
            .unwrap();
        assert_eq!(engine.state.frame_ack_ms, measured);
        let releases = counts.lock().2;
        engine.recent.front_mut().unwrap().1 = Instant::now() - Duration::from_secs(3);
        let expired = engine.recent.clone();
        let presented = engine.presented;
        engine.sample_cursor();
        assert_eq!(engine.recent, expired);
        assert_eq!(engine.presented, presented);
        assert_eq!(engine.state.frame_id, frame.status.frame_id);
        assert!(
            engine
                .presentation(event.generation, event.frame_id)
                .is_err()
        );
        engine.release_if_stale();
        assert!(counts.lock().2 > releases);
        // A different desktop at identical geometry must invalidate input too.
        counts.lock().3 += 1;
        event.sequence = 3;
        assert!(engine.input(event.clone(), true).is_err());
        assert_eq!(engine.state.cursor, None);
        assert_eq!(engine.state.frame_ack_ms, 0.0);
        counts.lock().0 = 20;
        event.sequence = 4;
        assert!(engine.input(event.clone(), true).is_err());
        let fresh = engine.capture();
        event.sequence = 5;
        event.generation = fresh.status.generation;
        event.frame_id = fresh.status.frame_id;
        assert!(engine.input(event.clone(), false).is_err());
        assert_eq!(counts.lock().1, 1);
        assert!(counts.lock().2 > 0);
    }

    #[test]
    fn native_input_failure_ends_control_and_survives_new_video() {
        struct FailingInput {
            backend: Fake,
            rejected: Arc<std::sync::atomic::AtomicBool>,
        }
        impl Backend for FailingInput {
            fn display(&mut self) -> Result<Display> {
                self.backend.display()
            }
            fn capture(&mut self) -> Result<Vec<u8>> {
                self.backend.capture()
            }
            fn input(&mut self, event: &Input) -> Result<()> {
                if self.rejected.load(Ordering::SeqCst) {
                    Err(error("native injection rejected"))
                } else {
                    self.backend.input(event)
                }
            }
            fn release(&mut self) {
                self.backend.release();
            }
        }
        let counts = Arc::new(parking_lot::Mutex::new((10, 0, 0, 0)));
        let rejected = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut engine = Engine::new(Box::new(FailingInput {
            backend: Fake(counts.clone()),
            rejected: rejected.clone(),
        }));
        engine.control(true, true).unwrap();
        let frame = engine.capture();
        let mut event = InputEvent {
            generation: frame.status.generation,
            frame_id: frame.status.frame_id,
            sequence: 1,
            input: Input::Button {
                button: crate::Button::Left,
                down: true,
                drag_start: false,
            },
        };
        let failure = engine.input(event.clone(), true).unwrap_err();
        engine.report_command_failure(&failure);
        assert!(!engine.state.controlling);
        assert!(counts.lock().2 > 0);
        let failed = engine.state.message.clone();
        assert!(!failed.is_empty());
        let late_ack = engine
            .presentation(event.generation, event.frame_id)
            .unwrap_err();
        engine.report_command_failure(&late_ack);
        assert_eq!(engine.state.message, failed);
        event.sequence += 1;
        event.input = Input::Button {
            button: crate::Button::Left,
            down: false,
            drag_start: false,
        };
        let queued_release = engine.input(event, true).unwrap_err();
        engine.report_command_failure(&queued_release);
        assert_eq!(engine.state.message, failed);
        assert_eq!(engine.capture().status.message, failed);
        assert_eq!(engine.capture().status.message, failed);
        rejected.store(false, Ordering::SeqCst);
        engine.control(true, true).unwrap();
        let retry = engine.capture();
        assert!(retry.status.message.is_empty());
        engine
            .input(
                InputEvent {
                    generation: retry.status.generation,
                    frame_id: retry.status.frame_id,
                    sequence: 3,
                    input: Input::Button {
                        button: crate::Button::Left,
                        down: false,
                        drag_start: false,
                    },
                },
                true,
            )
            .unwrap();
        assert!(engine.state.controlling);
        assert_eq!(counts.lock().1, 1);
    }

    #[test]
    fn observation_never_acquires_write_admission_and_revocation_releases_it() {
        struct Admission(Arc<AtomicU64>);
        struct Lease(Arc<AtomicU64>);
        impl crate::ControlAdmission for Admission {
            fn acquire(&self) -> Result<Box<dyn crate::ControlLease>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Lease(self.0.clone())))
            }
        }
        impl Drop for Lease {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let writers = Arc::new(AtomicU64::new(0));
        let mut engine = Engine::new(Box::new(Fake(Arc::new(parking_lot::Mutex::new((
            10, 0, 0, 0,
        ))))));
        engine.admission = Some(Arc::new(Admission(writers.clone())));
        engine.capture();
        assert_eq!(writers.load(Ordering::SeqCst), 0);
        assert!(engine.control(true, false).is_err());
        assert_eq!(writers.load(Ordering::SeqCst), 0);
        engine.control(true, true).unwrap();
        assert_eq!(writers.load(Ordering::SeqCst), 1);
        let frame = engine.capture();
        engine
            .presentation(frame.status.generation, frame.status.frame_id)
            .unwrap();
        let presented = engine.presented;
        engine.control(true, true).unwrap();
        assert_eq!(engine.state.generation, frame.status.generation);
        assert_eq!(engine.presented, presented);
        assert_eq!(engine.recent.len(), 1);
        assert_eq!(writers.load(Ordering::SeqCst), 1);
        engine.release_control();
        assert_eq!(writers.load(Ordering::SeqCst), 0);
        let generation = engine.state.generation;
        engine.control(false, true).unwrap();
        assert_eq!(engine.state.generation, generation);
        engine.control(true, true).unwrap();
        drop(engine);
        assert_eq!(writers.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unavailable_backend_reports_failure_and_stops() {
        let mut worker = Worker::start(
            Arc::new(|| Err(error("desktop is locked"))),
            Arc::new(Policy::new(false)),
            Arc::new(parking_lot::Mutex::new(SessionStatus::default())),
        );
        tokio::time::timeout(Duration::from_secs(2), worker.frames.changed())
            .await
            .unwrap()
            .unwrap();
        let frame = worker.frames.borrow_and_update().clone().unwrap();
        assert_eq!(frame.status.state, "capture_unavailable");
        assert!(frame.status.message.contains("locked"));
        assert!(frame.bytes.is_empty());
        assert!(
            worker
                .command(Command::Control { enabled: true })
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(2), worker.stop())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn presentation_backpressure_drains_encoder_without_new_capture() {
        #[derive(Default)]
        struct Pipeline {
            submitted: u32,
            output_ready: bool,
            drained: bool,
        }
        struct Video(Arc<parking_lot::Mutex<Pipeline>>);
        fn encoded(keyframe: bool) -> crate::EncodedFrame {
            crate::EncodedFrame {
                format: crate::VideoFormat {
                    codec: crate::VideoCodec::H264,
                    width: 16,
                    height: 16,
                    keyframe,
                    ..Default::default()
                },
                bytes: vec![1; if keyframe { 128 * 1024 } else { 1024 }],
            }
        }
        impl Backend for Video {
            fn display(&mut self) -> Result<Display> {
                Ok(Display {
                    id: 1,
                    name: "async pipeline".into(),
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 16,
                })
            }
            fn capture(&mut self) -> Result<Vec<u8>> {
                unreachable!()
            }
            fn capture_video(
                &mut self,
                _: &crate::DesktopContext,
                _: crate::VideoCodec,
                _: bool,
                _: crate::VideoRate,
            ) -> Result<Option<crate::EncodedFrame>> {
                let mut state = self.0.lock();
                // The first completed IDR has a second submission still in the
                // encoder. Do not complete it until admission closes the window.
                if state.submitted == 0 {
                    state.submitted = 2;
                    return Ok(Some(encoded(true)));
                }
                if state.output_ready {
                    state.submitted += 1;
                }
                Ok(None)
            }
            fn drain_video(
                &mut self,
                _: &crate::DesktopContext,
                _: Duration,
            ) -> Result<Option<crate::EncodedFrame>> {
                let mut state = self.0.lock();
                if state.output_ready && !state.drained {
                    state.drained = true;
                    return Ok(Some(encoded(false)));
                }
                Ok(None)
            }
            fn input(&mut self, _: &Input) -> Result<()> {
                Ok(())
            }
            fn release(&mut self) {}
        }
        let pipeline = Arc::new(parking_lot::Mutex::new(Pipeline::default()));
        let source = pipeline.clone();
        let mut worker = Worker::start_video(
            Arc::new(move || Ok(Box::new(Video(source.clone())))),
            Arc::new(Policy::new(false)),
            Arc::new(parking_lot::Mutex::new(SessionStatus::default())),
            crate::VideoCodec::H264,
            crate::VideoPreferences::default(),
            true,
            None,
        );
        worker.frames.changed().await.unwrap();
        let initial = worker.media.pop().unwrap();
        let feedback = worker.feedback.clone().unwrap();
        assert_eq!(
            feedback.admit(&initial, initial.bytes.len()),
            crate::congestion::Admission::Send
        );
        assert!(feedback.budget().2);
        pipeline.lock().output_ready = true;
        worker.wake.notify();
        let started = Instant::now();
        let drained = tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                if let Some(frame) = worker.media.pop() {
                    break frame;
                }
                worker.frames.changed().await.unwrap();
            }
        })
        .await;
        // Always join the worker, including the pre-fix failure, so this test
        // cannot leave a capture task running inside the shared test runtime.
        worker.stop().await;
        let drained = drained
            .expect("ACK backpressure must not hold completed encoder output past its age limit");
        assert_eq!(
            pipeline.lock().submitted,
            2,
            "no new capture is allowed without credit"
        );
        assert_eq!(drained.status.generation, initial.status.generation);
        assert_eq!(drained.status.frame_id, initial.status.frame_id + 1);
        assert!(!drained.status.video.keyframe);
        assert!(
            feedback.budget().2,
            "draining is not a presentation acknowledgment"
        );
        println!(
            "paused_encoder drain_ms={:.3} submissions=2",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    #[tokio::test]
    async fn unavailable_desktop_retires_old_ack_debt_and_resumes_control() {
        #[derive(Default)]
        struct Capture {
            unavailable: bool,
            desktop: u64,
            pending: u32,
            ready: bool,
            inputs: u32,
            cursor: i32,
        }
        struct Video(Arc<parking_lot::Mutex<Capture>>);
        impl Backend for Video {
            fn display(&mut self) -> Result<Display> {
                if self.0.lock().unavailable {
                    return Err(error("desktop transition"));
                }
                Ok(Display {
                    id: 1,
                    name: "desktop transition".into(),
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                })
            }
            fn context(&mut self) -> Result<crate::DesktopContext> {
                Ok(crate::DesktopContext {
                    display: self.display()?,
                    generation: self.0.lock().desktop,
                    desktop: "test".into(),
                })
            }
            fn capture(&mut self) -> Result<Vec<u8>> {
                Err(error("test requires H264"))
            }
            fn capture_video(
                &mut self,
                _: &crate::DesktopContext,
                _: crate::VideoCodec,
                keyframe: bool,
                _: crate::VideoRate,
            ) -> Result<Option<crate::EncodedFrame>> {
                let mut state = self.0.lock();
                if state.pending > 0 {
                    state.pending -= 1;
                    return Ok(None);
                }
                if !std::mem::take(&mut state.ready) {
                    return Ok(None);
                }
                Ok(Some(crate::EncodedFrame {
                    format: crate::VideoFormat {
                        codec: crate::VideoCodec::H264,
                        keyframe,
                        width: 1920,
                        height: 1080,
                        ..Default::default()
                    },
                    bytes: vec![1; if keyframe { 128 * 1024 } else { 1024 }],
                }))
            }
            fn input(&mut self, _: &Input) -> Result<()> {
                self.0.lock().inputs += 1;
                Ok(())
            }
            fn cursor(
                &mut self,
                expected: &crate::DesktopContext,
            ) -> Result<Option<crate::CursorPosition>> {
                if self.context()? != *expected {
                    return Err(error("desktop changed during cursor sampling"));
                }
                Ok(Some(crate::CursorPosition {
                    x: self.0.lock().cursor,
                    y: 0,
                    visible: true,
                }))
            }
            fn release(&mut self) {}
        }
        async fn next(worker: &mut Worker) -> Frame {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(frame) = worker.media.pop() {
                        return frame;
                    }
                    worker.frames.changed().await.unwrap();
                }
            })
            .await
            .unwrap()
        }

        let capture = Arc::new(parking_lot::Mutex::new(Capture::default()));
        let produced = capture.clone();
        let mut worker = Worker::start_video(
            Arc::new(move || Ok(Box::new(Video(produced.clone())))),
            Arc::new(Policy::new(true)),
            Arc::new(parking_lot::Mutex::new(SessionStatus::default())),
            crate::VideoCodec::H264,
            crate::VideoPreferences::default(),
            true,
            None,
        );
        worker
            .command(Command::Control { enabled: true })
            .await
            .unwrap();
        capture.lock().ready = true;
        let feedback = worker.feedback.clone().unwrap();
        let initial = next(&mut worker).await;
        assert!(initial.status.video.keyframe);
        assert_eq!(
            feedback.admit(&initial, initial.bytes.len()),
            crate::congestion::Admission::Send
        );
        assert!(feedback.budget().2);

        // Cursor motion is observable while the initial IDR owns all video
        // credit. It creates neither a frame nor a presentation acknowledgment.
        worker.cursor.borrow_and_update();
        capture.lock().cursor = 17;
        let moved = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                worker.cursor.changed().await.unwrap();
                let state = worker.cursor.borrow_and_update().clone().unwrap();
                if state.cursor.is_some_and(|cursor| cursor.x == 17) {
                    break state;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(moved.generation, initial.status.generation);
        assert_eq!(moved.frame_id, initial.status.frame_id);
        assert_eq!(moved.frames, initial.status.frames);
        assert_eq!(moved.frame_ack_ms, 0.0);
        assert!(feedback.budget().2);
        assert!(worker.media.pop().is_none());
        assert_eq!(capture.lock().inputs, 0);

        capture.lock().unavailable = true;
        let unavailable = next(&mut worker).await;
        assert_eq!(unavailable.status.generation, initial.status.generation + 1);
        // Real Worker uses std::time for the four-second ACK deadline. Wait on
        // its repeated status frames, not sleeps or a fake Engine clock.
        let until = Instant::now() + Duration::from_millis(4100);
        while Instant::now() < until {
            let frame = next(&mut worker).await;
            assert_eq!(frame.status.state, "capture_unavailable");
            assert_eq!(frame.status.generation, unavailable.status.generation);
            assert!(frame.bytes.is_empty());
            assert!(!feedback.stalled());
        }
        {
            let mut state = capture.lock();
            state.unavailable = false;
            state.desktop += 1;
            state.pending = 3;
            state.ready = true;
        }
        let recovered = loop {
            let frame = next(&mut worker).await;
            if !frame.bytes.is_empty() {
                break frame;
            }
        };
        assert_eq!(capture.lock().pending, 0);
        assert_eq!(
            recovered.status.generation,
            unavailable.status.generation + 1
        );
        assert!(recovered.status.video.keyframe);
        assert!(recovered.status.controlling);
        assert_eq!(
            feedback.admit(&recovered, recovered.bytes.len()),
            crate::congestion::Admission::Send
        );
        assert!(!feedback.stalled());
        assert!(feedback.budget().2);
        assert!(
            worker
                .command(Command::Presented {
                    generation: initial.status.generation,
                    frame_id: initial.status.frame_id,
                })
                .await
                .is_err()
        );
        assert!(
            feedback.budget().2,
            "old ACK must not release the new IDR's credit"
        );
        worker
            .command(Command::Presented {
                generation: recovered.status.generation,
                frame_id: recovered.status.frame_id,
            })
            .await
            .unwrap();
        assert!(!feedback.budget().2);
        worker
            .command(Command::Input {
                event: InputEvent {
                    generation: recovered.status.generation,
                    frame_id: recovered.status.frame_id,
                    sequence: 1,
                    input: Input::Move { x: 3, y: 3 },
                },
            })
            .await
            .unwrap();
        assert_eq!(capture.lock().inputs, 1);
        capture.lock().ready = true;
        let resumed = next(&mut worker).await;
        assert_eq!(resumed.status.generation, recovered.status.generation);
        assert!(!resumed.status.video.keyframe);
        worker.stop().await;
    }

    #[tokio::test]
    async fn decoder_reset_replaces_an_unacknowledged_idr_and_resumes_capture() {
        struct Video(Arc<AtomicU32>);
        impl Backend for Video {
            fn display(&mut self) -> Result<Display> {
                Ok(Display {
                    id: 1,
                    name: "decoder recovery".into(),
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                })
            }
            fn capture(&mut self) -> Result<Vec<u8>> {
                Err(error("test requires H264"))
            }
            fn capture_video(
                &mut self,
                _: &crate::DesktopContext,
                _: crate::VideoCodec,
                keyframe: bool,
                _: crate::VideoRate,
            ) -> Result<Option<crate::EncodedFrame>> {
                if self
                    .0
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_err()
                {
                    return Ok(None);
                }
                Ok(Some(crate::EncodedFrame {
                    format: crate::VideoFormat {
                        codec: crate::VideoCodec::H264,
                        keyframe,
                        width: 1920,
                        height: 1080,
                        ..Default::default()
                    },
                    bytes: vec![1; if keyframe { 128 * 1024 } else { 1024 }],
                }))
            }
            fn input(&mut self, _: &Input) -> Result<()> {
                Ok(())
            }
            fn release(&mut self) {}
        }
        async fn next(worker: &mut Worker) -> Frame {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(frame) = worker.media.pop() {
                        return frame;
                    }
                    worker.frames.changed().await.unwrap();
                }
            })
            .await
            .unwrap()
        }

        let remaining = Arc::new(AtomicU32::new(1));
        let produced = remaining.clone();
        let mut worker = Worker::start_video(
            Arc::new(move || Ok(Box::new(Video(produced.clone())))),
            Arc::new(Policy::new(false)),
            Arc::new(parking_lot::Mutex::new(SessionStatus::default())),
            crate::VideoCodec::H264,
            crate::VideoPreferences::default(),
            true,
            None,
        );
        let feedback = worker.feedback.clone().unwrap();
        let initial = next(&mut worker).await;
        assert!(initial.status.video.keyframe);
        assert_eq!(
            feedback.admit(&initial, initial.bytes.len()),
            crate::congestion::Admission::Send
        );
        assert!(feedback.budget().2);

        // The renderer discarded the initial IDR and cannot acknowledge it.
        worker.command(Command::Keyframe).await.unwrap();
        remaining.store(1, Ordering::SeqCst);
        let replacement = next(&mut worker).await;
        assert!(replacement.status.video.keyframe);
        assert!(replacement.status.frame_id > initial.status.frame_id);
        assert_eq!(
            feedback.admit(&replacement, replacement.bytes.len()),
            crate::congestion::Admission::Send
        );
        assert!(feedback.budget().2);
        worker
            .command(Command::Presented {
                generation: replacement.status.generation,
                frame_id: replacement.status.frame_id,
            })
            .await
            .unwrap();
        assert!(!feedback.budget().2);
        remaining.store(1, Ordering::SeqCst);
        let resumed = next(&mut worker).await;
        assert!(!resumed.status.video.keyframe);
        assert_eq!(
            feedback.admit(&resumed, resumed.bytes.len()),
            crate::congestion::Admission::Send
        );
        assert!(feedback.request_keyframe());
        worker.stop().await;
    }
}
