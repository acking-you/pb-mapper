use crate::{Backend, Frame, InputEvent, Result, SessionStatus, error, wire::Command};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

pub(crate) type Factory = Arc<dyn Fn() -> Result<Box<dyn Backend>> + Send + Sync>;
type Request = (Command, oneshot::Sender<Result<()>>);

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

pub(crate) struct Worker {
    requests: mpsc::SyncSender<Request>,
    pub frames: watch::Receiver<Option<Frame>>,
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
                    backend.configure_video(preferences)?;
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
            engine.admission = admission;
            let mut policy = allowed.get();
            let mut next_frame = Instant::now();
            let mut configured = preferences;
            let mut checked_context = Instant::now();
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
                                engine.keyframe = true;
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
                            Command::Authenticate { .. } => Err(error("already authenticated")),
                        };
                        if let Err(e) = &result {
                            engine.state.message = e.to_string();
                        }
                        let _ = reply.send(result);
                    }
                    Err(e) => {
                        status.lock().message = e.to_string();
                        break;
                    }
                    Ok(None) => {}
                }
                if Instant::now() >= next_frame {
                    let mut paused = false;
                    if let Some(feedback) = &renderer {
                        let (preferences, bitrate, waiting) = feedback.budget();
                        engine.bitrate = bitrate;
                        paused = waiting;
                        if configured != preferences {
                            match engine.backend.configure_video(preferences) {
                                Ok(()) => {
                                    configured = preferences;
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
                    // Flow control pauses production without losing references.
                    // Only actual queue loss, stale output or a context change
                    // requires an IDR; restarting every credit window otherwise
                    // spends the available bandwidth repeatedly on keyframes.
                    if paused && checked_context.elapsed() >= Duration::from_millis(200) {
                        let _ = engine.geometry();
                        checked_context = Instant::now();
                    }
                    engine.keyframe |= outgoing.take_keyframe_request();
                    engine.state.dropped_frames = outgoing.dropped();
                    let started = Instant::now();
                    if !paused && let Some(frame) = engine.capture_video() {
                        if let Some(feedback) = &renderer {
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
    sample_at: Instant,
    sample_frames: u64,
    admission: Option<Arc<dyn crate::ControlAdmission>>,
    lease: Option<Box<dyn crate::ControlLease>>,
}
impl Engine {
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
            sample_at: Instant::now(),
            sample_frames: 0,
            admission: None,
            lease: None,
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
        self.release_control();
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
        self.capture_video().expect("fake capture returns an image")
    }
    fn capture_video(&mut self) -> Option<Frame> {
        let image = self.geometry().and_then(|()| {
            let context = self
                .context
                .as_ref()
                .ok_or_else(|| error("missing desktop context"))?;
            let frame =
                self.backend
                    .capture_video(context, self.codec, self.keyframe, self.bitrate)?;
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
                self.state.message.clear();
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
                self.invalidate();
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
        self.backend
            .input_in_context(context, &event.input)
            .inspect_err(|_| self.backend.release())
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
        engine.release_control();
        assert_eq!(writers.load(Ordering::SeqCst), 0);
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
}
