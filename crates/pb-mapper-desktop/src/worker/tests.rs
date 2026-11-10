use super::*;
use crate::{
    CursorPosition, Display, EncodedFrame, Input, InputEvent, KeyframeReason, VideoFormat,
    VideoRate, error,
    idr::IdrReason,
    lane::LANE_CAPACITY,
    modes::ModeCommand,
    outgoing::HostUnit,
    shared::{Policy, Targets},
};
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64},
};
use tokio_util::sync::CancellationToken;

const LIMITS: VideoLimits = VideoLimits {
    max_width: 1920,
    max_height: 1080,
    max_fps: 60,
};
const MOVE: Input = Input::Move { x: 3, y: 3 };

/// The fake backend's own reading of the engine's time.
type Time = Arc<dyn Fn() -> Instant + Send + Sync>;

/// A clock the test sets.
struct Manual(Arc<Mutex<Instant>>);
impl Clock for Manual {
    fn now(&self) -> Instant {
        *self.0.lock()
    }
}

/// What the fake backend was asked to do, at the engine's clock.
#[derive(Default)]
struct Log {
    configured: u32,
    /// `(started, finished, keyframe requested, rate)`
    captures: Vec<(Instant, Instant, bool, VideoRate)>,
    inputs: Vec<(Instant, Input)>,
    releases: u32,
    /// Desktop reads, by context checks, the cursor and the injection boundary.
    contexts: u32,
}

#[derive(Clone)]
struct Fake {
    log: Arc<Mutex<Log>>,
    clock: Time,
    width: Arc<AtomicU32>,
    fail_input: Arc<AtomicBool>,
    fail_lock: Arc<AtomicBool>,
    /// Every capture fails, while the desktop stays as it was.
    fail_capture: Arc<AtomicBool>,
    /// The OS double-click time and box; `None` never holds a press.
    guard: Arc<Mutex<Option<crate::DoubleClickGuard>>>,
    /// Where the pointer is: where the last move put it, or where the test
    /// moved it.
    pointer: Arc<Mutex<(i32, i32)>>,
    /// A `ClipCursor` bound: a move further right stops there.
    clip: Arc<Mutex<Option<u32>>>,
    /// The pointer is hidden, and reads as the Windows backend reads it then.
    hidden: Arc<AtomicBool>,
    /// How long a capture call takes, in real time.
    delay: Duration,
    /// Told when a capture call starts.
    started: Option<std::sync::mpsc::SyncSender<()>>,
    /// How long a picture stays in the encoder, at the engine's clock; zero
    /// returns it from the capture call itself.
    latency: Arc<Mutex<Duration>>,
    /// Submitted pictures, with when each was submitted and is ready to drain.
    encoding: Arc<Mutex<VecDeque<(Instant, Instant, EncodedFrame)>>>,
    /// While set, every other capture call drops the oldest picture rather
    /// than return anything, as the Windows encoder discards a late output in
    /// the call that submits the next: the encoder holds as many as before.
    discard: Arc<AtomicBool>,
}
impl Fake {
    fn new(clock: Time) -> Self {
        Self {
            log: Arc::default(),
            clock,
            width: Arc::new(AtomicU32::new(10)),
            fail_input: Arc::default(),
            fail_lock: Arc::default(),
            fail_capture: Arc::default(),
            guard: Arc::default(),
            pointer: Arc::new(Mutex::new((3, 3))),
            clip: Arc::default(),
            hidden: Arc::default(),
            delay: Duration::ZERO,
            started: None,
            latency: Arc::default(),
            encoding: Arc::default(),
            discard: Arc::default(),
        }
    }

    /// The oldest picture the encoder has finished, if any, with its submit
    /// to output time as the Windows encoder measures it.
    fn finished(&self) -> Option<EncodedFrame> {
        let now = (self.clock)();
        let mut encoding = self.encoding.lock();
        if encoding.front().is_none_or(|(_, ready, _)| *ready > now) {
            return None;
        }
        let (submitted, _, mut frame) = encoding.pop_front()?;
        frame.format.encode_us = u32::try_from((now - submitted).as_micros()).unwrap();
        Some(frame)
    }
}
impl Backend for Fake {
    fn configure_video(&mut self, limits: VideoLimits, _: u32) -> Result<()> {
        self.log.lock().configured += 1;
        limits.validate()
    }
    fn display(&mut self) -> Result<Display> {
        self.log.lock().contexts += 1;
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
        if self.hidden.load(Ordering::SeqCst) {
            return Ok(Some(CursorPosition::default()));
        }
        let (x, y) = *self.pointer.lock();
        Ok(Some(CursorPosition {
            x,
            y,
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
        let calls = {
            let mut log = self.log.lock();
            log.captures.push((started, (self.clock)(), keyframe, rate));
            log.captures.len()
        };
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
        let finished = if self.discard.load(Ordering::SeqCst) && calls % 2 == 0 {
            self.encoding.lock().pop_front();
            None
        } else {
            self.finished()
        };
        self.encoding
            .lock()
            .push_back((started, started + latency, frame));
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
        if let Input::Move { x, y } = *event {
            let x = self.clip.lock().map_or(x, |clip| x.min(clip));
            *self.pointer.lock() = (x as i32, y as i32);
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
        admission,
    }
}

/// Thread C on a fake clock: each step runs one iteration at the chosen time,
/// and the test plays B-recv and B-send through `shared`.
struct Harness {
    shared: Arc<SessionShared>,
    engine: Engine<Fake, Manual>,
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
        let clock: Time = Arc::new(move || *time.lock());
        let fake = Fake::new(clock.clone());
        let shared = session(allowed);
        let engine = Engine::start(
            &shared,
            fake.clone(),
            config(admission),
            Manual(now.clone()),
        )
        .unwrap();
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
        self.published.push(self.state().record.state);
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
        let generation = self.state().record.generation;
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
    let mut worker = prepare(move || Ok(fake), false).start(shared.clone(), config(None));
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
        let generation = shared.state.borrow().record.generation;
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
    wait_for("control", &|| shared.state.borrow().record.controlling);
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
        !state.record.controlling && state.record.reason == Some(StateReason::InputCongestion)
    });

    // Nothing queued before a stop runs.
    control();
    wait_for("control", &|| shared.state.borrow().record.controlling);
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
    assert_eq!(h.state().record.state, SessionState::Paused);
    h.shared.targets.paused.store(false, Ordering::Release);
    h.run(Duration::from_millis(50), true);
    let resumed = &h.captures()[before..];
    assert!(resumed[0].1);
    assert!(resumed[1..].iter().all(|(_, keyframe, _)| !keyframe));
    assert_eq!(h.state().record.state, SessionState::Streaming);

    // An encoder that holds each unit for 45 ms, nearly three periods: no
    // tick is skipped for it.
    *h.fake.latency.lock() = Duration::from_millis(45);
    let (before, skipped) = (h.captures().len(), h.state().skipped_ticks);
    h.run(Duration::from_millis(2100), true);
    let slow = h.captures().len() - before;
    assert!((125..=127).contains(&slow), "{slow}");
    assert_eq!(h.state().skipped_ticks, skipped);
}

#[test]
fn the_encoder_dwell_stays_true_when_the_encoder_drops_units() {
    let mut h = Harness::new(true, None);
    // An encoder that holds each unit for 45 ms never runs empty at 60 fps.
    // It drops units in calls that also submit, so it holds as many as
    // before: which unit a later call returns cannot be told from the count.
    *h.fake.latency.lock() = Duration::from_millis(45);
    h.run(Duration::from_millis(500), true);
    h.fake.discard.store(true, Ordering::SeqCst);
    // Two windows, so the published one began after the drops did.
    h.run(Duration::from_millis(2100), true);
    let dwell = h.state().encoder_dwell.unwrap();
    assert!(
        (Duration::from_millis(45)..=Duration::from_millis(47)).contains(&dwell),
        "{dwell:?}"
    );
}

#[test]
fn fps_and_preset_changes_never_reconfigure_the_encoder() {
    let mut h = Harness::new(true, None);
    h.run(Duration::from_millis(100), true);
    let generation = h.state().record.generation;
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
    assert_eq!(h.state().record.generation, generation);

    // A display change rebuilds once, with an IDR and a new generation.
    h.fake.width.store(20, Ordering::SeqCst);
    let before = h.captures().len();
    h.run(Duration::from_millis(300), true);
    assert_eq!(h.fake.log.lock().configured, 2);
    let rebuilt = &h.captures()[before..];
    assert!(rebuilt[0].1);
    assert!(rebuilt[1..].iter().all(|(_, keyframe, _)| !keyframe));
    assert!(h.state().record.generation > generation);
    assert_eq!(
        h.state().record.display.map(|display| display.width),
        Some(20)
    );
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
    let generation = h.state().record.generation;
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
    let failing = h.state().record;
    assert_eq!(failing.state, SessionState::CaptureUnavailable);
    assert_eq!(failing.generation, generation + 1);
    assert!(!h.published.contains(&SessionState::Connecting));

    h.fake.fail_capture.store(false, Ordering::SeqCst);
    h.run(Duration::from_millis(300), true);
    assert_eq!(h.state().record.state, SessionState::Streaming);
}

#[tokio::test]
async fn unavailable_backend_reports_failure_and_stops() {
    let shared = session(false);
    // A backend that cannot be built describes nothing, and says so at once
    // rather than when the host stops waiting.
    let mut prepared = prepare(|| Err::<Fake, _>(error("desktop is locked")), true);
    let asked = Instant::now();
    let described = tokio::time::timeout(Duration::from_secs(5), &mut prepared.description).await;
    assert!(matches!(described, Ok(Err(_))));
    assert!(asked.elapsed() < Duration::from_secs(2));
    let mut worker = prepared.start(shared.clone(), config(None));
    let exit = tokio::time::timeout(Duration::from_secs(2), worker.exited())
        .await
        .unwrap();
    assert!(matches!(exit, WorkerExit::CaptureFailed(message) if message.contains("locked")));
    let state = shared.state.borrow().record.clone();
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
    // Between context checks, only the injection boundary reads the desktop.
    let reads = h.fake.log.lock().contexts;
    h.input(MOVE);
    h.step();
    assert_eq!(h.inputs(), 2);
    assert_eq!(h.fake.log.lock().contexts, reads + 1);
    // A repeated sequence, and a frame newer than the presented one.
    h.sequence -= 1;
    h.input(MOVE);
    h.presented.1 += 1;
    h.input(MOVE);
    h.presented.1 -= 1;
    h.step();
    assert_eq!(h.inputs(), 2);

    // A different desktop at the same position invalidates input too. The
    // boundary sees it before any check is due: the event is dropped, a new
    // generation starts, and control stays.
    h.fake.width.store(11, Ordering::SeqCst);
    let releases = h.fake.log.lock().releases;
    let generation = h.state().record.generation;
    h.input(MOVE);
    h.step();
    assert_eq!(h.inputs(), 2);
    assert!(h.fake.log.lock().releases > releases);
    assert!(h.state().record.generation > generation);
    assert!(h.state().record.controlling);

    // View-only: the host's revoke ends control at once.
    h.present();
    h.shared.policy.set(false);
    h.input(MOVE);
    h.step();
    assert!(!h.state().record.controlling);
    assert!(!h.state().record.control_allowed);
    assert_eq!(h.inputs(), 2);
}

#[test]
fn the_cursor_reports_every_change_but_the_controllers_own_move() {
    let mut h = Harness::new(true, None);
    let reported = |h: &Harness| {
        h.shared.cursor.borrow().map(|report| {
            let cursor = report.cursor;
            (report.serial, cursor.generation, cursor.x, cursor.y)
        })
    };
    // The last sequence C had taken when it sampled the last report.
    let taken = |h: &Harness| h.shared.cursor.borrow().unwrap().input_sequence;
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    h.present();
    // The first sample of a generation is reported.
    let (first, generation, ..) = reported(&h).unwrap();
    assert_eq!(reported(&h), Some((first, generation, 3, 3)));
    // The controller's own moves come back at each sample: none is reported.
    for x in 4..8 {
        h.input(Input::Move { x, y: 3 });
        h.run(Duration::from_millis(20), true);
    }
    assert_eq!(h.inputs(), 4);
    assert_eq!(reported(&h), Some((first, generation, 3, 3)));
    // A move C takes but does not apply, for a frame newer than the one
    // presented, counts as taken all the same.
    h.presented.1 += 1;
    h.input(MOVE);
    h.presented.1 -= 1;
    h.run(Duration::from_millis(20), true);
    assert_eq!(h.inputs(), 4);
    // Someone else moves it: reported at the next sample, without a hold;
    // and back to where the controller left it, which is their move too.
    for (x, serial) in [(1, first + 1), (7, first + 2)] {
        *h.fake.pointer.lock() = (x, 3);
        h.run(Duration::from_millis(20), true);
        assert_eq!(reported(&h), Some((serial, generation, x, 3)));
        assert_eq!(taken(&h), h.sequence);
    }
    // A move the OS stops short is reported where the pointer stopped, though
    // the pointer did not move since the last sample; and after each later
    // move it stops there too, at that move's sequence, since the viewer sets
    // aside a report older than its last move.
    *h.fake.clip.lock() = Some(8);
    for x in [8, 9] {
        h.input(Input::Move { x, y: 3 });
        h.run(Duration::from_millis(20), true);
    }
    assert_eq!(reported(&h), Some((first + 3, generation, 8, 3)));
    h.input(Input::Move { x: 9, y: 3 });
    h.run(Duration::from_millis(20), true);
    assert_eq!(reported(&h), Some((first + 4, generation, 8, 3)));
    assert_eq!(taken(&h), h.sequence);
    // Hiding is reported, and the controller's next move, which shows the
    // pointer again, is still its own.
    h.fake.hidden.store(true, Ordering::SeqCst);
    h.run(Duration::from_millis(20), true);
    assert_eq!(reported(&h), Some((first + 5, generation, 0, 0)));
    h.fake.hidden.store(false, Ordering::SeqCst);
    h.input(Input::Move { x: 6, y: 3 });
    h.run(Duration::from_millis(20), true);
    assert_eq!(reported(&h), Some((first + 5, generation, 0, 0)));
    // A new generation reports where the pointer is, though it did not move.
    h.input(Input::Move { x: 5, y: 5 });
    h.run(Duration::from_millis(20), true);
    assert_eq!(reported(&h), Some((first + 5, generation, 0, 0)));
    h.fake.width.store(11, Ordering::SeqCst);
    h.run(Duration::from_millis(300), true);
    let (serial, moved, x, y) = reported(&h).unwrap();
    assert!(serial > first + 5 && moved > generation);
    assert_eq!((x, y), (5, 5));
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
    let failed = h.state().record;
    assert!(!failed.controlling);
    assert_eq!(failed.reason, Some(StateReason::InputFailed));
    assert!(failed.message.starts_with("input_failed"));
    // New video does not hide why control ended, and a queued input is
    // never replayed.
    h.input(MOVE);
    h.run(Duration::from_millis(200), true);
    assert_eq!(h.state().record.reason, Some(StateReason::InputFailed));
    assert_eq!(h.state().record.message, failed.message);

    h.fake.fail_input.store(false, Ordering::SeqCst);
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    assert_eq!(h.state().record.reason, None);
    h.present();
    h.input(MOVE);
    h.step();
    assert!(h.state().record.controlling);
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
    assert_eq!(h.state().record.reason, Some(StateReason::ControlDenied));

    h.shared.policy.set(true);
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    assert_eq!(writers.load(Ordering::SeqCst), 1);
    // A repeated grant keeps the lease and the generation.
    let generation = h.state().record.generation;
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    assert_eq!(writers.load(Ordering::SeqCst), 1);
    assert_eq!(h.state().record.generation, generation);
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
    assert!(failed.record.controlling, "a failed lock held nothing down");
    let (code, message, serial) = failed.notice.unwrap();
    assert_eq!(code, NoticeCode::LockFailed);
    assert!(message.contains("rejected"));
    // The notice shows for 5 s.
    h.run(Duration::from_millis(4900), true);
    assert_eq!(h.state().notice.map(|notice| notice.2), Some(serial));
    h.run(Duration::from_millis(200), true);
    assert!(h.state().notice.is_none());
    assert!(h.state().record.controlling);

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
        if !h.state().record.controlling {
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
    assert!(!h.state().record.controlling);

    let injected = hold(&mut h);
    let (generation, now) = (h.state().record.generation, h.now());
    assert!(!h.shared.auth.lock().on_presented(generation, u64::MAX, now));
    h.run(ms(700), true);
    assert_eq!(h.inputs(), injected);
}

/// A viewer's display mode command is refused with a notice while it does
/// not control, before the backend is asked; the owner's restore needs no
/// control, and with nothing changed it says nothing.
#[test]
fn a_display_mode_change_needs_control_but_the_owners_restore_does_not() {
    let mut h = Harness::new(true, None);
    h.present();
    h.send(LaneCommand::DisplayMode(ModeCommand::Set(crate::Mode {
        width: 10,
        height: 10,
    })));
    h.step();
    let (code, _, serial) = h.state().notice.unwrap();
    assert_eq!(code, NoticeCode::DisplayModeFailed);
    h.send(LaneCommand::DisplayMode(ModeCommand::HostRestore));
    h.step();
    assert_eq!(h.state().notice.map(|notice| notice.2), Some(serial));
}
