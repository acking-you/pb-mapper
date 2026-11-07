//! Explicit acceptance over a real relay and TLS, with a synthetic
//! asynchronous encoder and a simulated decoder: pipeline observations, not a
//! GPU benchmark or real rendered fps. Each case shapes the viewer's link to
//! the relay and reads the host's once-a-second `desktop_feedback_interval`
//! trace. The cases run one at a time. Run with:
//! cargo test -p pb-mapper-desktop -- --ignored --nocapture slow_link
#![allow(clippy::unwrap_used)]
use crate::*;
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use pb_mapper_testkit::{Relay, Shape, admin_credential, shaped_link};
use std::{
    collections::{BTreeSet, VecDeque},
    future::Future,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tracing_subscriber::{Layer, filter::Targets, layer::SubscriberExt};

/// Balanced's start at 1080p and 60 fps: 66 667 bits per frame (perf 5.9.1).
const START_BPS: u64 = 4_000_000;
/// Balanced's ceiling under the viewer's 60 fps limit.
const CEILING_FPS: u64 = 60;
/// The synthetic encoder's latency, and when an output left in it is lost.
const ENCODE_TIME: Duration = Duration::from_millis(8);
const ENCODER_EXPIRY: Duration = Duration::from_millis(250);
const FEEDBACK: &str = "pb_mapper_desktop::feedback";

/// One host stats interval, as `desktop_feedback_interval` traced it.
#[derive(Clone, Copy, Debug)]
struct Interval {
    at: Instant,
    feedback: bool,
    /// The delay controller's fps, before the viewer cap clamps the pace.
    fps: u64,
    pace_fps: u64,
    /// Bits per frame × pace.
    bitrate: u64,
    encoder_bitrate: u64,
    viewer_cap: Option<u64>,
    window_clamped: bool,
    unresponsive: bool,
    timestamp_model: bool,
    skipped_ticks: u64,
    /// IDRs the host produced since the session began.
    keyframes: u64,
}
impl Interval {
    fn new(at: Instant) -> Self {
        Self {
            at,
            feedback: false,
            fps: 0,
            pace_fps: 0,
            bitrate: 0,
            encoder_bitrate: 0,
            viewer_cap: None,
            window_clamped: false,
            unresponsive: false,
            timestamp_model: false,
            skipped_ticks: 0,
            keyframes: 0,
        }
    }
}
impl tracing::field::Visit for Interval {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "event" {
            self.feedback = value == "desktop_feedback_interval";
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "fps" => self.fps = value,
            "pace_fps" => self.pace_fps = value,
            "bitrate" => self.bitrate = value,
            "encoder_bitrate" => self.encoder_bitrate = value,
            "viewer_cap" => self.viewer_cap = Some(value),
            "skipped_ticks" => self.skipped_ticks = value,
            "keyframes" => self.keyframes = value,
            _ => {}
        }
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        match field.name() {
            "window_clamped" => self.window_clamped = value,
            "unresponsive" => self.unresponsive = value,
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "rate_model" {
            self.timestamp_model = format!("{value:?}") == "Timestamp";
        }
    }
}

/// The host's feedback trace of one case.
#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Vec<Interval>>>);
impl Trace {
    fn last(&self) -> Interval {
        *self.0.lock().unwrap().last().unwrap()
    }
    /// The intervals traced in `from..to`.
    fn between(&self, from: Instant, to: Instant) -> Vec<Interval> {
        let intervals = self.0.lock().unwrap();
        intervals
            .iter()
            .filter(|interval| (from..to).contains(&interval.at))
            .copied()
            .collect()
    }
}
impl<S: tracing::Subscriber> Layer<S> for Trace {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut interval = Interval::new(Instant::now());
        event.record(&mut interval);
        if interval.feedback {
            self.0.lock().unwrap().push(interval);
        }
    }
}

/// Run one case on its own runtime, every thread of which traces the host's
/// feedback into the case's `Trace`. The cases measure timing, so they never
/// share the machine with one another.
fn traced<F: Future<Output = ()>>(case: impl FnOnce(Trace) -> F) {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let trace = Trace::default();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry().with(
            trace
                .clone()
                .with_filter(Targets::new().with_target(FEEDBACK, tracing::Level::DEBUG)),
        ),
    );
    let threads = dispatch.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        // The default lasts as long as the thread, and the runtime's threads
        // end with the runtime.
        .on_thread_start(move || std::mem::forget(tracing::dispatcher::set_default(&threads)))
        .build()
        .unwrap();
    tracing::dispatcher::with_default(&dispatch, || runtime.block_on(case(trace)));
}

#[derive(Default)]
struct Probe {
    captures: AtomicU64,
    configured: AtomicU32,
    expired: AtomicU32,
    held: AtomicBool,
}

/// What the synthetic encoder's pictures weigh.
#[derive(Clone, Copy)]
enum Sizes {
    /// The per-frame budget `bitrate / nominal_fps`, and an IDR `idr` times it.
    Budget { idr: usize },
    /// Every picture, IDR or not.
    Fixed(usize),
}
/// A desktop's usual IDR, four times a P-frame.
const TYPICAL: Sizes = Sizes::Budget { idr: 4 };

/// A synthetic asynchronous H.264 encoder at 1080p, run as thread C runs the
/// Windows MFT: each capture submits one picture, whose output is ready
/// `ENCODE_TIME` later and must be drained; an output left in it longer than
/// `ENCODER_EXPIRY` is lost. No decoder reads the bytes.
struct SyntheticEncoder {
    probe: Arc<Probe>,
    sizes: Sizes,
    nominal_fps: u32,
    frames: u64,
    pending: VecDeque<(Instant, EncodedFrame)>,
}
impl SyntheticEncoder {
    fn ready(&mut self) -> Option<EncodedFrame> {
        while let Some((submitted, _)) = self.pending.front() {
            let age = submitted.elapsed();
            if age < ENCODE_TIME {
                return None;
            }
            let (_, frame) = self.pending.pop_front()?;
            if age <= ENCODER_EXPIRY {
                return Some(frame);
            }
            self.probe.expired.fetch_add(1, Ordering::Relaxed);
        }
        None
    }
}
impl Backend for SyntheticEncoder {
    fn configure_video(&mut self, limits: VideoLimits, nominal_fps: u32) -> Result<()> {
        limits.validate()?;
        self.nominal_fps = nominal_fps;
        self.probe.configured.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn display(&mut self) -> Result<Display> {
        Ok(Display {
            id: 1,
            name: "synthetic transport load".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        Err(error("the synthetic encoder produces H.264 only"))
    }
    fn capture_video(
        &mut self,
        _: &DesktopContext,
        _: VideoCodec,
        keyframe: bool,
        rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        let ready = self.ready();
        self.frames += 1;
        self.probe.captures.fetch_add(1, Ordering::Relaxed);
        let budget = rate.bitrate as usize / self.nominal_fps.max(1) as usize / 8;
        let bytes = match self.sizes {
            Sizes::Budget { idr } if keyframe => idr * budget,
            Sizes::Budget { .. } => budget,
            Sizes::Fixed(bytes) => bytes,
        }
        .clamp(256, wire::MAX_PAYLOAD);
        let frame = EncodedFrame {
            format: VideoFormat {
                codec: VideoCodec::H264,
                width: 1920,
                height: 1080,
                keyframe,
                pts_us: self.frames * 16_667,
                ..VideoFormat::default()
            },
            bytes: vec![0x65; bytes],
        };
        self.pending.push_back((Instant::now(), frame));
        Ok(ready)
    }
    fn drain_video(&mut self, _: &DesktopContext, _: Duration) -> Result<Option<EncodedFrame>> {
        Ok(self.ready())
    }
    fn pending_video(&self) -> usize {
        self.pending.len()
    }
    fn input(&mut self, _: &Input) -> Result<()> {
        self.probe.held.store(true, Ordering::Release);
        Ok(())
    }
    fn release(&mut self) {
        self.probe.held.store(false, Ordering::Release);
    }
}

/// A simulated decoder on its own thread, as a native consumer runs: it pulls
/// a unit, spends `decode` on it, then presents it.
struct Decoder {
    stop: Arc<AtomicBool>,
    sizes: Arc<Mutex<BTreeSet<(u32, u32)>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Decoder {
    fn start(viewer: &Viewer, decode: Duration) -> Self {
        let consumer = viewer.video_sink().attach().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let sizes = Arc::new(Mutex::new(BTreeSet::new()));
        let (done, seen) = (stop.clone(), sizes.clone());
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                match consumer.next(Duration::from_millis(50), wire::MAX_PAYLOAD) {
                    Pull::Unit(unit) => {
                        seen.lock().unwrap().insert((unit.width, unit.height));
                        std::thread::sleep(decode);
                        consumer.presented(unit.generation, unit.frame_id);
                    }
                    Pull::Closed => break,
                    _ => {}
                }
            }
        });
        Self {
            stop,
            sizes,
            thread: Some(thread),
        }
    }
    fn sizes(&self) -> Vec<(u32, u32)> {
        self.sizes.lock().unwrap().iter().copied().collect()
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

/// A relay, a host of the synthetic encoder, and a viewer that reaches it
/// through the relay, over a shaped link when there is a `shape`.
struct Rig {
    host: Host,
    viewer: Viewer,
    probe: Arc<Probe>,
    _clients: [Client; 2],
    _relay: Relay,
}
impl Rig {
    async fn start(name: &str, shape: Option<Arc<Shape>>, sizes: Sizes) -> Self {
        let relay = Relay::start(&format!("desktop-{name}")).await;
        let client = |addr: SocketAddr| {
            Client::from_credential(addr.to_string(), admin_credential(), true, None)
        };
        let local = client(relay.addr());
        let probe = Arc::new(Probe::default());
        let source = probe.clone();
        let host = Host::start_with_backend(
            &local,
            HostOptions {
                service: format!("desktop:{name}"),
                display_id: 1,
                allow_control: true,
                allow_direct: false,
            },
            Arc::new(move || {
                Ok(Box::new(SyntheticEncoder {
                    probe: source.clone(),
                    sizes,
                    nominal_fps: 0,
                    frames: 0,
                    pending: VecDeque::new(),
                }))
            }),
        )
        .await
        .unwrap();
        let remote = client(match shape {
            Some(shape) => shaped_link(relay.addr(), shape).await,
            None => relay.addr(),
        });
        let viewer = Viewer::connect(
            &remote,
            host.invitation(),
            ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..Default::default()
            },
            ViewerConfig {
                codecs: vec![VideoCodec::H264],
                limits: VideoLimits::for_viewer(60.0),
                quality: QualityPreset::Balanced,
                progress: None,
            },
        )
        .await
        .unwrap();
        Self {
            host,
            viewer,
            probe,
            _clients: [local, remote],
            _relay: relay,
        }
    }
    fn expired(&self) -> u32 {
        self.probe.expired.load(Ordering::Relaxed)
    }
    async fn stop(&self) {
        self.viewer.stop().await.unwrap();
        self.host.stop().await.unwrap();
    }
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// A fixed propagation delay is not congestion: the delay controller climbs
/// to the ceiling, bits per frame never fall, and nothing rebuilds the
/// encoder or changes its size.
#[test]
#[ignore = "explicit ~20-second 350 ms WAN acceptance over a real relay and TLS"]
fn fixed_wan_keeps_full_rate_at_fixed_resolution() {
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape.delay_ms.store(175, Ordering::Relaxed);
        let rig = Rig::start("fixed-wan", Some(shape), TYPICAL).await;
        let decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        tokio::time::sleep(Duration::from_secs(20)).await;
        let status = rig.viewer.status();
        let last = trace.last();
        rig.stop().await;
        let sizes = decoder.sizes();
        let configured = rig.probe.configured.load(Ordering::Relaxed);
        println!(
            "fixed_wan roundtrip_ms=350 pace_fps={} bitrate={} encoder_bitrate={} presented_fps={:.1} received_fps={:.1} rtt_ms={:?} keyframes={} sizes={sizes:?} configured={configured} expired_encoder_outputs={} state={:?}",
            last.pace_fps,
            last.bitrate,
            last.encoder_bitrate,
            status.stats.presented_fps,
            status.stats.received_fps,
            status.stats.rtt_ms,
            status.stats.keyframes,
            rig.expired(),
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert_eq!(
            last.pace_fps, CEILING_FPS,
            "a fixed delay is not congestion"
        );
        assert!(last.bitrate >= START_BPS, "{last:?}");
        assert_eq!(sizes, [(1920, 1080)]);
        assert_eq!(configured, 1, "the encoder is never rebuilt");
        assert_eq!(rig.expired(), 0);
    });
}

/// A 1 Mbit/s link for 8 s, then an unlimited one: the delivery rate clamps
/// the window at once, the probe lowers fps and bits per frame, and both
/// recover once the link does. The encoder is drained throughout.
#[test]
#[ignore = "explicit ~45-second bandwidth limit and recovery acceptance"]
fn bandwidth_limit_lowers_fps_and_bitrate_then_recovers() {
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        let rig = Rig::start("bandwidth", Some(shape.clone()), TYPICAL).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        tokio::time::sleep(Duration::from_secs(6)).await;
        let before = trace.last();
        let limited = Instant::now();
        shape.rate.store(125_000, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(8)).await;
        shape.rate.store(0, Ordering::Relaxed);
        let lifted = Instant::now();
        let recovered = loop {
            let recovered = trace
                .between(lifted, Instant::now())
                .into_iter()
                .find(|interval| interval.bitrate * 5 >= START_BPS * 4);
            if recovered.is_some() || lifted.elapsed() > Duration::from_secs(30) {
                break recovered.map(|interval| interval.at - lifted);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        let status = rig.viewer.status();
        rig.stop().await;
        let limit = trace.between(limited, lifted);
        let clamped = limit
            .iter()
            .find(|interval| interval.window_clamped)
            .map(|interval| interval.at - limited);
        let lowest = |value: fn(&Interval) -> u64| limit.iter().map(value).min().unwrap();
        let lowest_pace = lowest(|interval| interval.pace_fps);
        let lowest_bitrate = lowest(|interval| interval.bitrate);
        let lowest_encoder = lowest(|interval| interval.encoder_bitrate);
        println!(
            "bandwidth_limit link_mbps=1 limit_s=8 before_pace={} before_bitrate={} before_encoder_bitrate={} clamped_after_ms={:?} lowest_pace={lowest_pace} lowest_bitrate={lowest_bitrate} lowest_encoder_bitrate={lowest_encoder} recovered_after_ms={:?} final_pace={} final_bitrate={} expired_encoder_outputs={} state={:?}",
            before.pace_fps,
            before.bitrate,
            before.encoder_bitrate,
            clamped.map(|after| after.as_millis()),
            recovered.map(|after| after.as_millis()),
            status.stats.pace_fps,
            status.stats.bitrate,
            rig.expired(),
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        // The trace samples once a second, so this is "within 1 s" to its
        // resolution.
        assert!(
            clamped.is_some_and(|after| after <= Duration::from_secs(1) + session::STATS_INTERVAL),
            "the window must clamp within 1 s of the limit: {clamped:?}"
        );
        assert!(lowest_pace <= 15, "{lowest_pace}");
        // Bits per frame may have risen past the start by the time the limit
        // begins, so the fall is judged against where the limit found them.
        assert!(
            lowest_encoder < before.encoder_bitrate,
            "bits per frame must fall below where the limit found them: {before:?}"
        );
        assert!(
            recovered.is_some(),
            "the bitrate must recover to 80% of the start within 30 s"
        );
        assert_eq!(
            rig.expired(),
            0,
            "a full window must never starve the encoder"
        );
    });
}

/// A 1 Mbit/s link from the start, with IDRs twenty times a P-frame: each
/// takes over a second to cross. The P-frames behind the session's first IDR
/// wait for it rather than age out into another, the viewer does not ask for
/// it again, and fps and bits per frame fall as on any slow link.
#[test]
#[ignore = "explicit ~15-second slow IDR acceptance"]
fn idr_slower_than_a_second_is_followed_not_replaced() {
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape.rate.store(125_000, Ordering::Relaxed);
        let rig = Rig::start("slow-idr", Some(shape), Sizes::Budget { idr: 20 }).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        let started = Instant::now();
        tokio::time::sleep(Duration::from_secs(15)).await;
        let status = rig.viewer.status();
        let intervals = trace.between(started, Instant::now());
        rig.stop().await;
        let last = *intervals.last().unwrap();
        let lowest_encoder = intervals
            .iter()
            .map(|interval| interval.encoder_bitrate)
            .min()
            .unwrap();
        println!(
            "slow_idr link_mbps=1 idr_kib={} keyframes={} received_keyframes={} keyframes_requested={} lowest_encoder_bitrate={lowest_encoder} final_pace={} received_fps={:.1} presented_fps={:.1} state={:?}",
            20 * START_BPS / CEILING_FPS / 8 / 1024,
            last.keyframes,
            status.stats.keyframes,
            status.stats.keyframes_requested,
            last.pace_fps,
            status.stats.received_fps,
            status.stats.presented_fps,
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        // The IDR's header lands well inside the viewer's 1 s hold and
        // answers it, so nothing asks for another; the P-frames behind it
        // must not age out into one either.
        assert_eq!(
            status.stats.keyframes_requested, 0,
            "the IDR in flight answers the hold"
        );
        assert_eq!(
            last.keyframes, 1,
            "the IDR in flight is the resync; the P-frames behind it must not age out into another: {last:?}"
        );
        assert!(lowest_encoder < START_BPS, "bits per frame must fall");
        assert!(
            status.stats.received_fps >= 2.0,
            "P-frames must follow the IDR: {:.1} fps",
            status.stats.received_fps
        );
    });
}

/// A decoder that takes 67 ms per frame: the viewer's fps cap brings the host
/// down to what it presents, while the host's own controller, its bits per
/// frame and its rate model stay where the network puts them.
#[test]
#[ignore = "explicit ~20-second slow presenter acceptance"]
fn slow_presenter_is_capped_by_fps_cap_not_host_backpressure() {
    traced(|trace| async move {
        let rig = Rig::start("slow-presenter", None, TYPICAL).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::from_millis(67));
        tokio::time::sleep(Duration::from_secs(20)).await;
        let measured = Instant::now();
        let status = rig.viewer.status();
        rig.stop().await;
        let steady = trace.between(measured - Duration::from_secs(10), measured);
        let mean_pace = steady.iter().map(|interval| interval.pace_fps).sum::<u64>() as f64
            / steady.len() as f64;
        let last = *steady.last().unwrap();
        println!(
            "slow_presenter decode_ms=67 mean_pace_fps={mean_pace:.1} caps={:?} fps={} encoder_bitrate={} presented_fps={:.1} backlog_drops={} keyframes_requested={} expired_encoder_outputs={} state={:?}",
            steady
                .iter()
                .map(|interval| interval.viewer_cap)
                .collect::<Vec<_>>(),
            last.fps,
            last.encoder_bitrate,
            status.stats.presented_fps,
            status.stats.backlog_drops,
            status.stats.keyframes_requested,
            rig.expired(),
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert!(
            steady.iter().all(|interval| interval.viewer_cap.is_some()),
            "the fps cap must hold the host down"
        );
        assert!((10.0..=16.0).contains(&mean_pace), "{mean_pace}");
        assert_eq!(last.fps, CEILING_FPS, "the network is not congested");
        assert!(
            last.encoder_bitrate >= START_BPS && !last.timestamp_model,
            "a slow presenter must not lower the host's bitrate: {last:?}"
        );
        assert!(steady.iter().all(|interval| !interval.unresponsive));
        assert_eq!(rig.expired(), 0);
    });
}

/// Poll `condition` for up to one second.
async fn observed(label: &str, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label} was not observed within one second"));
}

/// 30 s on a 256 KiB/s link far below the 64 KiB pictures at the pace: the
/// session stays up, capture holds back instead of queueing, control and
/// input never wait behind video, and a local stop is prompt.
#[test]
#[ignore = "explicit 30-second slow TLS transport acceptance; no GPU or real input"]
fn sustained_slow_tls_reader() {
    const FRAME_BYTES: usize = 64 * 1024;
    const LINK_BYTES_PER_SECOND: u64 = 256 * 1024;
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape.rate.store(LINK_BYTES_PER_SECOND, Ordering::Relaxed);
        let rig = Rig::start("slow-tls", Some(shape), Sizes::Fixed(FRAME_BYTES)).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        let (host, viewer, probe) = (&rig.host, &rig.viewer, &rig.probe);
        let start = Instant::now();
        let mut tick = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let mut control_ms = 0.0;
        let mut release_ms = 0.0;
        let mut revoke_ms = 0.0;
        let mut halfway = 0;
        for second in 1..=30 {
            tick.tick().await;
            let status = viewer.status();
            assert_eq!(
                (host.status().state, status.state),
                (HostState::Streaming, ViewerState::Streaming),
                "the session must stay up for all 30 seconds: {}",
                status.message
            );
            if second == 15 {
                halfway = probe.captures.load(Ordering::Relaxed);
                assert!(
                    trace.last().skipped_ticks > 0,
                    "the slow link must hold capture back"
                );
                let at = Instant::now();
                viewer.request_control(true).unwrap();
                observed("control enabled", || {
                    host.status()
                        .viewer
                        .is_some_and(|viewer| viewer.controlling)
                })
                .await;
                control_ms = ms(at);
                for revoke in [false, true] {
                    // The synthetic backend injects no OS input.
                    let fresh = tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            let status = viewer.status();
                            if status.controlling && status.fresh {
                                return status;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    let press = Input::Button {
                        button: Button::Left,
                        down: true,
                        drag_start: false,
                    };
                    viewer.input(fresh.generation, &[press]).unwrap();
                    observed("synthetic input", || probe.held.load(Ordering::Acquire)).await;
                    let at = Instant::now();
                    if revoke {
                        host.set_control_allowed(false);
                    } else {
                        viewer.release_input().unwrap();
                    }
                    observed("release or revoke", || !probe.held.load(Ordering::Acquire)).await;
                    if revoke {
                        revoke_ms = ms(at);
                    } else {
                        release_ms = ms(at);
                    }
                }
            }
            if second % 10 == 0 {
                let last = trace.last();
                println!(
                    "slow_tls seconds={second} captured={} skipped_ticks={} pace_fps={} received_kbit_s={}",
                    probe.captures.load(Ordering::Relaxed),
                    last.skipped_ticks,
                    last.pace_fps,
                    status.stats.received_bitrate / 1000
                );
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        let captured = probe.captures.load(Ordering::Relaxed);
        assert!(
            captured > halfway + 30,
            "capture must continue after control revocation"
        );
        assert!(
            host.status()
                .viewer
                .is_some_and(|viewer| !viewer.controlling)
        );
        let stopping = Instant::now();
        tokio::time::timeout(Duration::from_secs(2), host.stop())
            .await
            .unwrap()
            .unwrap();
        let stop_ms = ms(stopping);
        viewer.stop().await.unwrap();
        println!(
            "slow_tls elapsed_s={elapsed:.3} link_kib_s={} frame_bytes={FRAME_BYTES} captured={captured} skipped_ticks={} control_ms={control_ms:.3} release_ms={release_ms:.3} revoke_ms={revoke_ms:.3} stop_ms={stop_ms:.3}",
            LINK_BYTES_PER_SECOND / 1024,
            trace.last().skipped_ticks
        );
    });
}
