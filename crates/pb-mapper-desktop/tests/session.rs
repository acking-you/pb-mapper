//! End to end over a real relay, TLS and both v2 ends: the host's session on
//! receipt credit and thread C, and the viewer with a native-style consumer.
#![allow(clippy::unwrap_used)]
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use pb_mapper_desktop::{
    Backend, Button, ConnectError, CursorPosition, DesktopContext, Display, EncodedFrame, Host,
    HostOptions, HostState, Input, InputError, Invitation, Key, Pull, QualityPreset, Result,
    VideoCodec, VideoFormat, VideoLimits, VideoRate, Viewer, ViewerConfig, ViewerState,
    ViewerStatus,
};
use pb_mapper_server::DirectServerConfig;
use pb_mapper_testkit::{Relay, Shape, admin_credential, shaped_link};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// What a test backend saw, shared with the test.
#[derive(Default)]
struct Observation {
    captures: AtomicU32,
    inputs: AtomicU32,
    releases: AtomicU32,
    /// A button is held down.
    held: AtomicBool,
    dropped: AtomicU32,
    /// DIAGNOSTIC (temporary): when each encoded capture ran, and thread C's
    /// QoS class.
    stamps: Mutex<Vec<Instant>>,
    qos: AtomicU32,
}
impl Observation {
    fn input(&self, input: &Input) {
        if matches!(input, Input::Button { down: true, .. }) {
            self.held.store(true, Ordering::Release);
        }
        self.inputs.fetch_add(1, Ordering::AcqRel);
    }
    fn release(&self) {
        self.held.store(false, Ordering::Release);
        self.releases.fetch_add(1, Ordering::AcqRel);
    }
}

/// A JPEG desktop on a secondary display.
struct Desktop(Arc<Observation>);
impl Backend for Desktop {
    fn display(&mut self) -> Result<Display> {
        Ok(Display {
            id: 1,
            name: "Test display".into(),
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        self.0.captures.fetch_add(1, Ordering::Relaxed);
        Ok(vec![10, 20, 30, 40])
    }
    fn input(&mut self, input: &Input) -> Result<()> {
        self.0.input(input);
        Ok(())
    }
    fn release(&mut self) {
        self.0.release();
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// An H.264 desktop with a pointer at `cursor`: every capture is one opaque
/// unit of `bytes`, an IDR when the worker asks for one.
struct Encoded {
    observed: Arc<Observation>,
    cursor: Arc<AtomicU32>,
    bytes: usize,
    frames: u64,
}
impl Backend for Encoded {
    fn display(&mut self) -> Result<Display> {
        Ok(Display {
            id: 1,
            name: "encoded desktop".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        unreachable!("this desktop is H.264 only")
    }
    fn capture_video(
        &mut self,
        _: &DesktopContext,
        _: VideoCodec,
        keyframe: bool,
        _: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        self.frames += 1;
        self.observed.captures.fetch_add(1, Ordering::Relaxed);
        self.observed.stamps.lock().unwrap().push(Instant::now());
        self.observed.qos.store(diag::qos(), Ordering::Relaxed);
        Ok(Some(EncodedFrame {
            format: VideoFormat {
                codec: VideoCodec::H264,
                width: 1920,
                height: 1080,
                keyframe,
                pts_us: self.frames * 16_667,
                ..Default::default()
            },
            bytes: vec![1; self.bytes],
        }))
    }
    fn cursor(&mut self, _: &DesktopContext) -> Result<Option<CursorPosition>> {
        Ok(Some(CursorPosition {
            x: self.cursor.load(Ordering::Relaxed) as i32,
            y: 0,
            visible: true,
        }))
    }
    fn input(&mut self, input: &Input) -> Result<()> {
        self.observed.input(input);
        Ok(())
    }
    fn release(&mut self) {
        self.observed.release();
    }
}

/// A host of `Encoded` units through the relay only, with control allowed.
async fn encoded_host(
    client: &Client,
    service: &str,
    observed: &Arc<Observation>,
    cursor: &Arc<AtomicU32>,
    bytes: usize,
) -> Host {
    let (observed, cursor) = (observed.clone(), cursor.clone());
    let mut options = HostOptions::new(service);
    options.allow_control = true;
    options.allow_direct = false;
    Host::builder(options)
        .backend(move || {
            Ok(Encoded {
                observed: observed.clone(),
                cursor: cursor.clone(),
                bytes,
                frames: 0,
            })
        })
        .start(client)
        .await
        .unwrap()
}

/// The same host and pin with another bearer token, as a well-formed `pbd1`.
fn forged(invitation: &Invitation) -> Invitation {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let text = invitation.encode();
    let mut payload = URL_SAFE_NO_PAD.decode(&text["pbd1.".len()..]).unwrap();
    let check = payload.len() - 4;
    payload[check - 32..check].fill(b'x');
    let digest = ring::digest::digest(&ring::digest::SHA256, &payload[..check]);
    payload[check..].copy_from_slice(&digest.as_ref()[..4]);
    Invitation::decode(&format!("pbd1.{}", URL_SAFE_NO_PAD.encode(payload))).unwrap()
}

fn config(codec: VideoCodec) -> ViewerConfig {
    ViewerConfig {
        codecs: vec![codec],
        limits: VideoLimits::for_viewer(60.0),
        fps_override: false,
        quality: QualityPreset::Balanced,
        progress: None,
    }
}

async fn relay_viewer(client: &Client, host: &Host, codec: VideoCodec) -> Viewer {
    Viewer::connect(
        client,
        host.invitation(),
        ConnectOptions {
            path: PathPolicy::RelayOnly,
            ..Default::default()
        },
        config(codec),
    )
    .await
    .unwrap()
}

/// A unit the renderer took.
#[derive(Clone, Copy, Debug)]
struct Pulled {
    keyframe: bool,
    discontinuity: bool,
}

/// A renderer on its own thread, as a native consumer runs: it pulls every
/// unit and, while presenting, presents it at once.
struct Renderer {
    presenting: Arc<AtomicBool>,
    pulled: Arc<Mutex<Vec<Pulled>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Renderer {
    fn start(viewer: &Viewer) -> Self {
        let consumer = viewer.video_sink().attach().unwrap();
        let presenting = Arc::new(AtomicBool::new(true));
        let pulled = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (present, log, done) = (presenting.clone(), pulled.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut capacity = 4 << 20;
            while !done.load(Ordering::Acquire) {
                match consumer.next(Duration::from_millis(50), capacity) {
                    Pull::Unit(unit) => {
                        log.lock().unwrap().push(Pulled {
                            keyframe: unit.keyframe,
                            discontinuity: unit.discontinuity,
                        });
                        if present.load(Ordering::Acquire) {
                            consumer.presented(unit.generation, unit.frame_id);
                        }
                    }
                    Pull::NeedCapacity(needed) => capacity = needed,
                    Pull::Timeout | Pull::Paused => {}
                    Pull::Closed => break,
                }
            }
        });
        Self {
            presenting,
            pulled,
            stop,
            thread: Some(thread),
        }
    }
    fn present(&self, presenting: bool) {
        self.presenting.store(presenting, Ordering::Release);
    }
    fn pulled(&self) -> usize {
        self.pulled.lock().unwrap().len()
    }
    /// The `index`th unit pulled, waiting for it.
    async fn unit(&self, index: usize) -> Pulled {
        poll(Duration::from_secs(10), || {
            self.pulled.lock().unwrap().get(index).copied()
        })
        .await
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

/// What `probe` first returns, checking every 10 ms for up to `limit`.
async fn poll<T>(limit: Duration, mut probe: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(limit, async {
        loop {
            if let Some(value) = probe() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

/// The viewer's status once `condition` holds, within 10 s.
async fn wait(viewer: &Viewer, condition: impl Fn(&ViewerStatus) -> bool) -> ViewerStatus {
    poll(Duration::from_secs(10), || {
        let status = viewer.status();
        condition(&status).then_some(status)
    })
    .await
}

/// Poll `condition` for up to `limit`.
async fn until(limit: Duration, mut condition: impl FnMut() -> bool) {
    poll(limit, || condition().then_some(())).await;
}

const SHIFT: [Input; 1] = [Input::Key {
    key: Key::Shift,
    down: true,
}];
const PRESS: [Input; 1] = [Input::Button {
    button: Button::Left,
    down: true,
    drag_start: false,
}];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_desktop_direct_and_relay_enforce_grants_and_owned_teardown() {
    let mut relay = Relay::start_with_direct(
        "desktop-session",
        Some(DirectServerConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            advertise: "127.0.0.1:0".parse().unwrap(),
        }),
    )
    .await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
    for path in [PathPolicy::RelayOnly, PathPolicy::Auto] {
        let options = || ConnectOptions {
            path,
            ..Default::default()
        };
        let observed = Arc::new(Observation::default());
        let backend = observed.clone();
        let mut host_options = HostOptions::new("desktop:test");
        host_options.allow_control = true;
        let host = Host::builder(host_options)
            .backend(move || Ok(Desktop(backend.clone())))
            .start(&client)
            .await
            .unwrap();
        // A valid relay credential does not grant desktop access.
        let intruder = Viewer::connect(
            &client,
            &forged(host.invitation()),
            options(),
            config(VideoCodec::Jpeg),
        )
        .await;
        assert!(matches!(intruder, Err(ConnectError::Denied)));
        assert_eq!(observed.captures.load(Ordering::Relaxed), 0);

        let viewer = Viewer::connect(
            &client,
            host.invitation(),
            options(),
            config(VideoCodec::Jpeg),
        )
        .await
        .unwrap();
        let renderer = Renderer::start(&viewer);
        let viewed = wait(&viewer, |status| status.fresh).await;
        assert_eq!(
            viewed.stats.path,
            if path == PathPolicy::Auto {
                "direct"
            } else {
                "relay"
            }
        );
        // A direct path's link reaches the status, under the names the app
        // reads; the relay has none.
        let stats = serde_json::to_value(&viewed.stats).unwrap();
        let link = &stats["link"];
        if path == PathPolicy::Auto {
            for key in [
                "kind",
                "family",
                "via",
                "peer",
                "transport",
                "rtt_ms",
                "min_rtt_ms",
                "setup_ms",
            ] {
                assert!(!link[key].is_null(), "{key} missing: {link}");
            }
            for key in ["local_ip", "same_lan"] {
                assert!(link.get(key).is_some(), "{key} missing: {link}");
            }
        } else {
            assert!(link.is_null(), "{link}");
        }
        assert!(matches!(
            viewer.input(viewed.generation, &SHIFT),
            Err(InputError::NotControlling)
        ));

        // Control starts a new generation: input needs one of its frames
        // presented through the consumer, not the frame shown before.
        renderer.present(false);
        viewer.request_control(true).unwrap();
        let granted = wait(&viewer, |status| status.controlling).await;
        assert!(granted.generation > viewed.generation);
        assert!(matches!(
            viewer.input(granted.generation, &SHIFT),
            Err(InputError::NotFresh)
        ));
        renderer.present(true);
        let controlled = wait(&viewer, |status| status.controlling && status.fresh).await;
        viewer.input(controlled.generation, &SHIFT).unwrap();
        until(Duration::from_secs(3), || {
            observed.inputs.load(Ordering::Acquire) == 1
        })
        .await;

        // Repeated releases coalesce on the way to the host.
        let releases = observed.releases.load(Ordering::Acquire);
        for _ in 0..1000 {
            viewer.release_input().unwrap();
        }
        until(Duration::from_secs(3), || {
            observed.releases.load(Ordering::Acquire) > releases
        })
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let released = observed.releases.load(Ordering::Acquire) - releases;
        assert!(released < 100, "{released} releases reached the backend");

        host.set_control_allowed(false);
        let revoked = wait(&viewer, |status| {
            !status.controlling && !status.control_allowed
        })
        .await;
        assert!(viewer.input(revoked.generation, &SHIFT).is_err());
        assert_eq!(observed.inputs.load(Ordering::Acquire), 1);
        // Direct ownership survives coordinator loss; relay transport need not.
        if path == PathPolicy::Auto {
            relay.stop_server().await;
            let frame = revoked.presented.unwrap().frame_id;
            wait(&viewer, |status| {
                status
                    .presented
                    .is_some_and(|presented| presented.frame_id > frame)
            })
            .await;
        }
        viewer.stop().await.unwrap();
        drop(renderer);
        until(Duration::from_secs(5), || {
            host.status().state == HostState::Waiting
        })
        .await;
        let stopped = host.status();
        assert!(stopped.message.is_empty(), "{}", stopped.message);
        host.stop().await.unwrap();
        assert_eq!(observed.dropped.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frozen_renderer_keeps_credit_and_cursor_but_never_input() {
    let relay = Relay::start("desktop-frozen").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
    let observed = Arc::new(Observation::default());
    let cursor = Arc::new(AtomicU32::new(0));
    let host = encoded_host(&client, "desktop:frozen", &observed, &cursor, 4096).await;
    let viewer = relay_viewer(&client, &host, VideoCodec::H264).await;
    let renderer = Renderer::start(&viewer);
    wait(&viewer, |status| status.fresh).await;
    viewer.request_control(true).unwrap();
    let controlled = wait(&viewer, |status| status.controlling && status.fresh).await;
    viewer.input(controlled.generation, &PRESS).unwrap();
    until(Duration::from_secs(3), || {
        observed.held.load(Ordering::Acquire)
    })
    .await;
    // DIAGNOSTIC (temporary): one second while presenting.
    let (warm_at, warm_pulled, warm_captures, warm_cpu) = (
        Instant::now(),
        renderer.pulled(),
        observed.captures.load(Ordering::Relaxed),
        diag::cpu(),
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    let presenting = format!(
        "presenting {:.2}s: captures +{} pulled +{} cpu {:?} {}",
        warm_at.elapsed().as_secs_f64(),
        observed.captures.load(Ordering::Relaxed) - warm_captures,
        renderer.pulled() - warm_pulled,
        diag::cpu().zip(warm_cpu).map(|(now, then)| now - then),
        diag::stats(&viewer),
    );

    // The renderer freezes: it keeps pulling but never presents.
    renderer.present(false);
    let frozen = Instant::now();
    let pulled = renderer.pulled();
    let (frozen_captures, frozen_cpu) = (observed.captures.load(Ordering::Relaxed), diag::cpu());
    let mut skipped_max = 0f32;
    let mut received_min = f32::MAX;
    // The slowest pace the host reported once its `Stats` began.
    let mut slowest = u32::MAX;
    let mut sample = |viewer: &Viewer| {
        let pace = viewer.status().stats.pace_fps;
        if pace > 0 {
            slowest = slowest.min(pace);
        }
    };
    sample(&viewer);
    // The host lets go of the held button once the presentation is stale.
    until(Duration::from_secs(3), || {
        !observed.held.load(Ordering::Acquire)
    })
    .await;
    let released = frozen.elapsed();
    assert!(released < Duration::from_millis(2500), "{released:?}");
    // Cursor updates still arrive.
    let seq = viewer.view().cursor_seq;
    cursor.store(17, Ordering::Relaxed);
    until(Duration::from_secs(3), || {
        let view = viewer.view();
        view.cursor_seq != seq && view.cursor_x == 17
    })
    .await;
    wait(&viewer, |status| !status.fresh).await;
    assert!(matches!(
        viewer.input(controlled.generation, &SHIFT),
        Err(InputError::NotFresh)
    ));
    while frozen.elapsed() < Duration::from_secs(5) {
        sample(&viewer);
        let stats = viewer.status().stats;
        skipped_max = skipped_max.max(stats.skipped_per_s);
        received_min = received_min.min(stats.received_fps);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Video keeps arriving at the host's pace, and the session goes on: a
    // frozen renderer costs neither credit nor the connection.
    let arrived = renderer.pulled() - pulled;
    // DIAGNOSTIC (temporary): where the units went, printed past the capture.
    let stamps = observed.stamps.lock().unwrap().clone();
    let intervals: Vec<Duration> = stamps
        .windows(2)
        .filter(|pair| pair[0] >= frozen)
        .map(|pair| pair[1] - pair[0])
        .collect();
    let late = intervals
        .iter()
        .filter(|gap| **gap > Duration::from_millis(25))
        .count();
    diag::say(format_args!("frozen-test {presenting}"));
    diag::say(format_args!(
        "frozen-test frozen {:.2}s: captures +{} pulled +{arrived} pace {slowest} skipped_per_s max {skipped_max:.1} received_fps min {received_min:.1} cpu {:?} thread-C qos {:#x} capture intervals [{}] over 25ms {late} {}",
        frozen.elapsed().as_secs_f64(),
        observed.captures.load(Ordering::Relaxed) - frozen_captures,
        diag::cpu().zip(frozen_cpu).map(|(now, then)| now - then),
        observed.qos.load(Ordering::Relaxed),
        diag::spread(intervals),
        diag::stats(&viewer),
    ));
    assert!(
        slowest < u32::MAX && arrived >= 4 * slowest as usize,
        "{arrived} units at pace {slowest}"
    );
    let status = viewer.status();
    assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
    assert!(status.controlling);
    assert_eq!(observed.inputs.load(Ordering::Acquire), 1);

    // Presenting a frame again restores input.
    renderer.present(true);
    let fresh = wait(&viewer, |status| status.fresh && status.controlling).await;
    viewer.input(fresh.generation, &SHIFT).unwrap();
    until(Duration::from_secs(3), || {
        observed.inputs.load(Ordering::Acquire) == 2
    })
    .await;
    drop(renderer);
    viewer.stop().await.unwrap();
    host.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_stops_video_and_resume_starts_with_an_idr() {
    let relay = Relay::start("desktop-pause").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
    let observed = Arc::new(Observation::default());
    let cursor = Arc::new(AtomicU32::new(0));
    let host = encoded_host(&client, "desktop:pause", &observed, &cursor, 4096).await;
    let shape = Arc::new(Shape::default());
    let link = shaped_link(relay.addr(), shape.clone()).await;
    let remote = Client::from_credential(link.addr().to_string(), admin_credential(), true, None);
    let viewer = relay_viewer(&remote, &host, VideoCodec::H264).await;
    let renderer = Renderer::start(&viewer);
    viewer.request_control(true).unwrap();
    wait(&viewer, |status| status.controlling && status.fresh).await;

    viewer.pause();
    until(Duration::from_secs(3), || {
        host.status()
            .viewer
            .is_some_and(|viewer| viewer.paused && !viewer.controlling)
    })
    .await;
    let pulled = renderer.pulled();
    // A slower link shows in the host's probe round trip only if the paused
    // viewer still echoes probes.
    shape.delay_ms.store(100, Ordering::Relaxed);
    let paused = wait(&viewer, |status| {
        status.last_video_age_ms.is_some_and(|age| age >= 2000)
            && status.stats.rtt_ms.is_some_and(|rtt| rtt >= 150)
    })
    .await;
    assert_eq!(paused.state, ViewerState::Paused);
    assert!(!paused.controlling);
    assert_eq!(host.status().state, HostState::Paused);
    assert_eq!(renderer.pulled(), pulled);

    shape.delay_ms.store(0, Ordering::Relaxed);
    viewer.resume();
    let first = renderer.unit(pulled).await;
    assert!(first.keyframe && first.discontinuity, "{first:?}");
    let resumed = wait(&viewer, |status| status.fresh).await;
    assert_eq!(resumed.state, ViewerState::Streaming);
    drop(renderer);
    viewer.stop().await.unwrap();
    host.stop().await.unwrap();
}

/// Must-fix R29: a control grant while video of the old generation is queued
/// at the host and in flight must not end the session. 64 KiB units over a
/// 100 ms round trip keep the credit window full and the hand-off queued.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_grant_with_queued_video_keeps_the_session() {
    let relay = Relay::start("desktop-grant").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
    let observed = Arc::new(Observation::default());
    let cursor = Arc::new(AtomicU32::new(0));
    let host = encoded_host(&client, "desktop:grant", &observed, &cursor, 64 * 1024).await;
    let shape = Arc::new(Shape::default());
    shape.delay_ms.store(50, Ordering::Relaxed);
    let link = shaped_link(relay.addr(), shape).await;
    let remote = Client::from_credential(link.addr().to_string(), admin_credential(), true, None);
    let viewer = relay_viewer(&remote, &host, VideoCodec::H264).await;
    let renderer = Renderer::start(&viewer);
    let start = wait(&viewer, |status| status.fresh).await;

    let mut generation = start.generation;
    for enabled in [true, false, true, false, true, false, true] {
        viewer.request_control(enabled).unwrap();
        let changed = wait(&viewer, |status| {
            status.generation > generation && status.controlling == enabled && status.fresh
        })
        .await;
        generation = changed.generation;
    }
    let status = viewer.status();
    assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
    assert_eq!(status.reason, None);
    viewer.input(generation, &SHIFT).unwrap();
    until(Duration::from_secs(3), || {
        observed.inputs.load(Ordering::Acquire) == 1
    })
    .await;
    drop(renderer);
    viewer.stop().await.unwrap();
    host.stop().await.unwrap();
}

/// DIAGNOSTIC (temporary): how late this runner's waits end, on a thread of
/// their own as thread C waits: the condition variable of the portable
/// `Waiter` for a 2 ms encoder poll and a 60 fps tick, and a 2 ms sleep;
/// plain, under the activity a capturing session holds on macOS, and at the
/// user-initiated and user-interactive QoS classes.
#[test]
fn pacing_probe() {
    fn measure(label: &'static str, class: Option<u32>) {
        std::thread::spawn(move || {
            if let Some(class) = class {
                diag::set_qos(class);
            }
            let pair = (parking_lot::Mutex::new(false), parking_lot::Condvar::new());
            let wait = |period: Duration| {
                let deadline = Instant::now() + period;
                let mut flag = pair.0.lock();
                while !*flag {
                    if pair.1.wait_until(&mut flag, deadline).timed_out() {
                        break;
                    }
                }
                drop(flag);
                Instant::now().saturating_duration_since(deadline)
            };
            let poll: Vec<Duration> = (0..60).map(|_| wait(Duration::from_millis(2))).collect();
            let tick: Vec<Duration> = (0..30)
                .map(|_| wait(Duration::from_micros(16_667)))
                .collect();
            let sleep: Vec<Duration> = (0..60)
                .map(|_| {
                    let at = Instant::now();
                    std::thread::sleep(Duration::from_millis(2));
                    at.elapsed().saturating_sub(Duration::from_millis(2))
                })
                .collect();
            diag::say(format_args!(
                "probe {label}: qos {:#x} {} condvar 2ms late [{}] condvar 16.7ms late [{}] sleep 2ms late [{}]",
                diag::qos(),
                diag::background(),
                diag::spread(poll),
                diag::spread(tick),
                diag::spread(sleep),
            ));
        })
        .join()
        .unwrap();
    }
    measure("plain", None);
    #[cfg(target_os = "macos")]
    {
        let activity = diag::Activity::begin();
        measure("activity", None);
        drop(activity);
    }
    measure("user-initiated", Some(0x19));
    measure("user-interactive", Some(0x21));
}

/// DIAGNOSTIC (temporary): measurements printed past the harness's capture.
mod diag {
    use std::{io::Write, time::Duration};

    pub fn say(line: std::fmt::Arguments<'_>) {
        let _ = writeln!(std::io::stderr(), "pacing-diag {line}");
    }

    pub fn spread(mut samples: Vec<Duration>) -> String {
        if samples.is_empty() {
            return "n=0".into();
        }
        samples.sort();
        let at =
            |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1e3;
        format!(
            "n={} p50={:.2}ms p90={:.2}ms p99={:.2}ms max={:.2}ms",
            samples.len(),
            at(0.5),
            at(0.9),
            at(0.99),
            at(1.0)
        )
    }

    pub fn stats(viewer: &pb_mapper_desktop::Viewer) -> String {
        let stats = viewer.status().stats;
        format!(
            "stats capture_fps {:.1} received_fps {:.1} pace {} target {} cap {} skipped_per_s {:.1} rtt {:?} base_rtt {:?} queue {:?} ack_queue {:?} backlog_drops {} dropped {}",
            stats.capture_fps,
            stats.received_fps,
            stats.pace_fps,
            stats.target_fps,
            stats.fps_cap,
            stats.skipped_per_s,
            stats.rtt_ms,
            stats.base_rtt_ms,
            stats.queue_delay_ms,
            stats.ack_queue_ms,
            stats.backlog_drops,
            stats.dropped_frames,
        )
    }

    #[cfg(unix)]
    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }
    #[cfg(unix)]
    unsafe extern "C" {
        fn clock_gettime(clock: i32, time: *mut Timespec) -> i32;
    }

    /// This process's CPU time.
    pub fn cpu() -> Option<Duration> {
        #[cfg(target_os = "macos")]
        const PROCESS: i32 = 12;
        #[cfg(all(unix, not(target_os = "macos")))]
        const PROCESS: i32 = 2;
        #[cfg(unix)]
        {
            let mut time = Timespec { sec: 0, nsec: 0 };
            // SAFETY: a local the call fills.
            (unsafe { clock_gettime(PROCESS, &mut time) } == 0)
                .then(|| Duration::new(time.sec as u64, time.nsec as u32))
        }
        #[cfg(not(unix))]
        None
    }

    #[cfg(target_os = "macos")]
    unsafe extern "C" {
        fn qos_class_self() -> u32;
        fn pthread_set_qos_class_self_np(class: u32, relative: i32) -> i32;
        fn getpriority(which: i32, who: u32) -> i32;
    }

    /// The calling thread's QoS class (0x21 user-interactive, 0x19
    /// user-initiated, 0x15 default, 0x11 utility, 0x09 background, 0
    /// unspecified).
    pub fn qos() -> u32 {
        #[cfg(target_os = "macos")]
        // SAFETY: reads the calling thread's class.
        return unsafe { qos_class_self() };
        #[cfg(not(target_os = "macos"))]
        0
    }

    pub fn set_qos(class: u32) {
        #[cfg(target_os = "macos")]
        // SAFETY: sets the calling thread's class.
        unsafe {
            pthread_set_qos_class_self_np(class, 0);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = class;
    }

    /// Whether the process and this thread run in Darwin's background state.
    pub fn background() -> String {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: reads this process's and thread's state.
            let (process, thread) =
                unsafe { (getpriority(4, std::process::id()), getpriority(3, 0)) };
            format!("darwin-bg process {process:#x} thread {thread:#x}")
        }
        #[cfg(not(target_os = "macos"))]
        String::new()
    }

    /// The activity a capturing session holds on macOS.
    #[cfg(target_os = "macos")]
    pub struct Activity(
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>,
    );
    #[cfg(target_os = "macos")]
    impl Activity {
        pub fn begin() -> Self {
            use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
            let options = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep
                | NSActivityOptions::IdleDisplaySleepDisabled
                | NSActivityOptions::LatencyCritical;
            let reason = NSString::from_str("pacing probe");
            Self(NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &reason))
        }
    }
    #[cfg(target_os = "macos")]
    impl Drop for Activity {
        fn drop(&mut self) {
            // SAFETY: the token beginActivity returned, ended once.
            unsafe { objc2_foundation::NSProcessInfo::processInfo().endActivity(&self.0) };
        }
    }
}
