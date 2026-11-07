use super::*;
use crate::{
    CursorPosition, Display, Input, KeyframeReason, VideoFormat,
    lane::LANE_CAPACITY,
    outgoing::HostUnit,
    shared::{Policy, Targets},
};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use tokio_util::sync::CancellationToken;

const LIMITS: VideoLimits = VideoLimits {
    max_width: 1920,
    max_height: 1080,
    max_fps: 60,
};
const MOVE: Input = Input::Move { x: 3, y: 3 };

/// What the fake backend was asked to do, at the engine's clock.
#[derive(Default)]
struct Log {
    configured: u32,
    /// `(started, finished, keyframe requested, rate)`
    captures: Vec<(Instant, Instant, bool, VideoRate)>,
    inputs: Vec<(Instant, Input)>,
    releases: u32,
}

#[derive(Clone)]
struct Fake {
    log: Arc<Mutex<Log>>,
    clock: Clock,
    width: Arc<AtomicU32>,
    fail_input: Arc<AtomicBool>,
    fail_lock: Arc<AtomicBool>,
    /// Every capture fails, while the desktop stays as it was.
    fail_capture: Arc<AtomicBool>,
    /// The OS double-click time and box; `None` never holds a press.
    guard: Arc<Mutex<Option<crate::DoubleClickGuard>>>,
    /// How long a capture call takes, in real time.
    delay: Duration,
    /// Told when a capture call starts.
    started: Option<std::sync::mpsc::SyncSender<()>>,
    /// How long a picture stays in the encoder, at the engine's clock; zero
    /// returns it from the capture call itself.
    latency: Arc<Mutex<Duration>>,
    /// Submitted pictures, with when each is ready to drain.
    encoding: Arc<Mutex<VecDeque<(Instant, EncodedFrame)>>>,
}
impl Fake {
    fn new(clock: Clock) -> Self {
        Self {
            log: Arc::default(),
            clock,
            width: Arc::new(AtomicU32::new(10)),
            fail_input: Arc::default(),
            fail_lock: Arc::default(),
            fail_capture: Arc::default(),
            guard: Arc::default(),
            delay: Duration::ZERO,
            started: None,
            latency: Arc::default(),
            encoding: Arc::default(),
        }
    }

    /// The oldest picture the encoder has finished, if any.
    fn finished(&self) -> Option<EncodedFrame> {
        let now = (self.clock)();
        let mut encoding = self.encoding.lock();
        encoding
            .front()
            .is_some_and(|(ready, _)| *ready <= now)
            .then(|| encoding.pop_front().map(|(_, frame)| frame))
            .flatten()
    }
}
impl Backend for Fake {
    fn configure_video(&mut self, limits: VideoLimits, _: u32) -> Result<()> {
        self.log.lock().configured += 1;
        limits.validate()
    }
    fn display(&mut self) -> Result<Display> {
        Ok(Display {
            id: 1,
            name: "fake".into(),
            x: 0,
            y: 0,
            width: self.width.load(Ordering::SeqCst),
            height: 10,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        Ok(vec![1])
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<CursorPosition>> {
        if &self.context()? != expected {
            return Err(error("fake desktop changed"));
        }
        Ok(Some(CursorPosition {
            x: 3,
            y: 3,
            visible: true,
        }))
    }
    fn capture_video(
        &mut self,
        _: &DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        let started = (self.clock)();
        if let Some(notify) = &self.started {
            let _ = notify.try_send(());
        }
        std::thread::sleep(self.delay);
        self.log
            .lock()
            .captures
            .push((started, (self.clock)(), keyframe, rate));
        if self.fail_capture.load(Ordering::SeqCst) {
            return Err(error("no compatible hardware encoder"));
        }
        let frame = EncodedFrame {
            format: VideoFormat {
                codec,
                width: 16,
                height: 10,
                keyframe,
                gop: u32::MAX,
                ..VideoFormat::default()
            },
            bytes: vec![1; 8],
        };
        let latency = *self.latency.lock();
        if latency.is_zero() {
            return Ok(Some(frame));
        }
        // As the Windows encoder: drain what is finished, then submit.
        let finished = self.finished();
        self.encoding.lock().push_back((started + latency, frame));
        Ok(finished)
    }
    fn drain_video(&mut self, _: &DesktopContext, _: Duration) -> Result<Option<EncodedFrame>> {
        Ok(self.finished())
    }
    fn pending_video(&self) -> usize {
        self.encoding.lock().len()
    }
    fn input(&mut self, event: &Input) -> Result<()> {
        if self.fail_input.load(Ordering::SeqCst)
            || matches!(event, Input::Lock {}) && self.fail_lock.load(Ordering::SeqCst)
        {
            return Err(error("native injection rejected"));
        }
        self.log.lock().inputs.push(((self.clock)(), event.clone()));
        Ok(())
    }
    fn release(&mut self) {
        self.log.lock().releases += 1;
    }
    fn double_click_guard(&mut self) -> Option<crate::DoubleClickGuard> {
        *self.guard.lock()
    }
}

fn session(allowed: bool) -> Arc<SessionShared> {
    Arc::new(SessionShared::new(
        Arc::new(Policy::new(allowed)),
        CancellationToken::new(),
        Targets::new(60, 4_000_000),
    ))
}

fn config(admission: Option<Arc<dyn ControlAdmission>>) -> WorkerConfig {
    WorkerConfig {
        codec: VideoCodec::H264,
        limits: LIMITS,
        nominal_fps: 60,
        admission,
    }
}

/// Thread C on a fake clock: each step runs one iteration at the chosen time,
/// and the test plays B-recv and B-send through `shared`.
struct Harness {
    shared: Arc<SessionShared>,
    engine: Engine,
    now: Arc<Mutex<Instant>>,
    fake: Fake,
    sequence: u64,
    presented: (u64, u64),
    /// The state each step published.
    published: Vec<SessionState>,
}
impl Harness {
    fn new(allowed: bool, admission: Option<Arc<dyn ControlAdmission>>) -> Self {
        let now = Arc::new(Mutex::new(Instant::now()));
        let time = now.clone();
        let clock: Clock = Arc::new(move || *time.lock());
        let fake = Fake::new(clock.clone());
        let shared = session(allowed);
        let backend = fake.clone();
        let factory: Factory = Arc::new(move || Ok(Box::new(backend.clone())));
        let engine = Engine::start(&shared, &factory, config(admission), clock).unwrap();
        Self {
            shared,
            engine,
            now,
            fake,
            sequence: 0,
            presented: (0, 0),
            published: Vec::new(),
        }
    }
    fn now(&self) -> Instant {
        *self.now.lock()
    }
    /// One iteration at the current time; returns its deadline.
    fn step(&mut self) -> Instant {
        let deadline = match self.engine.iterate() {
            Step::Wait(deadline) => deadline,
            Step::Exit(exit) => panic!("thread C exited: {exit:?}"),
        };
        self.published.push(self.state().state);
        deadline
    }
    /// Iterate at every deadline for `span`, with B-send taking every unit when
    /// `send` is set.
    fn run(&mut self, span: Duration, send: bool) {
        let end = self.now() + span;
        let mut spins = 0;
        loop {
            let deadline = self.step();
            if send {
                self.sent();
            }
            if deadline > end {
                *self.now.lock() = end;
                return;
            }
            let now = self.now();
            spins = if deadline > now { 0 } else { spins + 1 };
            assert!(spins < 1000, "thread C spins without waiting");
            *self.now.lock() = deadline.max(now);
        }
    }
    /// B-send writes everything queued.
    fn sent(&self) -> Vec<HostUnit> {
        std::iter::from_fn(|| self.shared.outgoing.pop()).collect()
    }
    /// The viewer presents the newest unit of the current generation.
    fn present(&mut self) {
        self.run(Duration::from_millis(50), false);
        let generation = self.state().generation;
        let unit = self
            .sent()
            .into_iter()
            .rev()
            .find(|unit| unit.generation == generation)
            .unwrap();
        let now = self.now();
        assert!(
            self.shared
                .auth
                .lock()
                .on_presented(generation, unit.frame_id, now)
        );
        self.presented = (generation, unit.frame_id);
    }
    fn send(&self, command: LaneCommand) {
        assert!(self.shared.lane.push(command));
    }
    fn input(&mut self, input: Input) {
        self.sequence += 1;
        self.send(LaneCommand::Input(InputEvent {
            generation: self.presented.0,
            frame_id: self.presented.1,
            sequence: self.sequence,
            input,
        }));
    }
    fn state(&self) -> EngineState {
        self.shared.state.borrow().clone()
    }
    fn inputs(&self) -> usize {
        self.fake.log.lock().inputs.len()
    }
    fn captures(&self) -> Vec<(Instant, bool, VideoRate)> {
        let log = self.fake.log.lock();
        log.captures
            .iter()
            .map(|(started, _, keyframe, rate)| (*started, *keyframe, *rate))
            .collect()
    }
}

fn spacing(captures: &[(Instant, bool, VideoRate)]) -> Vec<Duration> {
    captures
        .windows(2)
        .map(|pair| pair[1].0 - pair[0].0)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_lane_runs_before_capture_and_never_after_stop() {
    let shared = session(true);
    let (started, starts) = std::sync::mpsc::sync_channel(64);
    let mut fake = Fake::new(Arc::new(Instant::now));
    fake.delay = Duration::from_millis(30);
    fake.started = Some(started);
    let log = fake.log.clone();
    let factory: Factory = Arc::new(move || Ok(Box::new(fake.clone())));
    let mut worker = spawn_session(shared.clone(), factory, config(None));
    let wait_for = |what: &str, done: &dyn Fn() -> bool| {
        let until = Instant::now() + Duration::from_secs(2);
        while !done() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    let control = || assert!(shared.lane.push(LaneCommand::Control { enabled: true }));
    // B-send writes every unit; the viewer presents the newest one.
    let written = Arc::new(Mutex::new((0, 0)));
    let sender = {
        let (shared, written) = (shared.clone(), written.clone());
        std::thread::spawn(move || {
            while !shared.cancel.is_cancelled() {
                while let Some(unit) = shared.outgoing.pop() {
                    *written.lock() = (unit.generation, unit.frame_id);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let present = || {
        let generation = shared.state.borrow().generation;
        wait_for("video", &|| written.lock().0 == generation);
        let (generation, frame_id) = *written.lock();
        assert!(
            shared
                .auth
                .lock()
                .on_presented(generation, frame_id, Instant::now())
        );
        (generation, frame_id)
    };
    let mut sequence = 0;
    let mut input = |(generation, frame_id): (u64, u64), x| {
        sequence += 1;
        shared.lane.push(LaneCommand::Input(InputEvent {
            generation,
            frame_id,
            sequence,
            input: Input::Move { x, y: 1 },
        }))
    };
    let next_capture = || {
        while starts.try_recv().is_ok() {}
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
    };
    control();
    wait_for("control", &|| shared.state.borrow().controlling);
    let frame = present();

    // Input queued during a 30 ms capture runs right after it, in order, and
    // before the next capture starts.
    next_capture();
    for x in 1..=3 {
        assert!(input(frame, x));
    }
    wait_for("input", &|| log.lock().inputs.len() == 3);
    {
        let log = log.lock();
        let injected: Vec<_> = log.inputs.iter().map(|(at, _)| *at).collect();
        let finished = log
            .captures
            .iter()
            .map(|(_, finished, ..)| *finished)
            .filter(|finished| *finished <= injected[0])
            .max()
            .unwrap();
        assert!(injected[0] - finished < Duration::from_millis(10));
        let order = log.inputs.iter().map(|(_, input)| match input {
            Input::Move { x, .. } => *x,
            _ => 0,
        });
        assert!(order.eq(1..=3));
        assert!(
            log.captures
                .iter()
                .all(|(started, ..)| *started < finished || *started >= injected[2])
        );
    }

    // A full lane ends control rather than inject a late burst.
    next_capture();
    for _ in 0..LANE_CAPACITY {
        assert!(shared.lane.push(LaneCommand::Release));
    }
    assert!(!shared.lane.push(LaneCommand::Release));
    wait_for("congestion", &|| {
        let state = shared.state.borrow();
        !state.controlling && state.reason == Some(StateReason::InputCongestion)
    });

    // Nothing queued before a stop runs.
    control();
    wait_for("control", &|| shared.state.borrow().controlling);
    let frame = present();
    next_capture();
    assert!(input(frame, 4));
    shared.cancel.cancel();
    assert_eq!(worker.exited().await, WorkerExit::Cancelled);
    assert_eq!(log.lock().inputs.len(), 3);
    sender.join().unwrap();
}

#[test]
fn pacing_is_even_and_skips_ticks_without_bursts() {
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    // Pace 60 with B-send keeping up: one submit per tick, evenly.
    h.run(Duration::from_millis(500), true);
    let even = h.captures();
    assert!((29..=31).contains(&even.len()), "{}", even.len());
    assert!(even[0].1, "a new encoder starts with an IDR");
    assert!(even[1..].iter().all(|(_, keyframe, _)| !keyframe));
    assert!(
        spacing(&even)
            .iter()
            .all(|gap| gap.abs_diff(period) < Duration::from_micros(10))
    );

    // B-send stalls: once two units wait, ticks are skipped, never queued.
    h.run(Duration::from_millis(200), false);
    assert!(h.captures().len() - even.len() <= 2);
    assert!(h.state().skipped_ticks >= 10);
    assert!(h.shared.outgoing.len() <= 2);

    // Credit returns after a long backend stall: no catch-up burst.
    let before = h.captures().len();
    h.sent();
    *h.now.lock() += Duration::from_millis(100);
    h.run(Duration::from_millis(200), true);
    let released = &h.captures()[before..];
    assert!((11..=13).contains(&released.len()), "{}", released.len());
    assert!(spacing(released).iter().all(|gap| *gap >= period * 3 / 4));

    // Pause: nothing is submitted or sent, and the first unit after resume is
    // an IDR.
    h.shared.targets.paused.store(true, Ordering::Release);
    let before = h.captures().len();
    h.run(Duration::from_millis(200), true);
    assert_eq!(h.captures().len(), before);
    assert_eq!(h.state().state, SessionState::Paused);
    h.shared.targets.paused.store(false, Ordering::Release);
    h.run(Duration::from_millis(50), true);
    let resumed = &h.captures()[before..];
    assert!(resumed[0].1);
    assert!(resumed[1..].iter().all(|(_, keyframe, _)| !keyframe));
    assert_eq!(h.state().state, SessionState::Streaming);
}

#[test]
fn fps_and_preset_changes_never_reconfigure_the_encoder() {
    let mut h = Harness::new(true, None);
    h.run(Duration::from_millis(100), true);
    let generation = h.state().generation;
    // The encoder's read-back GOP reaches the state, and so `Stats`.
    assert_eq!(h.state().gop, u32::MAX);
    // QoS pace and bitrate changes, and a preset switch, are rates only.
    for (pace_fps, bitrate) in [(30, 2_000_000), (15, 8_000_000), (60, 6_000_000)] {
        let targets = &h.shared.targets;
        targets.pace_fps.store(pace_fps, Ordering::Release);
        targets.encoder_bps.store(bitrate, Ordering::Release);
        h.run(Duration::from_millis(200), true);
        let (_, keyframe, rate) = *h.captures().last().unwrap();
        assert_eq!(rate, VideoRate { bitrate, pace_fps });
        assert!(!keyframe);
    }
    assert_eq!(h.fake.log.lock().configured, 1);
    assert_eq!(h.state().generation, generation);

    // A display change rebuilds once, with an IDR and a new generation.
    h.fake.width.store(20, Ordering::SeqCst);
    let before = h.captures().len();
    h.run(Duration::from_millis(300), true);
    assert_eq!(h.fake.log.lock().configured, 2);
    let rebuilt = &h.captures()[before..];
    assert!(rebuilt[0].1);
    assert!(rebuilt[1..].iter().all(|(_, keyframe, _)| !keyframe));
    assert!(h.state().generation > generation);
    assert_eq!(h.state().display.map(|display| display.width), Some(20));
}

#[test]
fn an_idr_slower_than_a_tick_is_asked_for_once() {
    let mut h = Harness::new(true, None);
    // An IDR spends 40 ms in the encoder, over two ticks at 60 fps.
    *h.fake.latency.lock() = Duration::from_millis(40);
    let keyframes = |h: &Harness| h.captures().iter().filter(|capture| capture.1).count();
    h.run(Duration::from_millis(300), true);
    assert_eq!(keyframes(&h), 1, "the new encoder's IDR");
    // A forced IDR, and a requested one, are each asked for once.
    h.send(LaneCommand::Control { enabled: true });
    h.run(Duration::from_millis(300), true);
    assert_eq!(keyframes(&h), 2);
    h.shared
        .idr
        .lock()
        .request(IdrReason::Viewer(KeyframeReason::Gap));
    h.run(Duration::from_millis(500), true);
    assert_eq!(keyframes(&h), 3);
}

#[test]
fn a_capture_that_keeps_failing_is_retried_every_unavailable_retry() {
    let mut h = Harness::new(true, None);
    h.run(Duration::from_millis(100), true);
    let generation = h.state().generation;
    h.fake.fail_capture.store(true, Ordering::SeqCst);
    let before = h.captures().len();
    h.published.clear();
    h.run(Duration::from_secs(1), true);
    // One retry at once, since a failure while streaming may be a desktop
    // switch, then one every 200 ms, never at the capture pace. Only the
    // first failure starts a new generation, and no retry shows as
    // recovered.
    let attempts = h.captures().len() - before;
    assert!((5..=7).contains(&attempts), "{attempts} captures in 1 s");
    let failing = h.state();
    assert_eq!(failing.state, SessionState::CaptureUnavailable);
    assert_eq!(failing.generation, generation + 1);
    assert!(!h.published.contains(&SessionState::Connecting));

    h.fake.fail_capture.store(false, Ordering::SeqCst);
    h.run(Duration::from_millis(300), true);
    assert_eq!(h.state().state, SessionState::Streaming);
}

#[tokio::test]
async fn unavailable_backend_reports_failure_and_stops() {
    let shared = session(false);
    let mut worker = spawn_session(
        shared.clone(),
        Arc::new(|| Err(error("desktop is locked"))),
        config(None),
    );
    let exit = tokio::time::timeout(Duration::from_secs(2), worker.exited())
        .await
        .unwrap();
    assert!(matches!(exit, WorkerExit::CaptureFailed(message) if message.contains("locked")));
    let state = shared.state.borrow().clone();
    assert_eq!(state.state, SessionState::CaptureUnavailable);
    assert!(state.message.contains("locked"));
}

#[test]
fn input_requires_control_current_geometry_and_unique_sequence() {
    let mut h = Harness::new(true, None);
    h.present();
    h.input(MOVE);
    h.step();
    assert_eq!(h.inputs(), 0, "viewing grants no input");

    h.send(LaneCommand::Control { enabled: true });
    h.step();
    // The grant started a new generation: the old presentation admits
    // nothing until a frame of the new one is presented.
    h.input(MOVE);
    h.step();
    assert_eq!(h.inputs(), 0);
    h.present();
    h.input(MOVE);
    h.step();
    assert_eq!(h.inputs(), 1);
    // A repeated sequence, and a frame newer than the presented one.
    h.sequence -= 1;
    h.input(MOVE);
    h.presented.1 += 1;
    h.input(MOVE);
    h.presented.1 -= 1;
    h.step();
    assert_eq!(h.inputs(), 1);

    // A different desktop at the same position invalidates input too.
    h.fake.width.store(11, Ordering::SeqCst);
    let releases = h.fake.log.lock().releases;
    h.input(MOVE);
    h.step();
    assert_eq!(h.inputs(), 1);
    assert!(h.fake.log.lock().releases > releases);

    // View-only: the host's revoke ends control at once.
    h.present();
    h.shared.policy.set(false);
    h.input(MOVE);
    h.step();
    assert!(!h.state().controlling);
    assert!(!h.state().control_allowed);
    assert_eq!(h.inputs(), 1);
}

#[test]
fn native_input_failure_ends_control_and_survives_new_video() {
    let mut h = Harness::new(true, None);
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    h.present();
    h.fake.fail_input.store(true, Ordering::SeqCst);
    h.input(Input::Button {
        button: crate::Button::Left,
        down: true,
        drag_start: false,
    });
    h.step();
    let failed = h.state();
    assert!(!failed.controlling);
    assert_eq!(failed.reason, Some(StateReason::InputFailed));
    assert!(failed.message.starts_with("input_failed"));
    // New video does not hide why control ended, and a queued input is
    // never replayed.
    h.input(MOVE);
    h.run(Duration::from_millis(200), true);
    assert_eq!(h.state().reason, Some(StateReason::InputFailed));
    assert_eq!(h.state().message, failed.message);

    h.fake.fail_input.store(false, Ordering::SeqCst);
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    assert_eq!(h.state().reason, None);
    h.present();
    h.input(MOVE);
    h.step();
    assert!(h.state().controlling);
    assert_eq!(h.inputs(), 1);
}

#[test]
fn observation_never_acquires_write_admission_and_revocation_releases_it() {
    struct Admission(Arc<AtomicU64>);
    struct Lease(Arc<AtomicU64>);
    impl ControlAdmission for Admission {
        fn acquire(&self) -> Result<Box<dyn ControlLease>> {
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
    let mut h = Harness::new(false, Some(Arc::new(Admission(writers.clone()))));
    // Capture, cursor sampling and presentation acquire nothing.
    h.present();
    h.run(Duration::from_millis(200), true);
    assert_eq!(writers.load(Ordering::SeqCst), 0);
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    assert_eq!(writers.load(Ordering::SeqCst), 0);
    assert_eq!(h.state().reason, Some(StateReason::ControlDenied));

    h.shared.policy.set(true);
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    assert_eq!(writers.load(Ordering::SeqCst), 1);
    // A repeated grant keeps the lease and the generation.
    let generation = h.state().generation;
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    assert_eq!(writers.load(Ordering::SeqCst), 1);
    assert_eq!(h.state().generation, generation);
    h.send(LaneCommand::EndControl);
    h.step();
    assert_eq!(writers.load(Ordering::SeqCst), 0);
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    assert_eq!(writers.load(Ordering::SeqCst), 1);
    drop(h);
    assert_eq!(writers.load(Ordering::SeqCst), 0);
}

#[test]
fn lock_is_control_gated_and_failure_keeps_control() {
    let mut h = Harness::new(false, None);
    // View-only: the lock never reaches the backend.
    h.present();
    h.input(Input::Lock {});
    h.step();
    assert_eq!(h.inputs(), 0);
    assert!(h.state().notice.is_none());

    h.shared.policy.set(true);
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    h.present();
    h.fake.fail_lock.store(true, Ordering::SeqCst);
    h.input(Input::Lock {});
    h.step();
    let failed = h.state();
    assert!(failed.controlling, "a failed lock held nothing down");
    let (code, message, serial) = failed.notice.unwrap();
    assert_eq!(code, NoticeCode::LockFailed);
    assert!(message.contains("rejected"));
    // The notice shows for 5 s.
    h.run(Duration::from_millis(4900), true);
    assert_eq!(h.state().notice.map(|notice| notice.2), Some(serial));
    h.run(Duration::from_millis(200), true);
    assert!(h.state().notice.is_none());
    assert!(h.state().controlling);

    h.fake.fail_lock.store(false, Ordering::SeqCst);
    h.present();
    h.input(Input::Lock {});
    h.step();
    let (code, _, locked) = h.state().notice.unwrap();
    assert_eq!(code, NoticeCode::Locked);
    assert!(locked > serial);
    assert_eq!(h.inputs(), 1);
}

#[test]
fn drag_start_press_waits_out_the_double_click_time() {
    fn left(down: bool, drag_start: bool) -> Input {
        Input::Button {
            button: crate::Button::Left,
            down,
            drag_start,
        }
    }
    /// Control, a fresh presentation, a click, and 100 ms later a drag start
    /// and a move, which are held.
    fn hold(h: &mut Harness) -> usize {
        if !h.state().controlling {
            h.send(LaneCommand::Control { enabled: true });
            h.step();
        }
        h.present();
        h.input(MOVE);
        h.input(left(true, false));
        h.input(left(false, false));
        h.step();
        *h.now.lock() += Duration::from_millis(100);
        let injected = h.inputs();
        h.input(left(true, true));
        h.input(Input::Move { x: 4, y: 4 });
        h.step();
        assert_eq!(h.inputs(), injected);
        injected
    }
    let ms = Duration::from_millis;
    let mut h = Harness::new(true, None);
    *h.fake.guard.lock() = Some(crate::DoubleClickGuard {
        time_ms: 500,
        half_box: (2, 2),
    });
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    h.present();

    // A click at t0, and a drag that starts at t0 + 200 ms: the press waits
    // until t0 + 520 ms, and the move queued after it follows it.
    h.input(MOVE);
    h.input(left(true, false));
    h.input(left(false, false));
    h.step();
    let t0 = h.now();
    *h.now.lock() = t0 + ms(200);
    h.input(left(true, true));
    h.input(Input::Move { x: 4, y: 4 });
    let deadline = h.step();
    assert_eq!(h.inputs(), 3);
    assert!(deadline <= t0 + ms(520));
    h.run(ms(400), true);
    {
        let log = h.fake.log.lock();
        assert_eq!(log.inputs.len(), 5);
        let (pressed, press) = &log.inputs[3];
        assert!(*pressed >= t0 + ms(520));
        assert!(matches!(press, Input::Button { down: true, .. }));
        let (moved, motion) = &log.inputs[4];
        assert!(moved >= pressed);
        assert!(matches!(motion, Input::Move { x: 4, y: 4 }));
    }

    // A press without the flag is never held.
    h.input(left(false, false));
    h.input(left(true, false));
    h.input(left(false, false));
    h.step();
    *h.now.lock() += ms(100);
    let injected = h.inputs();
    h.input(left(true, false));
    h.step();
    assert_eq!(h.inputs(), injected + 1);
    h.input(left(false, false));
    h.step();

    // A release, a policy change or a stale presentation during the hold:
    // nothing held is injected, ever.
    let injected = hold(&mut h);
    h.send(LaneCommand::Release);
    h.run(ms(700), true);
    assert_eq!(h.inputs(), injected);

    let injected = hold(&mut h);
    h.shared.policy.set(true);
    h.run(ms(700), true);
    assert_eq!(h.inputs(), injected);
    assert!(!h.state().controlling);

    let injected = hold(&mut h);
    let (generation, now) = (h.state().generation, h.now());
    assert!(!h.shared.auth.lock().on_presented(generation, u64::MAX, now));
    h.run(ms(700), true);
    assert_eq!(h.inputs(), injected);
}
