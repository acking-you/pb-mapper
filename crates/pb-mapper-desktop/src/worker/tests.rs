use super::*;
use crate::{
    CursorPosition, DRAIN_POLL, Display, EncodedFrame, Input, InputEvent, KeyframeReason,
    VideoFormat, VideoRate,
    codec::{
        present_waiter::{FRESH_FOR, Idle, Status},
        refine::Repeats,
    },
    error,
    idr::IdrReason,
    lane::LANE_CAPACITY,
    modes::{Journal, ModeCommand},
    native::PlatformBackend,
    outgoing::HostUnit,
    platform::synthetic::{Probe, Synthetic},
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
    /// Display mode restores asked for.
    restores: u32,
    /// Display mode changes asked for.
    modes_set: Vec<crate::Mode>,
    /// Desktop reads, by context checks, the cursor and the injection boundary.
    contexts: u32,
    /// Each pause and resume the backend heard of, with the capture calls
    /// made before it.
    paused: Vec<(bool, usize)>,
}

#[derive(Clone)]
struct Fake {
    log: Arc<Mutex<Log>>,
    clock: Time,
    width: Arc<AtomicU32>,
    fail_input: Arc<AtomicBool>,
    fail_move: Arc<AtomicBool>,
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
    /// The display's modes; `None` offers none.
    modes: Arc<Mutex<Option<crate::ModeList>>>,
    /// A shape the pointer took under a new handle, until the next read.
    shape: Arc<Mutex<Option<crate::CursorShape>>>,
    /// How often thread C asked for the shape.
    shape_reads: Arc<AtomicU32>,
    /// A push capture source stays idle until this instant or a callback.
    idle_until: Arc<Mutex<Option<Instant>>>,
    /// A pull source on a still desktop: a capture submits nothing unless
    /// it carries a keyframe, and the source names no time to try again.
    still: Arc<AtomicBool>,
    /// A capture thread that thread C arms, as the Windows encoder's.
    armed: Arc<Mutex<Option<Armed>>>,
    /// Each unit repeats an unchanged picture, as the Windows encoder's
    /// liveness frame does.
    duplicate: Arc<AtomicBool>,
    /// An encoder that asks for input only once it has output its last
    /// unit, as some Windows MFTs do: a capture that finds a unit still
    /// inside after draining submits nothing and leaves the tick armed
    /// (`Submit::Waiting`).
    serial: Arc<AtomicBool>,
    /// Its output wakes thread C (`drain_poll` is `None`) while its capture
    /// is still polled, as the Windows encoder's was with its event watcher
    /// before its capture thread; while a serial encoder waits for input it
    /// names a retry `DRAIN_POLL` after the capture that found it waiting,
    /// and asks for input (the wake) once its unit is out.
    push_drain: Arc<AtomicBool>,
    /// When the last capture found a serial encoder waiting for input.
    waiting: Arc<Mutex<Option<Instant>>>,
    /// The next capture that finds a serial encoder waiting has its unit
    /// finish, and the encoder ask for input, right after its drain: the
    /// request lands before thread C chooses when to ask again.
    asks_late: Arc<AtomicBool>,
    repeatable: bool,
    submitted_video: bool,
}
impl Fake {
    fn new(clock: Time) -> Self {
        Self {
            log: Arc::default(),
            clock,
            width: Arc::new(AtomicU32::new(10)),
            fail_input: Arc::default(),
            fail_move: Arc::default(),
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
            modes: Arc::default(),
            shape: Arc::default(),
            shape_reads: Arc::default(),
            idle_until: Arc::default(),
            still: Arc::default(),
            armed: Arc::default(),
            duplicate: Arc::default(),
            serial: Arc::default(),
            push_drain: Arc::default(),
            waiting: Arc::default(),
            asks_late: Arc::default(),
            repeatable: false,
            submitted_video: true,
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
        self.encoding.lock().clear();
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
    fn cursor_shape(&mut self, _: &DesktopContext) -> Result<Option<crate::CursorShape>> {
        self.shape_reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.shape.lock().take())
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
        if let Some(armed) = self.armed.lock().as_mut() {
            return Ok(armed.submit(started, keyframe, codec, &mut self.submitted_video));
        }
        // As the Windows encoder: drain what is finished, then submit, which
        // a serial encoder still holding a unit refuses.
        let mut drained = None;
        if self.serial.load(Ordering::SeqCst) {
            drained = self.finished();
            let holding = !self.encoding.lock().is_empty();
            *self.waiting.lock() = holding.then_some(started);
            if holding {
                if self.asks_late.swap(false, Ordering::SeqCst)
                    && let Some(unit) = self.encoding.lock().front_mut()
                {
                    unit.1 = started;
                }
                self.submitted_video = false;
                return Ok(drained);
            }
        }
        let idle = self.idle_until.lock().is_some_and(|until| until > started)
            || self.still.load(Ordering::SeqCst);
        self.submitted_video = !idle || (self.repeatable && keyframe);
        if !self.submitted_video {
            return Ok(self.finished());
        }
        self.repeatable = true;
        let frame = EncodedFrame {
            format: VideoFormat {
                codec,
                width: 16,
                height: 10,
                keyframe,
                duplicate: self.duplicate.load(Ordering::SeqCst),
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
        let finished = if drained.is_some() {
            drained
        } else if self.discard.load(Ordering::SeqCst) && calls % 2 == 0 {
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
    fn submitted_video(&self) -> bool {
        self.submitted_video
    }
    fn drain_poll(&self) -> Option<Duration> {
        (!self.push_drain.load(Ordering::SeqCst)).then_some(DRAIN_POLL)
    }
    fn capture_poll(&self) -> Option<Duration> {
        self.armed.lock().is_none().then_some(DRAIN_POLL)
    }
    fn capture_idle_until(&self, keyframe_due: Option<Instant>) -> Option<Instant> {
        if let Some(armed) = self.armed.lock().as_ref() {
            return armed.rule().until((self.clock)(), keyframe_due);
        }
        // As the Windows encoder: a pushing serial encoder waits for its
        // input request, which comes with its unit's output, and is asked
        // again `DRAIN_POLL` later meanwhile, and at once once it asked.
        if self.push_drain.load(Ordering::SeqCst)
            && let Some(waiting) = *self.waiting.lock()
        {
            let now = (self.clock)();
            let asked = self
                .encoding
                .lock()
                .front()
                .is_none_or(|(_, ready, _)| *ready <= now);
            return Some(if asked { waiting } else { waiting + DRAIN_POLL });
        }
        // The first call opens the push source and learns it has no picture.
        let until = self
            .idle_until
            .lock()
            .filter(|_| !self.log.lock().captures.is_empty())?;
        Some(if self.repeatable {
            keyframe_due.map_or(until, |due| due.min(until))
        } else {
            until
        })
    }
    fn input(&mut self, event: &Input) -> Result<()> {
        if self.fail_input.load(Ordering::SeqCst)
            || matches!(event, Input::Move { .. }) && self.fail_move.load(Ordering::SeqCst)
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
    fn video_paused(&mut self, paused: bool) {
        let mut log = self.log.lock();
        let captures = log.captures.len();
        log.paused.push((paused, captures));
    }
    fn double_click_guard(&mut self) -> Option<crate::DoubleClickGuard> {
        *self.guard.lock()
    }
    fn display_modes(&mut self, _: &DesktopContext) -> Result<Option<crate::ModeList>> {
        Ok(self.modes.lock().clone())
    }
    fn set_display_mode(&mut self, _: &DesktopContext, mode: crate::Mode) -> Result<()> {
        self.log.lock().modes_set.push(mode);
        Ok(())
    }
    fn restore_display_mode(&mut self) -> Result<bool> {
        self.log.lock().restores += 1;
        Ok(false)
    }
}

/// A capture thread that thread C arms, as the Windows encoder's, which
/// follows the encoder's rule (`present_waiter::Idle`): what it published
/// and how far it looked, and what each capture call did. Its look answers
/// at once, and each unit, 8 bytes, comes out in the call that submits it.
struct Armed {
    status: Status,
    /// When the picture that waits was published.
    published_at: Option<Instant>,
    created: Instant,
    last_submit: Option<Instant>,
    keyframe: bool,
    /// When the encoder repeats the last picture on its own: liveness only,
    /// unless a test gives it bursts.
    repeats: Repeats,
    pts: u64,
    /// Each capture call: when, and whether it armed the thread or sent a
    /// new picture or a repeat.
    calls: Vec<(Instant, Call)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Arm,
    Fresh,
    /// A keyframe, or the liveness repeat: it spends the tick.
    Repeat,
    /// A refinement repeat, which leaves the tick and the thread armed.
    Refine,
}
impl Armed {
    fn new(created: Instant) -> Self {
        Self {
            status: Status::default(),
            published_at: None,
            created,
            last_submit: None,
            keyframe: false,
            repeats: Repeats::default(),
            pts: 0,
            calls: Vec::new(),
        }
    }

    fn rule(&self) -> Idle {
        Idle {
            waiting: None,
            status: self.status,
            created: self.created,
            last_submit: self.last_submit,
            keyframe: self.keyframe,
            look: Self::LOOK,
            repeats: self.repeats,
        }
    }
    const LOOK: Duration = Duration::from_millis(32);

    /// The thread converted a present and published it at `at`, which
    /// disarms it.
    fn publish(&mut self, at: Instant) {
        self.status = Status {
            published: true,
            held: self.status.held,
            ..Status::default()
        };
        self.published_at = Some(at);
    }

    /// As the encoder's submit: the published picture, unless it waited
    /// `FRESH_FOR`, when it is held back while the thread looks past it,
    /// and goes once the look answered; else a due repeat once the thread
    /// found nothing newer, a refinement repeat leaving it armed; else arm
    /// the thread.
    fn submit(
        &mut self,
        now: Instant,
        keyframe: bool,
        codec: VideoCodec,
        submitted: &mut bool,
    ) -> Option<EncodedFrame> {
        self.keyframe |= keyframe;
        let waited = self
            .published_at
            .is_some_and(|at| now.saturating_duration_since(at) >= FRESH_FOR);
        if self.status.published && !self.status.held && waited {
            self.status.published = false;
            self.status.held = true;
        }
        let call = if self.status.published
            || (self.status.held && self.status.answered(now, Self::LOOK))
        {
            Call::Fresh
        } else if self.rule().repeat_now(now) {
            if self.rule().refines(now) {
                Call::Refine
            } else {
                Call::Repeat
            }
        } else {
            Call::Arm
        };
        self.calls.push((now, call));
        *submitted = !matches!(call, Call::Arm | Call::Refine);
        if call == Call::Arm {
            self.status.armed.get_or_insert(now);
            self.status.idle = true;
            return None;
        }
        if call != Call::Refine {
            self.status = Status::default();
            self.published_at = None;
        }
        self.last_submit = Some(now);
        self.pts += 1;
        let keyframe = std::mem::take(&mut self.keyframe);
        let duplicate = call != Call::Fresh;
        match call {
            Call::Refine => self.repeats.refined(now, self.pts),
            _ if !duplicate || keyframe => self.repeats.fresh(now),
            _ => self.repeats.liveness(),
        }
        let bytes = vec![1; 8];
        self.repeats
            .output(self.pts, duplicate, keyframe, bytes.len(), None);
        Some(EncodedFrame {
            format: VideoFormat {
                codec,
                width: 16,
                height: 10,
                keyframe,
                duplicate,
                gop: u32::MAX,
                ..VideoFormat::default()
            },
            bytes,
        })
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
        cursor_shapes: false,
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
        Self::with(allowed, config(admission))
    }
    fn with(allowed: bool, config: WorkerConfig) -> Self {
        let now = Arc::new(Mutex::new(Instant::now()));
        let time = now.clone();
        let clock: Time = Arc::new(move || *time.lock());
        let fake = Fake::new(clock.clone());
        let shared = session(allowed);
        let engine = Engine::start(&shared, fake.clone(), config, Manual(now.clone())).unwrap();
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
    fn join_watcher(&self, max_fps: u32) -> Arc<SessionShared> {
        let room = &self.engine.room;
        let slot = PeerSlot::new(1).unwrap();
        let shared = Arc::new(SessionShared::new_in_room(
            slot,
            room.wake.clone(),
            room.peer_cursors.clone(),
            room.policy.clone(),
            room.cancel.child_token(),
            Targets::new(max_fps, 4_000_000),
        ));
        room.set_slot(
            slot,
            2,
            RoomSlotConfig {
                shared: shared.clone(),
                color: PeerColor::new(slot.index()).unwrap(),
                limits: VideoLimits { max_fps, ..LIMITS },
                cursor_shapes: false,
                peer_cursors: true,
            },
        );
        shared
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
    /// Iterate at each deadline until thread C makes one more capture call;
    /// when that call started.
    fn next_capture(&mut self) -> Instant {
        let calls = self.fake.log.lock().captures.len();
        for _ in 0..1000 {
            let deadline = self.step();
            if let Some(&(started, ..)) = self.fake.log.lock().captures.get(calls) {
                return started;
            }
            let now = self.now();
            *self.now.lock() = deadline.max(now);
        }
        panic!("thread C made no capture call");
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
    let capture = Duration::from_millis(30);
    fake.delay = capture;
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
    // before the next capture starts: sooner than another capture would take,
    // however busy the machine is otherwise.
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
        assert!(injected[0] - finished < capture);
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
fn shared_capture_obeys_the_active_watchers_decoder_cap() {
    let mut h = Harness::new(false, None);
    let watcher = h.join_watcher(30);
    watcher.targets.pace_fps.store(15, Ordering::Release);
    h.run(Duration::from_millis(200), true);
    let captures = h.captures();
    assert!(captures.len() >= 3);
    assert!(captures.iter().all(|(_, _, rate)| rate.pace_fps == 15));
    assert!(
        spacing(&captures)
            .iter()
            .all(|gap| *gap >= Duration::from_millis(66))
    );
    assert_eq!(h.engine.limits.max_fps, 30);

    watcher.targets.paused.store(true, Ordering::Release);
    h.run(Duration::from_millis(100), true);
    assert_eq!(h.captures().last().unwrap().2.pace_fps, 60);
    assert_eq!(h.engine.limits, LIMITS);
}

#[test]
fn watcher_replaces_the_host_pointer_while_a_controller_moves_it() {
    let mut h = Harness::new(true, None);
    let watcher = h.join_watcher(60);
    let cursor = || watcher.cursor.borrow().as_ref().unwrap().cursor;
    h.send(LaneCommand::Control { enabled: true });
    h.present();
    assert!(cursor().visible);

    h.input(Input::Move { x: 6, y: 5 });
    h.run(Duration::from_millis(20), true);
    let peer = watcher.peer_cursors.borrow().entries[0];
    assert!(peer.visible);
    assert_eq!((peer.x, peer.y), (6, 5));
    assert!(!cursor().visible);

    *h.fake.pointer.lock() = (2, 1);
    h.run(Duration::from_millis(20), true);
    assert!(cursor().visible);
    assert_eq!((cursor().x, cursor().y), (2, 1));

    // The host moved away. If re-aiming fails, a click cannot land there.
    h.fake.fail_move.store(true, Ordering::Release);
    let before = h.inputs();
    h.input(Input::Button {
        button: crate::Button::Left,
        down: true,
        drag_start: false,
    });
    h.step();
    assert_eq!(h.inputs(), before);
    assert_eq!(h.state().record.reason, Some(StateReason::InputFailed));
}

#[test]
fn switching_codec_discards_inflight_output_before_the_next_drain() {
    let mut h = Harness::new(false, None);
    *h.fake.latency.lock() = Duration::from_millis(5);
    h.step();
    assert_eq!(h.fake.pending_video(), 1);
    h.engine.room.set_codec(VideoCodec::Jpeg);
    *h.now.lock() += Duration::from_millis(6);
    h.step();
    assert_eq!(h.fake.pending_video(), 0);
    assert!(h.sent().is_empty());
    h.run(Duration::from_millis(40), false);
    let frames = h.sent();
    assert!(frames[0].keyframe);
    assert!(frames.iter().all(|unit| unit.codec == VideoCodec::Jpeg));
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
        h.shared.cursor.borrow().as_ref().map(|report| {
            let cursor = report.cursor;
            (report.serial, cursor.generation, cursor.x, cursor.y)
        })
    };
    // The last sequence C had taken when it sampled the last report.
    let taken = |h: &Harness| h.shared.cursor.borrow().as_ref().unwrap().input_sequence;
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

/// The shape is no position: a new one is reported where the controller's
/// own move left the pointer, which the echo rule would otherwise hold back,
/// and an equal one read under a new handle is no change. A session whose
/// viewer reads no shapes never asks the backend for one.
#[test]
fn a_new_shape_is_reported_even_at_the_controllers_own_move() {
    let ibeam =
        crate::CursorShape::new(crate::CursorKind::Ibeam, 1, 2, (0, 1), vec![1; 8]).unwrap();
    for shapes in [false, true] {
        let mut h = Harness::with(
            true,
            WorkerConfig {
                cursor_shapes: shapes,
                ..config(None)
            },
        );
        let report = |h: &Harness| h.shared.cursor.borrow().clone().unwrap();
        h.send(LaneCommand::Control { enabled: true });
        h.step();
        h.present();
        h.input(Input::Move { x: 4, y: 3 });
        h.run(Duration::from_millis(20), true);
        let before = report(&h);
        *h.fake.shape.lock() = Some(ibeam.clone());
        h.run(Duration::from_millis(20), true);
        let after = report(&h);
        if !shapes {
            assert_eq!(after.serial, before.serial);
            assert_eq!(h.fake.shape_reads.load(Ordering::SeqCst), 0);
            continue;
        }
        assert_eq!(after.serial, before.serial + 1);
        assert_eq!((after.cursor.x, after.cursor.y), (4, 3));
        assert_eq!(after.shape.as_ref(), Some(&ibeam));
        // The same image under another handle.
        *h.fake.shape.lock() = Some(ibeam.clone());
        h.run(Duration::from_millis(20), true);
        assert_eq!(report(&h).serial, after.serial);
    }
}

/// While the pointer is hidden, or on a display the viewer does not see
/// (both read as `confine` gives them: hidden at 0, 0), its shape is not
/// read: nothing of it reaches the viewer, which keeps the last shape. Back
/// on the shared display, the shape it took meanwhile is reported.
#[test]
fn no_shape_is_read_while_the_pointer_is_away() {
    let ibeam =
        crate::CursorShape::new(crate::CursorKind::Ibeam, 1, 2, (0, 1), vec![1; 8]).unwrap();
    let mut h = Harness::with(
        true,
        WorkerConfig {
            cursor_shapes: true,
            ..config(None)
        },
    );
    let report = |h: &Harness| h.shared.cursor.borrow().clone().unwrap();
    h.step();
    h.present();
    let reads = h.fake.shape_reads.load(Ordering::SeqCst);
    assert!(reads > 0);
    h.fake.hidden.store(true, Ordering::SeqCst);
    *h.fake.shape.lock() = Some(ibeam.clone());
    h.run(Duration::from_millis(100), true);
    let away = report(&h);
    assert!(!away.cursor.visible && away.shape.is_none());
    assert_eq!(h.fake.shape_reads.load(Ordering::SeqCst), reads);
    h.fake.hidden.store(false, Ordering::SeqCst);
    h.run(Duration::from_millis(20), true);
    let back = report(&h);
    assert!(back.cursor.visible);
    assert_eq!(back.shape.as_ref(), Some(&ibeam));
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
    let peer = h.join_watcher(60);
    assert!(peer.lane.push(LaneCommand::Control { enabled: true }));
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    h.present();

    // A peer clicks at t0. The drag uses the same system pointer, even
    // though its commands arrive in a different lane.
    let (generation, frame_id) = h.presented;
    assert!(peer.auth.lock().on_presented(generation, frame_id, h.now()));
    for (seq, input) in [MOVE, left(true, false), left(false, false)]
        .into_iter()
        .enumerate()
    {
        assert!(peer.lane.push(LaneCommand::Input(InputEvent {
            generation,
            frame_id,
            sequence: seq as u64 + 1,
            input,
        })));
    }
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
    h.engine.room.request_restore();
    h.step();
    assert_eq!(h.state().notice.map(|notice| notice.2), Some(serial));
}

/// Owner restores coalesce independently of a full viewer input lane.
#[test]
fn an_overflowing_lane_still_runs_the_owners_restore() {
    let mut h = Harness::new(true, None);
    h.present();
    h.engine.room.request_restore();
    while h.shared.lane.push(LaneCommand::Release) {}
    h.engine.room.request_restore();
    h.step();
    assert_eq!(h.fake.log.lock().restores, 1);
    assert_eq!(h.state().record.reason, Some(StateReason::InputCongestion));
}

/// An overflow drain checks a viewer's display mode change against control
/// as the lane leaves it, as a drain does: one after the viewer's own
/// release or pause is refused with a notice, never run.
#[test]
fn an_overflow_drain_refuses_a_mode_change_after_the_viewers_release() {
    let shown = crate::Mode {
        width: 10,
        height: 10,
    };
    let asked = crate::Mode {
        width: 8,
        height: 8,
    };
    let mut h = Harness::new(true, None);
    *h.fake.modes.lock() = Some(crate::ModeList {
        current: shown,
        original: shown,
        supported: vec![shown, asked],
    });
    h.present();
    for release in [
        LaneCommand::Control { enabled: false },
        LaneCommand::EndControl,
    ] {
        h.send(LaneCommand::Control { enabled: true });
        h.step();
        assert!(h.state().record.controlling);
        let before = h.state().notice.map(|notice| notice.2);
        // A drain ran what the overflowing lane held; the next overflow
        // drain takes only what came since.
        while h.shared.lane.push(LaneCommand::Release) {}
        h.shared.lane.drain_into(&mut VecDeque::new());
        h.send(release);
        h.send(LaneCommand::DisplayMode(ModeCommand::Set(asked)));
        h.step();
        assert_eq!(h.fake.log.lock().modes_set, []);
        let (code, _, serial) = h.state().notice.unwrap();
        assert_eq!(code, NoticeCode::DisplayModeFailed);
        assert_ne!(Some(serial), before);
        assert_eq!(h.state().record.reason, Some(StateReason::InputCongestion));
    }
}

#[test]
fn an_idle_capture_drains_old_output_and_keeps_the_keyframe_tick_armed() {
    let mut h = Harness::new(true, None);
    let start = h.now();
    *h.fake.idle_until.lock() = Some(start + Duration::from_millis(250));
    // Model two async results ready before one worker wake. The submit finds
    // no fresh picture but can collect the first result.
    for keyframe in [true, false] {
        h.fake.encoding.lock().push_back((
            start,
            start,
            EncodedFrame {
                format: VideoFormat {
                    codec: VideoCodec::H264,
                    keyframe,
                    ..Default::default()
                },
                bytes: vec![1],
            },
        ));
    }

    assert_eq!(
        h.step(),
        start,
        "schedule the rest of the ready output immediately"
    );
    h.step();
    assert_eq!(h.sent().len(), 2);
    h.run(Duration::from_millis(5), true);

    // A callback 5 ms into the first tick submits immediately and carries the IDR.
    *h.fake.idle_until.lock() = None;
    h.step();
    assert_eq!(h.captures().len(), 2);
    assert!(h.sent()[0].keyframe);
}

#[test]
fn idle_capture_waits_through_cursor_wakes_but_repeats_for_a_deferred_keyframe() {
    let mut h = Harness::new(true, None);
    let start = h.now();
    h.step();
    assert!(h.sent()[0].keyframe);
    *h.fake.idle_until.lock() = Some(start + Duration::from_secs(1));
    h.shared
        .idr
        .lock()
        .request(IdrReason::Viewer(KeyframeReason::Gap));

    let interval = crate::idr::HOST_IDR_MIN_INTERVAL;
    h.run(interval - Duration::from_millis(1), true);
    assert_eq!(h.captures().len(), 1, "cursor wakes leave the capture idle");
    h.run(Duration::from_millis(1), false);
    assert_eq!(h.captures()[1].0, start + interval);
    assert!(h.sent()[0].keyframe);
}

/// The captures that started at `at` or later.
fn captures_since(h: &Harness, at: Instant) -> Vec<(Instant, bool, VideoRate)> {
    h.captures()
        .into_iter()
        .filter(|(started, ..)| *started >= at)
        .collect()
}

#[test]
fn a_pull_source_with_nothing_new_is_polled_while_active_and_once_a_period_while_still() {
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    h.step();
    assert!(h.sent()[0].keyframe);
    // A still desktop on a source that cannot wake thread C and names no time
    // to try again, as DXGI. Right after a new picture the desktop is active:
    // the armed tick is asked again every drain interval, never at once,
    // which would spin thread C.
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(HOT_WINDOW, true);
    let active = h.captures();
    assert!(active.len() > 100, "{} captures", active.len());
    let gaps = spacing(&active[1..]);
    assert!(gaps.iter().all(|gap| *gap == DRAIN_POLL), "{gaps:?}");

    // Still since: asked once a period, as often as the grid would ask.
    h.run(Duration::from_secs(1), true);
    let still = h.captures().split_off(active.len());
    assert!((60..=61).contains(&still.len()), "{} captures", still.len());
    let gaps = spacing(&still);
    assert!(gaps.iter().all(|gap| *gap == period), "{gaps:?}");

    // A repeat of the unchanged picture, as the encoder's liveness frame, is
    // no sign of activity: after the pacer's floor, a period apart again.
    h.fake.duplicate.store(true, Ordering::SeqCst);
    h.fake.still.store(false, Ordering::SeqCst);
    let repeated = h.next_capture();
    h.fake.still.store(true, Ordering::SeqCst);
    h.fake.duplicate.store(false, Ordering::SeqCst);
    h.run(Duration::from_millis(100), true);
    let after = captures_since(&h, repeated);
    assert_eq!(after[1].0 - repeated, period * 3 / 4);
    let gaps = spacing(&after[1..]);
    assert!(gaps.iter().all(|gap| *gap == period), "{gaps:?}");

    // A new picture is: every drain interval again after the floor.
    h.fake.still.store(false, Ordering::SeqCst);
    let changed = h.next_capture();
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(Duration::from_millis(50), true);
    let after = captures_since(&h, changed);
    assert_eq!(after[1].0 - changed, period * 3 / 4);
    let gaps = spacing(&after[1..]);
    assert!(gaps.iter().all(|gap| *gap == DRAIN_POLL), "{gaps:?}");

    // While active the tick stays armed: the next picture goes out at the
    // next poll, rather than at the next tick.
    let moved = h.now();
    h.fake.still.store(false, Ordering::SeqCst);
    let taken = h.next_capture();
    assert!(taken - moved <= DRAIN_POLL, "{:?}", taken - moved);
    assert_eq!(h.sent().len(), 1);
}

#[test]
fn a_faster_pace_brings_a_still_desktops_poll_in() {
    let mut h = Harness::new(true, None);
    h.shared.targets.pace_fps.store(15, Ordering::Release);
    h.step();
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(HOT_WINDOW * 2, true);
    // Still at 15 fps, the next poll is a 15 fps period away. At 60 fps the
    // armed tick is asked at once, then a 60 fps period apart.
    let polled = h.next_capture();
    *h.now.lock() += Duration::from_millis(1);
    let raised = h.now();
    h.shared.targets.pace_fps.store(60, Ordering::Release);
    assert_eq!(h.next_capture(), raised);
    assert!(raised - polled < Duration::from_secs(1) / 15);
    let next = h.next_capture();
    assert_eq!(next - raised, Duration::from_secs(1) / 60);
}

#[test]
fn a_new_picture_collected_by_a_capture_with_nothing_new_makes_it_active() {
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    h.step();
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(HOT_WINDOW * 2, true);
    // A change, whose unit leaves the encoder just as the next tick comes:
    // that tick's capture finds nothing new, but collects it.
    *h.fake.latency.lock() = period * 3 / 4;
    h.fake.still.store(false, Ordering::SeqCst);
    let changed = h.next_capture();
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(Duration::from_millis(30), true);
    let after = captures_since(&h, changed);
    assert_eq!(after[1].0 - changed, period * 3 / 4);
    assert_eq!(after[2].0 - after[1].0, DRAIN_POLL);
}

#[test]
fn an_injected_input_brings_a_still_desktops_poll_in_and_keeps_it_active() {
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    h.send(LaneCommand::Control { enabled: true });
    h.step();
    h.present();
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(HOT_WINDOW * 2, true);
    let polled = h.next_capture();
    // An input a millisecond after a still desktop's poll: the next poll
    // comes a drain interval after the input, not a period after the last,
    // and they go on at that interval while the input's answer may appear.
    *h.now.lock() += Duration::from_millis(1);
    let injected = h.now();
    h.input(MOVE);
    h.run(HOT_WINDOW, true);
    assert_eq!(h.inputs(), 1);
    let active = captures_since(&h, injected);
    assert_eq!(active[0].0, injected + DRAIN_POLL);
    assert!(active[0].0 < polled + period);
    assert!(active.len() > 100, "{} captures", active.len());
    let gaps = spacing(&active);
    assert!(gaps.iter().all(|gap| *gap == DRAIN_POLL), "{gaps:?}");

    // Nothing came of it: once a period again.
    h.run(Duration::from_millis(200), true);
    let still = captures_since(&h, injected + HOT_WINDOW);
    assert!(still.len() >= 12, "{} captures", still.len());
    let gaps = spacing(&still);
    assert!(gaps.iter().all(|gap| *gap == period), "{gaps:?}");
}

/// The capture that submitted the unit after `first`'s, on a serial encoder
/// whose unit from `first` is ready at `ready`: the first capture that
/// started then or later.
fn next_submit_after(h: &Harness, ready: Instant) -> Instant {
    captures_since(h, ready)
        .first()
        .map(|(started, ..)| *started)
        .unwrap()
}

/// An encoder that asks for input only once it has output its last unit, as
/// some Windows MFTs do, taking a period and a half per unit. A tick that
/// finds it still holding one stays armed, and thread C asks again every
/// poll interval, also on a desktop that was still, so the next picture goes
/// in within a poll of the encoder freeing up rather than at the tick after.
#[test]
fn an_encoder_that_has_not_asked_for_input_is_asked_again_at_the_next_poll() {
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    h.step();
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(HOT_WINDOW * 2, true);
    let latency = period * 3 / 2;
    *h.fake.latency.lock() = latency;
    h.fake.serial.store(true, Ordering::SeqCst);
    h.fake.still.store(false, Ordering::SeqCst);
    let first = h.next_capture();
    h.run(period * 3, true);
    let armed = captures_since(&h, first + Duration::from_micros(1));
    assert!(
        armed[0].0 >= first + period * 3 / 4,
        "{:?}",
        armed[0].0 - first
    );
    let ready = first + latency;
    let taken = next_submit_after(&h, ready);
    assert!(taken - ready <= DRAIN_POLL, "{:?}", taken - ready);
    assert!(taken < first + period * 2, "{:?}", taken - first);
    // Each armed ask came a poll after the last.
    let waiting: Vec<_> = armed
        .iter()
        .take_while(|(started, ..)| *started < ready)
        .cloned()
        .collect();
    assert!(waiting.len() >= 3, "{} asks", waiting.len());
    let gaps = spacing(&waiting);
    assert!(gaps.iter().all(|gap| *gap == DRAIN_POLL), "{gaps:?}");
    // Waiting for input is no backpressure: no tick was skipped for it.
    assert_eq!(h.state().skipped_ticks, 0);
}

/// The same encoder, pushing its output (its event watcher wakes thread C)
/// while its capture is pulled: while it waits for input it names a retry a
/// poll later, and its request, which wakes thread C, is answered at once,
/// between polls, rather than at the next poll or tick.
#[test]
fn a_pushing_encoder_waiting_for_input_is_asked_as_its_request_wakes_thread_c() {
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    h.fake.push_drain.store(true, Ordering::SeqCst);
    h.step();
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(HOT_WINDOW * 2, true);
    // Ready off the poll grid, so the wake and a poll cannot coincide.
    let latency = period * 3 / 2 + Duration::from_micros(700);
    *h.fake.latency.lock() = latency;
    h.fake.serial.store(true, Ordering::SeqCst);
    h.fake.still.store(false, Ordering::SeqCst);
    let first = h.next_capture();
    let ready = first + latency;
    h.run(ready - h.now() - Duration::from_micros(1), true);
    let waiting = captures_since(&h, first + Duration::from_micros(1));
    assert!(waiting.len() >= 3, "{} asks", waiting.len());
    let gaps = spacing(&waiting);
    assert!(gaps.iter().all(|gap| *gap == DRAIN_POLL), "{gaps:?}");
    // The unit is out and the encoder asks for input: thread C is woken.
    *h.now.lock() = ready;
    h.step();
    assert_eq!(next_submit_after(&h, ready), ready);
    assert_eq!(h.fake.encoding.lock().len(), 1, "the next unit went in");
    assert_eq!(h.sent().len(), 1);
}

/// The same, with the request landing after the capture that found the
/// encoder waiting drained it, and before thread C chose when to ask again:
/// it is answered at once all the same, rather than at the next poll.
#[test]
fn an_input_request_that_lands_as_a_waiting_capture_returns_is_answered_at_once() {
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    h.fake.push_drain.store(true, Ordering::SeqCst);
    h.step();
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(HOT_WINDOW * 2, true);
    *h.fake.latency.lock() = period * 3;
    h.fake.serial.store(true, Ordering::SeqCst);
    h.fake.still.store(false, Ordering::SeqCst);
    let first = h.next_capture();
    h.run(period + DRAIN_POLL * 2, true);
    assert!(
        h.fake.waiting.lock().is_some(),
        "a capture found the encoder waiting for input"
    );
    h.fake.asks_late.store(true, Ordering::SeqCst);
    let waiting = h.next_capture();
    assert!(waiting > first + period);
    assert_eq!(h.fake.encoding.lock().len(), 1);
    assert_eq!(h.next_capture(), waiting, "answered at once");
    assert_eq!(h.fake.encoding.lock().len(), 1, "the next unit went in");
    assert_eq!(h.sent().len(), 1);
}

/// A source whose encoder pushes (`drain_poll` is `None`) while its capture
/// is pulled (`capture_poll` is `Some`), as the Windows encoder with its
/// event watcher: its armed ticks are still asked again every poll interval
/// while the desktop is active, and once a period while it is still.
#[test]
fn a_pushing_encoder_with_a_pulled_capture_still_polls_armed_ticks_while_active() {
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    h.fake.push_drain.store(true, Ordering::SeqCst);
    h.step();
    assert!(h.sent()[0].keyframe);
    h.fake.still.store(true, Ordering::SeqCst);
    h.run(HOT_WINDOW, true);
    let active = h.captures();
    assert!(active.len() > 100, "{} captures", active.len());
    let gaps = spacing(&active[1..]);
    assert!(gaps.iter().all(|gap| *gap == DRAIN_POLL), "{gaps:?}");
    h.run(Duration::from_secs(1), true);
    let still = h.captures().split_off(active.len());
    assert!((60..=61).contains(&still.len()), "{} captures", still.len());
    let gaps = spacing(&still);
    assert!(gaps.iter().all(|gap| *gap == period), "{gaps:?}");
}

/// A capture thread that thread C arms, as the Windows encoder's, never
/// polled: thread C's first tick arms it and takes the first picture as
/// the thread publishes it. On a still desktop thread C then calls only to
/// arm it again after each 250 ms repeat, never spinning; a present the
/// armed thread publishes is taken the moment it lands; and one published
/// right after a repeat waits for the next tick, which, as it waited, arms
/// the thread to look past it first and sends it once the look answered.
#[test]
fn an_armed_capture_thread_is_taken_as_it_publishes_and_never_polled() {
    use crate::codec::pipeline::LIVENESS_INTERVAL;
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    let start = h.now();
    *h.fake.armed.lock() = Some(Armed::new(start));
    let calls = |h: &Harness| {
        h.fake
            .armed
            .lock()
            .as_ref()
            .map(|armed| armed.calls.clone())
            .unwrap()
    };
    let last = |h: &Harness| calls(h).last().copied();
    let publish = |h: &Harness| h.fake.armed.lock().as_mut().unwrap().publish(h.now());

    // Armed by the first tick, until the first picture's deadline.
    assert!(h.step() > start);
    assert_eq!(calls(&h), [(start, Call::Arm)]);
    let first = start + Duration::from_millis(40);
    *h.now.lock() = first;
    publish(&h);
    h.step();
    assert_eq!(last(&h), Some((first, Call::Fresh)));
    assert!(h.sent()[0].keyframe);

    // Still: an arming after each repeat, the repeats 250 ms apart.
    h.run(Duration::from_secs(1), true);
    let spell: Vec<(Instant, Call)> = calls(&h)
        .into_iter()
        .filter(|(at, _)| *at > first)
        .collect();
    let kinds: Vec<Call> = spell.iter().map(|(_, call)| *call).collect();
    assert_eq!(kinds, [Call::Arm, Call::Repeat].repeat(4), "{spell:?}");
    let repeats: Vec<Instant> = spell
        .iter()
        .filter(|(_, call)| *call == Call::Repeat)
        .map(|(at, _)| *at)
        .collect();
    assert_eq!(repeats[0], first + LIVENESS_INTERVAL);
    assert!(
        repeats
            .windows(2)
            .all(|pair| pair[1] - pair[0] == LIVENESS_INTERVAL)
    );

    // Armed again after the last repeat: a present that lands between
    // thread C's wakes is taken at once.
    let armed = h.next_capture();
    assert_eq!(last(&h), Some((armed, Call::Arm)));
    let landed = armed + Duration::from_micros(30_003);
    *h.now.lock() = landed;
    publish(&h);
    h.step();
    assert_eq!(last(&h), Some((landed, Call::Fresh)));
    assert!(!h.sent()[0].duplicate);

    // A repeat spends the tick: a present published right after it waits
    // for the next, which looks past it before it sends it, at that tick.
    let armed = h.next_capture();
    assert_eq!(last(&h), Some((armed, Call::Arm)));
    let repeated = h.next_capture();
    assert_eq!(last(&h), Some((repeated, Call::Repeat)));
    assert_eq!(repeated, landed + LIVENESS_INTERVAL);
    let late = repeated + Duration::from_millis(1);
    *h.now.lock() = late;
    publish(&h);
    let taken = h.next_capture();
    assert!(
        taken > late && taken <= repeated + period,
        "{:?}",
        taken - repeated
    );
    assert_eq!(last(&h), Some((taken, Call::Arm)));
    assert_eq!(h.next_capture(), taken);
    assert_eq!(last(&h), Some((taken, Call::Fresh)));
    assert!(!h.sent().last().unwrap().duplicate);
}

/// An armed capture thread whose encoder refines a still picture, as the
/// Windows encoder does where its GPU earned it: once a picture whose unit
/// filled its budget has stood still `REFINE_AFTER`, thread C sends its
/// refinement repeats a period apart, each leaving the tick and the thread
/// armed, until two small outputs after `REFINE_MIN` end the burst, and the
/// liveness repeat follows 250 ms later. A present that lands right after a
/// refinement repeat goes at once, where one after a liveness repeat waits
/// for the next tick.
#[test]
fn a_refinement_burst_keeps_the_tick_armed_for_the_next_present() {
    use crate::codec::{
        pipeline::LIVENESS_INTERVAL,
        refine::{REFINE_AFTER, REFINE_MIN},
    };
    let mut h = Harness::new(true, None);
    let period = Duration::from_secs(1) / 60;
    let start = h.now();
    let mut armed = Armed::new(start);
    // Its 8-byte units fill an 8-byte budget (3 840 bit/s at a nominal
    // 60), and as repeats are under `REFINE_DONE_BYTES`.
    armed.repeats = Repeats::new(true, 3_840, 60, 60);
    *h.fake.armed.lock() = Some(armed);
    let calls = |h: &Harness| {
        h.fake
            .armed
            .lock()
            .as_ref()
            .map(|armed| armed.calls.clone())
            .unwrap()
    };
    let last = |h: &Harness| calls(h).last().copied();
    let publish = |h: &Harness| h.fake.armed.lock().as_mut().unwrap().publish(h.now());
    let after = |h: &Harness, from: Instant| -> Vec<(Instant, Call)> {
        calls(h).into_iter().filter(|(at, _)| *at > from).collect()
    };

    h.step();
    let first = start + Duration::from_millis(40);
    *h.now.lock() = first;
    publish(&h);
    h.step();
    assert_eq!(last(&h), Some((first, Call::Fresh)));
    h.run(Duration::from_secs(1), true);
    let spell = after(&h, first);
    let refined: Vec<Instant> = spell
        .iter()
        .filter(|(_, call)| *call == Call::Refine)
        .map(|(at, _)| *at)
        .collect();
    let expected: Vec<Instant> = (0..u32::from(REFINE_MIN))
        .map(|n| first + REFINE_AFTER + period * n)
        .collect();
    assert_eq!(refined, expected, "{spell:?}");
    // Armed once after the picture, then the burst, then liveness only.
    let kinds: Vec<Call> = spell.iter().map(|(_, call)| *call).take(6).collect();
    assert_eq!(
        kinds,
        [
            Call::Arm,
            Call::Refine,
            Call::Refine,
            Call::Refine,
            Call::Repeat,
            Call::Arm
        ],
        "{spell:?}"
    );
    let ended = *refined.last().unwrap();
    assert_eq!(spell[4].0, ended + LIVENESS_INTERVAL);

    // A new picture that fills its budget refines in turn; a present that
    // lands just after its first refinement repeat is taken at once.
    publish(&h);
    h.run(period * 3, true);
    let (second, _) = after(&h, ended)
        .into_iter()
        .find(|(_, call)| *call == Call::Fresh)
        .unwrap();
    let refine = second + REFINE_AFTER;
    let to = refine + Duration::from_millis(1);
    h.run(to - h.now(), true);
    assert_eq!(last(&h), Some((refine, Call::Refine)));
    publish(&h);
    h.step();
    assert_eq!(last(&h), Some((to, Call::Fresh)));
    assert!(!h.sent().last().unwrap().duplicate);
}

#[test]
fn the_backend_hears_of_a_pause_once_each_way_and_not_while_a_viewer_watches() {
    let mut h = Harness::new(true, None);
    let paused = |h: &Harness| h.fake.log.lock().paused.clone();
    h.run(Duration::from_millis(50), true);
    assert!(paused(&h).is_empty());
    // The only viewer pauses: once, however long; resuming says so before
    // the capture that resumes.
    h.shared.targets.paused.store(true, Ordering::Release);
    h.run(Duration::from_millis(200), true);
    let captured = h.captures().len();
    assert_eq!(paused(&h), [(true, captured)]);
    h.shared.targets.paused.store(false, Ordering::Release);
    h.run(Duration::from_millis(50), true);
    assert_eq!(paused(&h), [(true, captured), (false, captured)]);
    assert!(h.captures().len() > captured);

    // One of two viewers pausing pauses nothing; the second does.
    let watcher = h.join_watcher(60);
    h.run(Duration::from_millis(50), true);
    watcher.targets.paused.store(true, Ordering::Release);
    h.run(Duration::from_millis(100), true);
    assert_eq!(paused(&h).len(), 2);
    h.shared.targets.paused.store(true, Ordering::Release);
    h.run(Duration::from_millis(100), true);
    watcher.targets.paused.store(false, Ordering::Release);
    h.run(Duration::from_millis(100), true);
    let heard: Vec<bool> = paused(&h).iter().map(|(paused, _)| *paused).collect();
    assert_eq!(heard, [true, false, true, false]);
}

/// Thread C on the synthetic platform's own backend, on a clock the test
/// sets, with B-send taking every unit. A dated display's presents land as
/// the clock reaches them, each waking thread C, as ScreenCaptureKit's do.
struct Synth<const PUSH: bool> {
    shared: Arc<SessionShared>,
    engine: Engine<PlatformBackend<Synthetic<PUSH>>, Manual>,
    now: Arc<Mutex<Instant>>,
    probe: Probe,
    /// Viewers that joined, whose units B-send takes too.
    joined: Vec<Arc<SessionShared>>,
}
impl<const PUSH: bool> Synth<PUSH> {
    /// `dated`: a display presenting at this rate, its presents this far
    /// after the first tick's, give or take whole periods.
    fn new(dated: Option<(f64, Duration)>) -> Self {
        let start = Instant::now();
        let now = Arc::new(Mutex::new(start));
        let probe = Probe::default();
        if let Some((hz, phase)) = dated {
            let time = now.clone();
            let epoch = start.checked_sub(Duration::from_secs(1)).unwrap() + phase;
            probe.date(hz, epoch, Arc::new(move || *time.lock()));
        }
        let backend = PlatformBackend::<Synthetic<PUSH>>::open(
            Arc::new(probe.clone()),
            &Probe::display(),
            Journal::default(),
        )
        .unwrap();
        let shared = session(true);
        let engine = Engine::start(&shared, backend, config(None), Manual(now.clone())).unwrap();
        Self {
            shared,
            engine,
            now,
            probe,
            joined: Vec::new(),
        }
    }
    fn now(&self) -> Instant {
        *self.now.lock()
    }
    /// Another viewer, at `limits`.
    fn join(&mut self, limits: VideoLimits) -> Arc<SessionShared> {
        let room = &self.engine.room;
        let index = self.joined.len() + 1;
        let slot = PeerSlot::new(index).unwrap();
        let shared = Arc::new(SessionShared::new_in_room(
            slot,
            room.wake.clone(),
            room.peer_cursors.clone(),
            room.policy.clone(),
            room.cancel.child_token(),
            Targets::new(limits.max_fps, 4_000_000),
        ));
        room.set_slot(
            slot,
            index as u64 + 1,
            RoomSlotConfig {
                shared: shared.clone(),
                color: PeerColor::new(slot.index()).unwrap(),
                limits,
                cursor_shapes: false,
                peer_cursors: false,
            },
        );
        self.joined.push(shared.clone());
        shared
    }
    /// Iterate at every deadline and every present for `span`; the units
    /// B-send took.
    fn run(&mut self, span: Duration) -> Vec<HostUnit> {
        let end = self.now() + span;
        let mut sent = Vec::new();
        let mut spins = 0;
        loop {
            let now = self.now();
            self.probe.land(now);
            let deadline = match self.engine.iterate() {
                Step::Wait(deadline) => deadline,
                Step::Exit(exit) => panic!("thread C exited: {exit:?}"),
            };
            sent.extend(std::iter::from_fn(|| self.shared.outgoing.pop()));
            for joined in &self.joined {
                while joined.outgoing.pop().is_some() {}
            }
            let next = self
                .probe
                .next_present(now)
                .map_or(deadline, |present| present.min(deadline));
            if next > end {
                *self.now.lock() = end;
                return sent;
            }
            spins = if next > now { 0 } else { spins + 1 };
            assert!(spins < 1000, "thread C spins without waiting");
            *self.now.lock() = next.max(now);
        }
    }
    /// From each present to the capture that took it, after the first
    /// `settle` takes.
    fn waits(&self, settle: usize) -> Vec<Duration> {
        self.probe
            .takes()
            .iter()
            .skip(settle)
            .map(|(presented, taken)| *taken - *presented)
            .collect()
    }
}

/// A display presenting a little faster than the pace, or at the pace with
/// a clock 100 ppm fast, is captured as each present lands, whatever its
/// phase against the first tick once the timeline has crept onto it, and no
/// present is replaced before a capture takes it. On an even grid both
/// would wait for the tick, ever longer, until one was replaced.
#[test]
fn a_dated_display_at_the_pace_is_captured_as_it_presents() {
    let period = Duration::from_secs(1) / 60;
    for hz in [60.05, 60.0 * (1.0 + 100e-6)] {
        for phase in [period / 10, period / 2, period * 9 / 10] {
            let mut h = Synth::<true>::new(Some((hz, phase)));
            let sent = h.run(Duration::from_secs(10));
            assert!(sent.len() >= 599, "{hz} Hz: {} units", sent.len());
            let waits = h.waits(64);
            assert!(waits.len() >= 530, "{hz} Hz: {} takes", waits.len());
            assert!(
                waits.iter().all(|wait| wait.is_zero()),
                "{hz} Hz, phase {phase:?}: {:?}",
                waits.iter().max()
            );
            assert_eq!(h.probe.replaced(), 0, "{hz} Hz, phase {phase:?}");
        }
    }
}

/// A display at the pace whose presents land a millisecond before the grid's
/// ticks: from the second picture on, each is captured as it lands, where
/// the grid made each wait the millisecond.
#[test]
fn a_present_a_millisecond_before_the_tick_is_captured_as_it_lands() {
    let period = Duration::from_secs(1) / 60;
    let mut h = Synth::<true>::new(Some((60.0, period - Duration::from_millis(1))));
    h.run(Duration::from_secs(2));
    let takes = h.probe.takes();
    let (presented, taken) = takes[0];
    let first = taken - presented;
    assert!(first.abs_diff(Duration::from_millis(1)) < Duration::from_micros(1));
    let waits = h.waits(1);
    assert!(waits.len() >= 115, "{} takes", waits.len());
    assert!(waits.iter().all(|wait| wait.is_zero()), "{waits:?}");
}

/// A viewer joining or pausing that changes only the frame-rate limit keeps
/// the capture source, with an IDR for the change; a smaller size builds
/// another.
#[test]
fn a_frame_rate_change_keeps_the_source_and_a_size_change_rebuilds_it() {
    let mut h = Synth::<false>::new(None);
    h.run(Duration::from_millis(100));
    assert_eq!(h.probe.opened(), 1);
    let watcher = h.join(VideoLimits {
        max_fps: 30,
        ..LIMITS
    });
    h.run(Duration::from_millis(100));
    assert_eq!(h.engine.limits.max_fps, 30);
    assert_eq!(h.probe.opened(), 1);
    // The watcher pausing gives the frame rate back.
    watcher.targets.paused.store(true, Ordering::Release);
    let sent = h.run(Duration::from_millis(100));
    assert_eq!(h.engine.limits, LIMITS);
    assert_eq!(h.probe.opened(), 1);
    assert!(sent.iter().any(|unit| unit.keyframe));

    h.join(VideoLimits {
        max_width: 32,
        max_height: 24,
        ..LIMITS
    });
    h.run(Duration::from_millis(100));
    assert_eq!(h.engine.limits.max_width, 32);
    assert_eq!(h.probe.opened(), 2);
}

/// A recovery builds a fresh capture source, as the backend contract asks,
/// also when the viewers' limits changed only their frame rate while capture
/// was unavailable, which outside an outage keeps the source.
#[test]
fn a_recovery_builds_a_new_source_even_after_a_frame_rate_change_meanwhile() {
    let mut h = Synth::<false>::new(None);
    h.run(Duration::from_millis(100));
    assert_eq!(h.probe.opened(), 1);
    // The display goes away and comes back with the same limits.
    h.probe.lose(true);
    h.run(Duration::from_millis(300));
    assert!(h.engine.unavailable.is_some());
    h.probe.lose(false);
    h.run(Duration::from_millis(300));
    assert!(h.engine.unavailable.is_none());
    assert_eq!(h.probe.opened(), 2);

    // Again, with a watcher at a lower frame rate joining meanwhile.
    h.probe.lose(true);
    h.run(Duration::from_millis(300));
    assert!(h.engine.unavailable.is_some());
    h.join(VideoLimits {
        max_fps: 30,
        ..LIMITS
    });
    h.run(Duration::from_millis(300));
    assert_eq!(h.engine.limits.max_fps, 30);
    h.probe.lose(false);
    let sent = h.run(Duration::from_millis(300));
    assert!(h.engine.unavailable.is_none());
    assert_eq!(h.probe.opened(), 3);
    assert!(sent.first().is_some_and(|unit| unit.keyframe));
}
