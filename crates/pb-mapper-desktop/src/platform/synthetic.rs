//! A platform for tests: in-memory parts that record what they are asked,
//! behind the same `PlatformBackend` every real platform uses. `PUSH = false`
//! is a pulled encoder that thread C polls, as Windows' MFT; `PUSH = true` is
//! a pushing one that wakes thread C when a unit is ready, as a callback
//! encoder does.

use super::{
    CursorSource, Desktop, DisplayModes, HostSupport, Injector, Parts, Platform, StillCapture,
    VideoSource,
};
use crate::{
    BackendError, Button, CursorMode, DRAIN_POLL, Display, DisplaySelection, EncodedFrame,
    FailureKind, HostOs, Key, Result, VideoCodec, VideoFormat, VideoLimits, VideoRate, Waker,
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

/// What every part of a session shares, and the test reads and steers.
#[derive(Clone, Default)]
pub(crate) struct Probe(Arc<Mutex<State>>);
#[derive(Default)]
struct State {
    lost: bool,
    pointer: Option<(i32, i32)>,
    down: Vec<Key>,
    released: Vec<Pressed>,
    /// Submit to drain, per unit.
    latencies: Vec<Duration>,
    /// The display's size now, once a mode changed it.
    size: Option<Mode>,
    /// The modes it offers; none, and it offers no change.
    modes: Vec<Mode>,
    /// Every mode set, and how often one was reset.
    applied: Vec<Mode>,
    resets: usize,
}
impl Probe {
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
            cursor: SynCursor(probe.clone()),
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
        Ok(())
    }
    fn display(&mut self) -> Result<Display> {
        if self.0.0.lock().lost {
            return Err(BackendError::new(FailureKind::DisplayLost, "display unplugged").into());
        }
        Ok(self.0.current())
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
    }
}

/// Units in flight, each with its submit time and when it is ready.
type Flight = Arc<Mutex<VecDeque<(Instant, Option<EncodedFrame>)>>>;

pub(crate) struct SynVideo<const PUSH: bool> {
    probe: Probe,
    codec: VideoCodec,
    waker: Waker,
    flight: Flight,
    frames: u64,
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
        _: VideoLimits,
        _: VideoRate,
        waker: &Waker,
    ) -> Result<Self> {
        Ok(Self {
            probe: probe.clone(),
            codec,
            waker: waker.clone(),
            flight: Flight::default(),
            frames: 0,
        })
    }
    fn codec(&self) -> VideoCodec {
        self.codec
    }
    fn set_rate(&mut self, _: VideoRate) -> Result<()> {
        Ok(())
    }
    /// Pulled, a unit is ready at the next poll; pushed, a millisecond later
    /// on the encoder's own thread, which wakes thread C.
    fn submit(&mut self, keyframe: bool) -> Result<()> {
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
            flight.push_back((Instant::now(), Some(frame)));
            return Ok(());
        }
        flight.push_back((Instant::now(), None));
        drop(flight);
        let (flight, waker) = (self.flight.clone(), self.waker.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1));
            if let Some(slot) = flight.lock().iter_mut().find(|(_, unit)| unit.is_none()) {
                slot.1 = Some(frame);
            }
            waker.wake();
        });
        Ok(())
    }
    fn drain(&mut self) -> Result<Option<EncodedFrame>> {
        let mut flight = self.flight.lock();
        if flight.front().is_none_or(|(_, unit)| unit.is_none()) {
            return Ok(None);
        }
        let Some((submitted, frame)) = flight.pop_front() else {
            return Ok(None);
        };
        self.probe.0.lock().latencies.push(submitted.elapsed());
        Ok(frame)
    }
    fn pending(&self) -> usize {
        self.flight.lock().len()
    }
}

pub(crate) struct SynInput(Probe);
impl Injector for SynInput {
    const LOCK: bool = false;
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
        Ok(())
    }
    fn text(&mut self, _: &str) -> Result<()> {
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

pub(crate) struct SynCursor(Probe);
impl CursorSource for SynCursor {
    const MODE: CursorMode = CursorMode::Separate;
    fn read(&mut self) -> Result<Option<(i32, i32, bool)>> {
        Ok(self.0.0.lock().pointer.map(|(x, y)| (x, y, true)))
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
    /// releases the key it holds when it ends; the next reports its display
    /// lost as the viewer's reason. A pushing source is drained as it wakes
    /// thread C, never polled.
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
        let shift = Input::Key {
            key: Key::Shift,
            down: true,
        };
        let point = Input::Move { x: 3, y: 4 };
        watching
            .viewer
            .input(controlled.generation, &[shift, point])
            .unwrap();
        until(|| {
            let state = probe.0.lock();
            state.down == [Key::Shift] && state.pointer == Some((-1917, 4))
        })
        .await;
        if PUSH {
            // Drained when the source woke thread C, not at the next tick.
            let latencies = probe.0.lock().latencies.clone();
            let prompt = latencies
                .iter()
                .filter(|latency| **latency < Duration::from_millis(8))
                .count();
            assert!(prompt * 2 >= latencies.len(), "{latencies:?}");
        }
        watching.stop().await;
        until(|| probe.0.lock().released.contains(&Pressed::Key(Key::Shift))).await;

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
        let dir = std::env::temp_dir().join(format!("pb-desktop-modes-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let journal = Journal::in_dir(Some(dir.clone()));
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
        let _ = std::fs::remove_dir_all(dir);
    }
}
