//! A platform for tests: in-memory parts that record what they are asked,
//! behind the same `PlatformBackend` every real platform uses. `PUSH = false`
//! is a pulled encoder that thread C polls, as Windows' MFT; `PUSH = true` is
//! a pushing one that wakes thread C when a unit is ready, as a callback
//! encoder does. A pushing source can also be a dated display
//! (`Probe::date`): pictures presented at a set rate on the test's clock,
//! each waking thread C as it lands, as ScreenCaptureKit's do.

use super::{
    CursorSource, Desktop, DisplayModes, HostSupport, Injector, Parts, Platform, StillCapture,
    VideoSource,
};
use crate::{
    BackendError, Button, CursorMode, CursorShape, DRAIN_POLL, Display, DisplaySelection,
    EncodedFrame, FailureKind, HostOs, Key, Presented, Result, VideoCodec, VideoFormat,
    VideoLimits, VideoRate, Waker,
    codec::pipeline::{FIRST_SUBMIT_WAIT, LIVENESS_INTERVAL},
    input::held::Pressed,
    modes::{Mode, Setting},
};
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) enum Synthetic<const PUSH: bool> {}

/// One keyboard injection, as the synthetic injector saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Typed {
    Key(Key, bool),
    Physical(u16, bool),
    Text(String),
}

/// What every part of a session shares, and the test reads and steers.
#[derive(Clone, Default)]
pub(crate) struct Probe(Arc<Mutex<State>>);
#[derive(Default)]
struct State {
    lost: bool,
    pointer: Option<(i32, i32)>,
    down: Vec<Key>,
    released: Vec<Pressed>,
    /// Every key, physical key and text injected, in order.
    typed: Vec<Typed>,
    /// Ready to drained, per unit.
    latencies: Vec<Duration>,
    /// The display's size now, once a mode changed it.
    size: Option<Mode>,
    /// The modes it offers; none, and it offers no change.
    modes: Vec<Mode>,
    /// Every mode set, and how often one was reset.
    applied: Vec<Mode>,
    resets: usize,
    /// The pointer's shape under a handle: 1, 2, … as the test sets them.
    shape: Option<(u64, CursorShape)>,
    /// Nothing on the display changes (`Probe::still`).
    still: bool,
    /// Video sources opened.
    opened: usize,
    /// The dated display, once `Probe::date` set one.
    dated: Option<Dated>,
    /// The desktop checks made so far.
    checks: Checks,
}

/// How often each desktop check ran: a fresh one, and one that may answer
/// from a check a moment ago (`Desktop::prepare_recent`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Checks {
    pub prepare: u32,
    pub prepare_recent: u32,
    pub matches: u32,
    pub matches_recent: u32,
}

/// The test's clock.
pub(crate) type TestClock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// A display presenting every `period` nanoseconds from `epoch`, on the
/// test's clock. Each present lands when `Probe::land` reaches it, replacing
/// the one before if no capture took that, and wakes the source's thread C.
struct Dated {
    epoch: Instant,
    period: f64,
    clock: TestClock,
    /// The newest present landed, and whether a capture took it.
    landed: Option<u64>,
    fresh: bool,
    /// Presents landed since the last capture took one.
    since_take: u32,
    /// Presents a newer one replaced before any capture took them.
    replaced: u64,
    /// Each take: the present's time, and when the capture took it.
    takes: Vec<(Instant, Instant)>,
    /// The open source's wake.
    waker: Option<Waker>,
}
impl Dated {
    fn at(&self, present: u64) -> Instant {
        self.epoch + Duration::from_nanos((present as f64 * self.period).round() as u64)
    }
    /// The newest present at or before `now`.
    fn newest(&self, now: Instant) -> Option<u64> {
        let since = now.checked_duration_since(self.epoch)?;
        let mut present = (since.as_nanos() as f64 / self.period) as u64;
        while self.at(present + 1) <= now {
            present += 1;
        }
        while present > 0 && self.at(present) > now {
            present -= 1;
        }
        Some(present)
    }
}

impl Probe {
    /// The pointer takes `shape`, under a new handle, as an OS sets a cursor.
    pub(crate) fn set_shape(&self, shape: CursorShape) {
        let mut state = self.0.lock();
        let handle = state.shape.as_ref().map_or(1, |(handle, _)| handle + 1);
        state.shape = Some((handle, shape));
    }
    /// Where the pointer is.
    pub(crate) fn point(&self, x: i32, y: i32) {
        self.0.lock().pointer = Some((x, y));
    }
    /// A display device no real computer has, so that nothing here ever
    /// reaches a real one's modes.
    pub(crate) fn display() -> Display {
        Display {
            id: 1,
            name: r"\\.\DISPLAY901".into(),
            x: -1920,
            y: 0,
            width: 64,
            height: 48,
        }
    }
    /// The display as it is now.
    fn current(&self) -> Display {
        let size = self.0.lock().size;
        let display = Self::display();
        match size {
            Some(size) => Display {
                width: size.width,
                height: size.height,
                ..display
            },
            None => display,
        }
    }
    /// A display that offers its own size and a smaller one.
    pub(crate) fn with_modes() -> (Self, Vec<Mode>) {
        let probe = Self::default();
        let display = Self::display();
        let modes = vec![
            Mode {
                width: display.width,
                height: display.height,
            },
            Mode {
                width: 32,
                height: 24,
            },
        ];
        probe.0.lock().modes = modes.clone();
        (probe, modes)
    }
    pub(crate) fn lose(&self, lost: bool) {
        self.0.lock().lost = lost;
    }
    /// While `still`, a pulled source's submit without a keyframe finds no
    /// new picture, as DXGI between two presents, and submits nothing.
    pub(crate) fn still(&self, still: bool) {
        self.0.lock().still = still;
    }
    /// The keyboard injections so far, in order.
    pub(crate) fn typed(&self) -> Vec<Typed> {
        self.0.lock().typed.clone()
    }
    pub(crate) fn released(&self) -> Vec<Pressed> {
        self.0.lock().released.clone()
    }
    /// How many video sources were opened.
    pub(crate) fn opened(&self) -> usize {
        self.0.lock().opened
    }
    /// The desktop checks made since the last call.
    pub(crate) fn checks(&self) -> Checks {
        std::mem::take(&mut self.0.lock().checks)
    }
    /// From now on a pushing source is a display presenting at `hz` from
    /// `epoch` on `clock`, whose presents `land` delivers; set it before the
    /// source opens.
    pub(crate) fn date(&self, hz: f64, epoch: Instant, clock: TestClock) {
        self.0.lock().dated = Some(Dated {
            epoch,
            period: 1e9 / hz,
            clock,
            landed: None,
            fresh: false,
            since_take: 0,
            replaced: 0,
            takes: Vec::new(),
            waker: None,
        });
    }
    /// The first present after `now`.
    pub(crate) fn next_present(&self, now: Instant) -> Option<Instant> {
        let state = self.0.lock();
        let dated = state.dated.as_ref()?;
        Some(dated.at(dated.newest(now).map_or(0, |present| present + 1)))
    }
    /// The presents up to `now` land: each replaces an untaken one, and a
    /// new one wakes the source's thread C.
    pub(crate) fn land(&self, now: Instant) {
        let mut state = self.0.lock();
        let Some(dated) = state.dated.as_mut() else {
            return;
        };
        let Some(newest) = dated.newest(now) else {
            return;
        };
        // Before the first, only the newest present counts.
        let first = dated.landed.map_or(newest, |landed| landed + 1);
        if newest < first {
            return;
        }
        let landing = newest - first + 1;
        dated.replaced += landing - u64::from(!dated.fresh);
        dated.since_take = dated
            .since_take
            .saturating_add(u32::try_from(landing).unwrap_or(u32::MAX));
        dated.landed = Some(newest);
        dated.fresh = true;
        if let Some(waker) = &dated.waker {
            waker.wake();
        }
    }
    /// Each picture a capture took: when it was presented, and taken.
    pub(crate) fn takes(&self) -> Vec<(Instant, Instant)> {
        self.0
            .lock()
            .dated
            .as_ref()
            .map_or_else(Vec::new, |dated| dated.takes.clone())
    }
    /// Presents a newer one replaced before a capture took them.
    pub(crate) fn replaced(&self) -> u64 {
        self.0
            .lock()
            .dated
            .as_ref()
            .map_or(0, |dated| dated.replaced)
    }
}

impl<const PUSH: bool> Platform for Synthetic<PUSH> {
    const OS: HostOs = HostOs::Linux;
    type Grant = Probe;
    type Desktop = SynDesktop;
    type Video = SynVideo<PUSH>;
    type Input = SynInput;
    type Cursor = SynCursor;
    type Modes = SynModes;
    type Still = SynStill;

    fn support() -> HostSupport {
        HostSupport {
            host: true,
            viewer: true,
            secure_desktop: false,
            os: Some(Self::OS),
            reason: None,
            permissions: None,
        }
    }
    fn displays() -> Result<Vec<Display>> {
        Ok(vec![Probe::display()])
    }
    fn acquire(_: &DisplaySelection) -> impl Future<Output = Result<(Probe, Display)>> + Send {
        std::future::ready(Ok((Probe::default(), Probe::display())))
    }
    fn open(probe: &Probe, _: &Display) -> Result<Parts<Self>> {
        Ok(Parts {
            input: SynInput(probe.clone()),
            cursor: SynCursor {
                probe: probe.clone(),
                shaped: 0,
            },
            modes: SynModes(probe.clone()),
            still: SynStill,
            desktop: SynDesktop(probe.clone()),
        })
    }
    fn default_state_dir() -> Option<PathBuf> {
        None
    }
    fn recover(_: &Path) -> Result<usize> {
        Ok(0)
    }
}

pub(crate) struct SynDesktop(Probe);
impl Desktop for SynDesktop {
    type Scope = ();
    fn scope(&self) {}
    fn prepare(&mut self, _: impl FnOnce()) -> Result<()> {
        self.0.0.lock().checks.prepare += 1;
        Ok(())
    }
    fn prepare_recent(&mut self, _: impl FnOnce()) -> Result<()> {
        self.0.0.lock().checks.prepare_recent += 1;
        Ok(())
    }
    fn display(&mut self) -> Result<Display> {
        if self.0.0.lock().lost {
            return Err(BackendError::new(FailureKind::DisplayLost, "display unplugged").into());
        }
        Ok(self.0.current())
    }
    fn display_matches(&mut self, expected: &Display) -> Result<bool> {
        self.0.0.lock().checks.matches += 1;
        Ok(&self.display()? == expected)
    }
    fn display_matches_recent(&mut self, expected: &Display) -> Result<bool> {
        self.0.0.lock().checks.matches_recent += 1;
        Ok(&self.display()? == expected)
    }
}

/// Modes the test lists, and the size each change leaves the display at.
pub(crate) struct SynModes(Probe);
impl DisplayModes for SynModes {
    const SUPPORTED: bool = true;
    fn available(&mut self, _: &str) -> bool {
        !self.0.0.lock().modes.is_empty()
    }
    fn list(&mut self, _: &str) -> Result<Vec<Mode>> {
        Ok(self.0.0.lock().modes.clone())
    }
    fn current(&mut self, _: &str) -> Result<Setting> {
        let display = self.0.current();
        Ok(setting(Mode {
            width: display.width,
            height: display.height,
        }))
    }
    fn target(&mut self, _: &str, mode: Mode) -> Result<Setting> {
        Ok(setting(mode))
    }
    fn apply(&mut self, _: &str, setting: &Setting) -> Result<()> {
        let mut state = self.0.0.lock();
        state.size = Some(setting.size());
        state.applied.push(setting.size());
        Ok(())
    }
    fn reset(&mut self, _: &str, original: &Setting) -> Result<()> {
        let mut state = self.0.0.lock();
        state.size = Some(original.size());
        state.resets += 1;
        Ok(())
    }
    fn scope(&mut self, _: &str) -> Result<String> {
        Ok("synthetic".into())
    }
}
fn setting(mode: Mode) -> Setting {
    Setting {
        width: mode.width,
        height: mode.height,
        hz: 60,
        bpp: 32,
        scale: 0,
    }
}

/// Units in flight, in submit order, each with the time it became ready
/// once it is.
type Flight = Arc<Mutex<VecDeque<Option<(Instant, EncodedFrame)>>>>;

pub(crate) struct SynVideo<const PUSH: bool> {
    probe: Probe,
    codec: VideoCodec,
    limits: VideoLimits,
    waker: Waker,
    flight: Flight,
    frames: u64,
    /// A dated display's source: when it opened and last submitted, on the
    /// test's clock; a keyframe asked for and not yet submitted; and the
    /// last submit's present.
    opened: Option<Instant>,
    last_submit: Option<Instant>,
    keyframe: bool,
    presented: Option<Presented>,
}
impl<const PUSH: bool> VideoSource for SynVideo<PUSH> {
    type Grant = Probe;
    const DRAIN: Option<Duration> = if PUSH { None } else { Some(DRAIN_POLL) };
    fn encodes(_: &Display, codec: VideoCodec) -> bool {
        codec == VideoCodec::H264
    }
    fn open(
        probe: &Probe,
        _: &Display,
        codec: VideoCodec,
        limits: VideoLimits,
        _: VideoRate,
        waker: &Waker,
    ) -> Result<Self> {
        let mut state = probe.0.lock();
        state.opened += 1;
        let opened = state.dated.as_mut().filter(|_| PUSH).map(|dated| {
            dated.waker = Some(waker.clone());
            (dated.clock)()
        });
        drop(state);
        Ok(Self {
            probe: probe.clone(),
            codec,
            limits,
            waker: waker.clone(),
            flight: Flight::default(),
            frames: 0,
            opened,
            last_submit: None,
            keyframe: true,
            presented: None,
        })
    }
    /// Serves new limits of the same size as it is; a size change rebuilds.
    fn retune(&mut self, _: &Display, limits: VideoLimits) -> Result<bool> {
        let same_size = (limits.max_width, limits.max_height)
            == (self.limits.max_width, self.limits.max_height);
        if same_size {
            self.limits = limits;
        }
        Ok(same_size)
    }
    fn codec(&self) -> VideoCodec {
        self.codec
    }
    fn set_rate(&mut self, _: VideoRate) -> Result<()> {
        Ok(())
    }
    /// Pulled, a unit is ready at the next poll; pushed, a millisecond later
    /// on the encoder's own thread, while thread C waits, which it wakes. The
    /// millisecond is waited as thread C waits, on a timer the platform does
    /// not coalesce. Pulled from a still probe, it submits only a keyframe,
    /// which repeats the last picture. A pushed source names no time to try
    /// again, so it is never still.
    fn submit(&mut self, keyframe: bool) -> Result<bool> {
        self.presented = None;
        if self.opened.is_some() {
            return self.submit_dated(keyframe);
        }
        if !PUSH && !keyframe && self.probe.0.lock().still {
            return Ok(false);
        }
        self.frames += 1;
        let display = self.probe.current();
        let frame = EncodedFrame {
            format: VideoFormat {
                codec: self.codec,
                width: display.width,
                height: display.height,
                keyframe,
                pts_us: self.frames * 16_667,
                ..VideoFormat::default()
            },
            bytes: vec![1; 64],
        };
        let mut flight = self.flight.lock();
        if !PUSH {
            flight.push_back(Some((Instant::now(), frame)));
            return Ok(true);
        }
        flight.push_back(None);
        drop(flight);
        let (flight, waker) = (self.flight.clone(), self.waker.clone());
        let wake = crate::scheduler::Wake::new();
        let encoder = crate::scheduler::Waiter::new(&wake)?;
        std::thread::spawn(move || {
            let ready = Instant::now() + Duration::from_millis(1);
            while Instant::now() < ready {
                if encoder.wait(ready).is_err() {
                    break;
                }
            }
            if let Some(slot) = flight.lock().iter_mut().find(|unit| unit.is_none()) {
                *slot = Some((Instant::now(), frame));
            }
            waker.wake();
        });
        Ok(true)
    }
    fn presented(&self) -> Option<Presented> {
        self.presented
    }
    /// A dated display's source as ScreenCaptureKit's: a new picture at
    /// once, else the last one again for a keyframe or every
    /// `LIVENESS_INTERVAL`; it wakes thread C for a new one.
    fn idle_until(&self, keyframe_due: Option<Instant>) -> Option<Instant> {
        let opened = self.opened?;
        let fresh = self
            .probe
            .0
            .lock()
            .dated
            .as_ref()
            .is_some_and(|dated| dated.fresh);
        match self.last_submit {
            _ if fresh => None,
            None => Some(opened + FIRST_SUBMIT_WAIT),
            Some(_) if self.keyframe => None,
            Some(last) => {
                let until = last + LIVENESS_INTERVAL;
                Some(keyframe_due.map_or(until, |due| due.min(until)))
            }
        }
    }
    fn drain(&mut self) -> Result<Option<EncodedFrame>> {
        let mut flight = self.flight.lock();
        if flight.front().is_none_or(Option::is_none) {
            return Ok(None);
        }
        let Some(Some((ready, frame))) = flight.pop_front() else {
            return Ok(None);
        };
        self.probe.0.lock().latencies.push(ready.elapsed());
        Ok(Some(frame))
    }
    fn pending(&self) -> usize {
        self.flight.lock().len()
    }
}

impl<const PUSH: bool> SynVideo<PUSH> {
    /// The newest present if no capture took it, else a repeat for a
    /// keyframe or liveness, else nothing; the unit is ready at once.
    fn submit_dated(&mut self, keyframe: bool) -> Result<bool> {
        self.keyframe |= keyframe;
        let mut state = self.probe.0.lock();
        let Some(dated) = state.dated.as_mut() else {
            return Ok(false);
        };
        let now = (dated.clock)();
        let repeat = self.keyframe
            || self
                .last_submit
                .is_some_and(|last| now >= last + LIVENESS_INTERVAL);
        match dated.landed {
            Some(present) if dated.fresh => {
                let at = dated.at(present);
                self.presented = Some(Presented::new(at, dated.since_take));
                dated.fresh = false;
                dated.since_take = 0;
                dated.takes.push((at, now));
            }
            Some(_) if repeat && self.last_submit.is_some() => {}
            _ => return Ok(false),
        }
        drop(state);
        self.frames += 1;
        let display = self.probe.current();
        let frame = EncodedFrame {
            format: VideoFormat {
                codec: self.codec,
                width: display.width,
                height: display.height,
                keyframe: self.keyframe,
                duplicate: self.presented.is_none(),
                pts_us: self.frames * 16_667,
                ..VideoFormat::default()
            },
            bytes: vec![1; 64],
        };
        self.flight.lock().push_back(Some((Instant::now(), frame)));
        self.waker.wake();
        self.keyframe = false;
        self.last_submit = Some(now);
        Ok(true)
    }
}

pub(crate) struct SynInput(Probe);
impl Injector for SynInput {
    const LOCK: bool = false;
    const PHYSICAL_KEYS: bool = true;
    fn pointer(&mut self, x: i32, y: i32) -> Result<()> {
        self.0.0.lock().pointer = Some((x, y));
        Ok(())
    }
    fn button(&mut self, _: Button, _: bool) -> Result<()> {
        Ok(())
    }
    fn wheel(&mut self, _: i32, _: i32) -> Result<()> {
        Ok(())
    }
    fn key(&mut self, key: Key, down: bool) -> Result<()> {
        let mut state = self.0.0.lock();
        if down {
            state.down.push(key);
        } else {
            state.down.retain(|held| *held != key);
        }
        state.typed.push(Typed::Key(key, down));
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<()> {
        self.0.0.lock().typed.push(Typed::Text(text.to_owned()));
        Ok(())
    }
    fn physical_key(&mut self, usage: u16, down: bool) -> Result<()> {
        self.0.0.lock().typed.push(Typed::Physical(usage, down));
        Ok(())
    }
    fn lock(&mut self) -> Result<()> {
        Err(BackendError::new(FailureKind::Unsupported, "lock").into())
    }
    fn release(&mut self, pressed: Pressed) -> Result<()> {
        self.0.0.lock().released.push(pressed);
        Ok(())
    }
}

/// The pointer the test steers, and its shape, which it sets as an OS sets
/// a new cursor handle.
pub(crate) struct SynCursor {
    probe: Probe,
    /// The handle whose shape `shape` last reported.
    shaped: u64,
}
impl CursorSource for SynCursor {
    const MODE: CursorMode = CursorMode::Separate;
    const SHAPES: bool = true;
    fn read(&mut self) -> Result<Option<(i32, i32, bool)>> {
        Ok(self.probe.0.lock().pointer.map(|(x, y)| (x, y, true)))
    }
    fn shape(&mut self) -> Result<Option<CursorShape>> {
        let state = self.probe.0.lock();
        match &state.shape {
            Some((handle, shape)) if *handle != self.shaped => {
                self.shaped = *handle;
                Ok(Some(shape.clone()))
            }
            _ => Ok(None),
        }
    }
}

pub(crate) struct SynStill;
impl StillCapture for SynStill {
    fn jpeg(&mut self, _: &Display) -> Result<Vec<u8>> {
        Ok(vec![0xff, 0xd8, 0xff, 0xd9])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Backend, DisplayModeError, Host, HostOptions, Input, ModeTarget, NoticeCode, StateReason,
        ViewerReason, ViewerStatus,
        launch::Launcher,
        modes::Journal,
        native::PlatformBackend,
        platform::fallback_caps,
        test_util::{Presenting, until},
    };
    use pb_mapper_client::sdk::Client;
    use pb_mapper_testkit::{Relay, admin_credential};

    /// One session streams, applies input at the display's own offset and
    /// releases the keys it holds when it ends, physical ones too; text is
    /// typed with one Enter per line break; the next session reports its
    /// display lost as the viewer's reason. A pushing source is drained as it
    /// wakes thread C, never polled.
    async fn session<const PUSH: bool>(name: &str) {
        let relay = Relay::start(name).await;
        let client =
            Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
        let probe = Probe::default();
        let grant = Arc::new(probe.clone());
        let backend = PlatformBackend::<Synthetic<PUSH>>::open(
            grant.clone(),
            &Probe::display(),
            Journal::default(),
        )
        .unwrap();
        assert_eq!(backend.drain_poll().is_none(), PUSH);
        drop(backend);
        let launch = Arc::new(Launcher::new(
            move || {
                PlatformBackend::<Synthetic<PUSH>>::open(
                    grant.clone(),
                    &Probe::display(),
                    Journal::default(),
                )
            },
            fallback_caps::<Synthetic<PUSH>>(),
        ));
        let mut options = HostOptions::new(format!("desktop:{name}"));
        options.allow_control = true;
        options.allow_direct = false;
        let host = Host::builder(options)
            .launch(launch)
            .start(&client)
            .await
            .unwrap();

        let watching = Presenting::connect(&client, &host).await;
        let controlled = watching.control().await;
        assert!(
            controlled
                .capabilities
                .iter()
                .any(|listed| listed == "physical_keys")
        );
        let shift = Input::Key {
            key: Key::Shift,
            down: true,
        };
        let point = Input::Move { x: 3, y: 4 };
        let (shift_left, a) = (0xE1, 0x04);
        let physical = |usage, down| Input::PhysicalKey { usage, down };
        let text = Input::Text {
            text: "a\r\nb".into(),
        };
        watching
            .viewer
            .input(
                controlled.generation,
                &[
                    shift,
                    point,
                    physical(shift_left, true),
                    physical(a, true),
                    physical(a, false),
                    text,
                ],
            )
            .unwrap();
        until(|| {
            let state = probe.0.lock();
            state.down == [Key::Shift] && state.pointer == Some((-1917, 4))
        })
        .await;
        until(|| probe.typed().len() == 8).await;
        assert_eq!(
            probe.typed(),
            [
                Typed::Key(Key::Shift, true),
                Typed::Physical(shift_left, true),
                Typed::Physical(a, true),
                Typed::Physical(a, false),
                Typed::Text("a".into()),
                Typed::Key(Key::Enter, true),
                Typed::Key(Key::Enter, false),
                Typed::Text("b".into()),
            ]
        );
        if !PUSH {
            // The pointer takes another shape where the controller's own
            // move left it: the shape reaches the viewer all the same, and
            // the next one under a number of its own.
            let shown = |viewer: &crate::Viewer| {
                let view = viewer.view();
                viewer.cursor_shape(view.cursor_shape)
            };
            for kind in [crate::CursorKind::Ibeam, crate::CursorKind::Custom] {
                let shape = CursorShape::new(kind, 2, 2, (1, 0), vec![9; 16]).unwrap();
                probe.set_shape(shape.clone());
                until(|| shown(&watching.viewer).as_ref() == Some(&shape)).await;
            }
        }
        if PUSH {
            // Drained because the source woke thread C. Without the wake a
            // ready unit would wait for thread C's next deadline, its 16 ms
            // pointer sample at the latest: as often more than 8 ms after it
            // was ready as less. Woken, nine units in ten go within 8 ms of
            // being ready, room for a busy machine's scheduling.
            until(|| probe.0.lock().latencies.len() >= 20).await;
            let latencies = probe.0.lock().latencies.clone();
            let prompt = latencies
                .iter()
                .filter(|latency| **latency < Duration::from_millis(8))
                .count();
            assert!(prompt * 10 >= latencies.len() * 9, "{latencies:?}");
        }
        watching.stop().await;
        until(|| probe.released().len() == 2).await;
        let released = probe.released();
        assert!(
            released.contains(&Pressed::Key(Key::Shift))
                && released.contains(&Pressed::Physical(shift_left))
        );

        let watching = Presenting::connect(&client, &host).await;
        watching.until(|status| status.fresh).await;
        probe.lose(true);
        until(|| host.status().reason == Some(StateReason::DisplayLost)).await;
        watching.stop().await;
        host.stop().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_pulled_platform_streams_injects_releases_and_loses_its_display() {
        session::<false>("desktop-synthetic-pull").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_pushed_platform_is_drained_when_it_wakes_thread_c() {
        session::<true>("desktop-synthetic-push").await;
    }

    /// A viewer that controls `host`.
    async fn controlling(client: &Client, host: &Host) -> Presenting {
        let watching = Presenting::connect(client, host).await;
        watching.control().await;
        watching
    }

    /// The status once the display is `width` wide, presented in a
    /// generation after `since`.
    async fn shows(watching: &Presenting, width: u32, since: u64) -> ViewerStatus {
        watching
            .until(|status| {
                status.display.as_ref().is_some_and(|d| d.width == width)
                    && status.video.is_some_and(|video| video.width == width)
                    && status.generation > since
                    && status.fresh
            })
            .await
    }

    /// The remote resolution, end to end on a synthetic display: a viewer
    /// fits its display and sees the new size in a new generation, from an
    /// IDR; a second change while the first settles is refused with a
    /// notice, and a capture failure then is `DisplayChanging`. The owner's
    /// re-grant keeps the mode; a revoke, the session's end and the owner's
    /// Restore put it back; a mode the owner chose meanwhile is left alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_remote_resolution_changes_live_and_goes_back() {
        let relay = Relay::start("desktop-synthetic-modes").await;
        let client =
            Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
        let (probe, modes) = Probe::with_modes();
        let (own, small) = (modes[0], modes[1]);
        let dir = pb_mapper_core::test_support::TempPath::new("desktop-modes-e2e");
        let journal = Journal::in_dir(Some(dir.to_path_buf()));
        let (grant, opened) = (Arc::new(probe.clone()), journal.clone());
        let (replayed, replaying) = (probe.clone(), journal.clone());
        let launch = Launcher::new(
            move || {
                PlatformBackend::<Synthetic<false>>::open(
                    grant.clone(),
                    &Probe::display(),
                    opened.clone(),
                )
            },
            fallback_caps::<Synthetic<false>>(),
        )
        .recovering(move || {
            crate::modes::recover(&mut SynModes(replayed.clone()), &replaying)?;
            Ok(replaying.pending())
        });
        let mut options = HostOptions::new("desktop:synthetic-modes");
        options.allow_control = true;
        options.allow_direct = false;
        let host = Host::builder(options)
            .launch(Arc::new(launch))
            .start(&client)
            .await
            .unwrap();
        let resets = || probe.0.lock().resets;
        let shows_owners =
            |status: &ViewerStatus| status.display.as_ref().is_some_and(|d| d.width == 48);

        // Fit the viewer's display: a new generation at the new size.
        let watching = controlling(&client, &host).await;
        let before = watching.viewer.status();
        let listed = before.modes.clone().unwrap();
        assert_eq!(
            (listed.current, listed.supported.clone()),
            (own, modes.clone())
        );
        assert_eq!(
            watching
                .viewer
                .set_display_mode(ModeTarget::FitLocal(small)),
            Ok(small)
        );
        let fitted = shows(&watching, small.width, before.generation).await;
        assert_eq!(fitted.modes.unwrap().original, own);
        assert_eq!(host.status().display_mode.unwrap().current, small);

        // Too soon for another change: refused, and said so.
        assert_eq!(
            watching.viewer.set_display_mode(ModeTarget::Original),
            Ok(own)
        );
        watching
            .until(|status| {
                status
                    .notice
                    .as_ref()
                    .is_some_and(|notice| notice.code == NoticeCode::DisplayModeFailed)
            })
            .await;
        // A capture failure while it settles is the display changing.
        probe.lose(true);
        until(|| host.status().reason == Some(StateReason::DisplayChanging)).await;
        watching
            .until(|status| status.reason == Some(ViewerReason::DisplayChanging))
            .await;
        probe.lose(false);
        let settled = watching.until(|status| status.fresh).await;

        // A re-grant ends control and keeps the mode; a revoke restores it.
        host.set_control_allowed(true);
        watching.until(|status| !status.controlling).await;
        assert_eq!((probe.current().width, resets()), (small.width, 0));
        watching.control().await;
        host.set_control_allowed(false);
        shows(&watching, own.width, settled.generation).await;
        assert_eq!(resets(), 1);
        assert_eq!(
            watching.viewer.set_display_mode(ModeTarget::Size(small)),
            Err(DisplayModeError::NotControlling)
        );

        // The owner picks another size meanwhile: never fought.
        host.set_control_allowed(true);
        watching.control().await;
        tokio::time::sleep(crate::modes::SETTLE).await;
        let generation = watching.viewer.status().generation;
        watching
            .viewer
            .set_display_mode(ModeTarget::Size(small))
            .unwrap();
        shows(&watching, small.width, generation).await;
        probe.0.lock().size = Some(Mode {
            width: 48,
            height: 36,
        });
        watching.until(shows_owners).await;
        watching.stop().await;
        until(|| host.status().viewer.is_none()).await;
        assert_eq!((probe.current().width, resets()), (48, 1));

        // The session's end puts the display back, and so does the owner.
        for owner in [false, true] {
            let watching = controlling(&client, &host).await;
            let generation = watching.viewer.status().generation;
            watching
                .viewer
                .set_display_mode(ModeTarget::Size(small))
                .unwrap();
            shows(&watching, small.width, generation).await;
            if owner {
                // The owner's Restore needs no control, but waits out the
                // settle window like any change. The host's card follows at
                // once, even while the viewer pauses.
                tokio::time::sleep(crate::modes::SETTLE).await;
                watching.viewer.pause();
                until(|| host.status().viewer.is_some_and(|viewer| viewer.paused)).await;
                assert!(host.restore_display_mode());
                until(|| host.status().display_mode.is_none()).await;
                assert_eq!(probe.current().width, 48);
                watching.viewer.resume();
                watching.until(shows_owners).await;
            }
            watching.stop().await;
            until(|| host.status().viewer.is_none()).await;
            until(|| probe.current().width == 48).await;
        }
        assert_eq!(resets(), 3);
        assert!(!journal.pending());
        host.stop().await.unwrap();
    }
}
