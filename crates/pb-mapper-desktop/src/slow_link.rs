//! Explicit acceptance over a real relay and TLS, with a synthetic
//! asynchronous encoder and a simulated decoder: pipeline observations, not a
//! GPU benchmark or real rendered fps. Each case shapes the viewer's link to
//! the relay and reads the host's once-a-second `desktop_feedback_interval`
//! trace. The cases run one at a time. Run with:
//! cargo test -p pb-mapper-desktop -- --ignored --nocapture slow_link
use crate::*;
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use pb_mapper_testkit::{Relay, Shape, ShapedLink, admin_credential, shaped_link};
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
/// The synthetic encoder's usual latency, and when an output left in it is
/// lost.
const ENCODE_TIME: Duration = Duration::from_millis(8);
const ENCODER_EXPIRY: Duration = Duration::from_millis(250);
/// A LAN or nearby direct path, one way.
const SHORT_PATH_MS: u64 = 18;
const FEEDBACK: &str = "pb_mapper_desktop::feedback";
/// The highest pace the rate-model latch looks at: 0.75 × the nominal 60.
const LATCH_PACE: u64 = (qos::LATCH_PACE_RATIO * CEILING_FPS as f64) as u64;
/// How soon after the pace falls to `LATCH_PACE` an encoder that budgets per
/// second must be latched to the timestamp model.
const LATCH_WITHIN: Duration = Duration::from_millis(2500);

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
    /// What the host wrote in the interval, in bit/s.
    sent_bitrate: u64,
    window_clamped: bool,
    unresponsive: bool,
    timestamp_model: bool,
    skipped_ticks: u64,
    /// IDRs the host produced since the session began.
    keyframes: u64,
    /// Not an interval: the host's `desktop_rate_model_latched`.
    latched: bool,
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
            sent_bitrate: 0,
            window_clamped: false,
            unresponsive: false,
            timestamp_model: false,
            skipped_ticks: 0,
            keyframes: 0,
            latched: false,
        }
    }
}
impl tracing::field::Visit for Interval {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "event" {
            self.feedback = value == "desktop_feedback_interval";
            self.latched = value == "desktop_rate_model_latched";
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "fps" => self.fps = value,
            "pace_fps" => self.pace_fps = value,
            "bitrate" => self.bitrate = value,
            "encoder_bitrate" => self.encoder_bitrate = value,
            "sent_bitrate" => self.sent_bitrate = value,
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
struct Trace {
    intervals: Arc<Mutex<Vec<Interval>>>,
    /// When the host latched the timestamp rate model.
    latched: Arc<Mutex<Option<Instant>>>,
}
impl Trace {
    fn last(&self) -> Interval {
        *self.intervals.lock().unwrap().last().unwrap()
    }
    /// The intervals traced in `from..to`.
    fn between(&self, from: Instant, to: Instant) -> Vec<Interval> {
        let intervals = self.intervals.lock().unwrap();
        intervals
            .iter()
            .filter(|interval| (from..to).contains(&interval.at))
            .copied()
            .collect()
    }
    fn latched(&self) -> Option<Instant> {
        *self.latched.lock().unwrap()
    }
    /// A per-frame budget is what the session starts from: nothing in its
    /// trace may latch the timestamp model.
    fn assert_never_latched(&self) {
        assert_eq!(
            self.latched(),
            None,
            "a per-frame budget must never latch the timestamp model"
        );
    }
}
impl<S: tracing::Subscriber> Layer<S> for Trace {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut interval = Interval::new(Instant::now());
        event.record(&mut interval);
        if interval.feedback {
            self.intervals.lock().unwrap().push(interval);
        } else if interval.latched {
            self.latched.lock().unwrap().get_or_insert(interval.at);
        }
    }
}

/// How a case's rate model went from the instant that lowers its pace: its
/// first capture, or a link's limit.
struct RateModelRun {
    /// From that instant until the host latched the timestamp model.
    latched_after: Option<Duration>,
    /// From the start of the stretch at a pace of `LATCH_PACE` or below the
    /// latch came in: the pace the encoder was asked for, each change timed at
    /// the capture that first asked for it. A capture that waits for credit
    /// can keep the encoder from seeing a pace at all.
    latched_in_stretch: Option<Duration>,
    /// The mean sent bitrate over the expected one in the trace intervals at a
    /// pace of `LATCH_PACE` or below: from that instant until the latch, or
    /// the end, and after the latch.
    before: Option<f64>,
    after: Option<f64>,
}
impl RateModelRun {
    fn read(trace: &Trace, probe: &Probe, from: Instant, ended: Instant) -> Self {
        let latched = trace.latched();
        // Each stretch of the pace at `LATCH_PACE` or below, as `start..end`.
        let mut low: Vec<(Instant, Instant)> = Vec::new();
        let mut since = None;
        for (at, pace) in probe.paces.lock().unwrap().iter() {
            match (u64::from(*pace) <= LATCH_PACE, since) {
                (true, None) => since = Some(*at),
                (false, Some(start)) => {
                    low.push((start, *at));
                    since = None;
                }
                _ => {}
            }
        }
        low.extend(since.map(|start| (start, ended)));
        // The stretch the latch came in, if the encoder had been asked for
        // such a pace by then.
        let stretch = latched.and_then(|latched| {
            low.iter()
                .find(|(start, end)| (*start..=*end).contains(&latched))
                .map(|(start, _)| *start)
        });
        let ratio = |intervals: Vec<Interval>| {
            let ratios: Vec<f64> = intervals
                .iter()
                .filter(|interval| interval.pace_fps <= LATCH_PACE && interval.bitrate > 0)
                .map(|interval| interval.sent_bitrate as f64 / interval.bitrate as f64)
                .collect();
            (!ratios.is_empty()).then(|| ratios.iter().sum::<f64>() / ratios.len() as f64)
        };
        // An interval's figures cover the second before it.
        let second = session::STATS_INTERVAL;
        let until = latched.unwrap_or(ended);
        Self {
            latched_after: latched.map(|latched| latched.saturating_duration_since(from)),
            latched_in_stretch: latched
                .zip(stretch)
                .map(|(latched, start)| latched.saturating_duration_since(start)),
            before: ratio(trace.between(from + second, until)),
            after: latched.and_then(|latched| ratio(trace.between(latched + second, ended))),
        }
    }
}
impl std::fmt::Display for RateModelRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ratio =
            |ratio: Option<f64>| ratio.map_or_else(|| "-".to_owned(), |r| format!("{r:.2}"));
        let ms = |after: Option<Duration>| after.map(|after| after.as_millis());
        write!(
            f,
            "latched_after_ms={:?} latched_in_low_stretch_ms={:?} sent_over_expected_before={} sent_over_expected_after={}",
            ms(self.latched_after),
            ms(self.latched_in_stretch),
            ratio(self.before),
            ratio(self.after)
        )
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
    /// Each pace the encoder was asked for, from the capture that first asked.
    paces: Mutex<Vec<(Instant, u32)>>,
}

/// What the synthetic encoder's pictures weigh.
#[derive(Clone, Copy)]
enum Sizes {
    /// The per-frame budget `bitrate / nominal_fps`, and an IDR `idr` times it.
    Budget { idr: usize },
    /// A per-second budget, spent as an encoder that rates by presentation
    /// time spends it: `bitrate` over the time since the previous capture, at
    /// most `PER_SECOND_GAP`, so a picture after idle carries the idle's bits
    /// too; an IDR `idr` times that.
    PerSecond { idr: usize },
    /// Every picture, IDR or not.
    Fixed(usize),
}
/// A desktop's usual IDR, four times a P-frame.
const TYPICAL: Sizes = Sizes::Budget { idr: 4 };
/// The same desktop from an encoder that budgets per second.
const PER_SECOND: Sizes = Sizes::PerSecond { idr: 4 };
const PER_SECOND_GAP: Duration = Duration::from_secs(1);

/// A synthetic asynchronous H.264 encoder at 1080p, run as thread C runs the
/// Windows MFT: each capture submits one picture, whose output is ready
/// `latency` later and must be drained; an output left in it longer than
/// `ENCODER_EXPIRY` is lost. No decoder reads the bytes.
struct SyntheticEncoder {
    probe: Arc<Probe>,
    sizes: Sizes,
    latency: Duration,
    nominal_fps: u32,
    frames: u64,
    last_capture: Option<Instant>,
    pending: VecDeque<(Instant, EncodedFrame)>,
}
impl SyntheticEncoder {
    fn ready(&mut self) -> Option<EncodedFrame> {
        while let Some((submitted, _)) = self.pending.front() {
            let age = submitted.elapsed();
            if age < self.latency {
                return None;
            }
            let (_, mut frame) = self.pending.pop_front()?;
            if age <= ENCODER_EXPIRY {
                // Submit to output, as the Windows encoder reports it.
                frame.format.encode_us = u32::try_from(age.as_micros()).unwrap_or(u32::MAX);
                return Some(frame);
            }
            self.probe.expired.fetch_add(1, Ordering::Relaxed);
        }
        None
    }
}
impl Backend for SyntheticEncoder {
    fn configure_video(&mut self, limits: VideoLimits, nominal_fps: u32) -> Result<()> {
        self.pending.clear();
        self.last_capture = None;
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
        let now = Instant::now();
        self.frames += 1;
        self.probe.captures.fetch_add(1, Ordering::Relaxed);
        {
            let mut paces = self.probe.paces.lock().unwrap();
            if paces.last().is_none_or(|(_, pace)| *pace != rate.pace_fps) {
                paces.push((now, rate.pace_fps));
            }
        }
        let nominal_period = Duration::from_secs(1) / self.nominal_fps.max(1);
        let gap = self
            .last_capture
            .replace(now)
            .map_or(nominal_period, |last| now - last)
            .min(PER_SECOND_GAP);
        let budget = rate.bitrate as usize / self.nominal_fps.max(1) as usize / 8;
        let per_second = (f64::from(rate.bitrate) * gap.as_secs_f64() / 8.0) as usize;
        let bytes = match self.sizes {
            Sizes::Budget { idr } if keyframe => idr * budget,
            Sizes::Budget { .. } => budget,
            Sizes::PerSecond { idr } if keyframe => idr * per_second,
            Sizes::PerSecond { .. } => per_second,
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

/// One wait for an IDR: from the request until the IDR reached the sink, and
/// the predicted frames that reached it first.
#[derive(Clone, Copy, Debug)]
struct Resync {
    waited: Duration,
    frames_first: u32,
    /// The viewer's limiter held the request back, to send it later.
    held: bool,
}

/// What a case and its `Resyncing` decoder share.
#[derive(Default)]
struct ResyncRounds {
    stop: AtomicBool,
    /// When the case began a round the decoder has yet to take up.
    begun: Mutex<Option<Instant>>,
    done: Mutex<Vec<Resync>>,
}

/// A simulated decoder like `Decoder`, without the decode time, that waits for
/// the next IDR in each round a case begins. One that `asks` requests it, as a
/// decoder missing a reference does, and discards the predicted frames that
/// arrive first; one that does not times an IDR asked for elsewhere and
/// presents what comes before it.
struct Resyncing {
    rounds: Arc<ResyncRounds>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Resyncing {
    fn start(viewer: &Viewer, asks: bool) -> Self {
        let consumer = viewer.video_sink().attach().unwrap();
        let rounds = Arc::new(ResyncRounds::default());
        let shared = rounds.clone();
        let thread = std::thread::spawn(move || {
            // The round under way: since when, its predicted frames, and
            // whether its request was held back.
            let mut open: Option<(Instant, u32, bool)> = None;
            while !shared.stop.load(Ordering::Acquire) {
                let pulled = consumer.next(Duration::from_millis(5), wire::MAX_PAYLOAD);
                // Taken up after the pull, so a unit received before the
                // round began never counts toward it.
                if open.is_none() {
                    let begun = shared.begun.lock().unwrap().take();
                    open = begun.map(|begun| {
                        if asks {
                            let asked = Instant::now();
                            (asked, 0, !consumer.keyframe(KeyframeReason::Gap))
                        } else {
                            (begun, 0, false)
                        }
                    });
                }
                match pulled {
                    Pull::Unit(unit) => {
                        if let Some((since, frames_first, held)) = &mut open
                            && unit.received_at >= *since
                        {
                            if unit.keyframe {
                                shared.done.lock().unwrap().push(Resync {
                                    waited: unit.received_at - *since,
                                    frames_first: *frames_first,
                                    held: *held,
                                });
                                open = None;
                            } else {
                                *frames_first += 1;
                                if asks {
                                    continue;
                                }
                            }
                        }
                        consumer.presented(unit.generation, unit.frame_id);
                    }
                    Pull::Closed => break,
                    _ => {}
                }
            }
        });
        Self {
            rounds,
            thread: Some(thread),
        }
    }
    /// `RESYNC_ROUNDS` rounds `RESYNC_SPACING` apart. Each begins just before
    /// `begin` runs and waits up to `RESYNC_LIMIT` for its IDR; the first that
    /// waits longer ends them. Returns the rounds answered.
    async fn run(&self, begin: impl Fn()) -> Vec<Resync> {
        for _ in 0..RESYNC_ROUNDS {
            tokio::time::sleep(RESYNC_SPACING).await;
            let answered = self.rounds.done.lock().unwrap().len();
            *self.rounds.begun.lock().unwrap() = Some(Instant::now());
            begin();
            let waited = tokio::time::timeout(RESYNC_LIMIT, async {
                while self.rounds.done.lock().unwrap().len() == answered {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await;
            if waited.is_err() {
                break;
            }
        }
        self.rounds.done.lock().unwrap().clone()
    }
}
impl Drop for Resyncing {
    fn drop(&mut self) {
        self.rounds.stop.store(true, Ordering::Release);
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
    /// The host's client, on the relay itself, and the viewer's.
    clients: [Client; 2],
    /// The shaped hop the viewer reaches the relay through, if any, held
    /// while the rig lives. The testkit does not name its type.
    _link: Option<ShapedLink>,
    _relay: Relay,
}
impl Rig {
    async fn start(name: &str, shape: Option<Arc<Shape>>, sizes: Sizes) -> Self {
        Self::with_encoder(name, shape, sizes, ENCODE_TIME).await
    }
    async fn with_encoder(
        name: &str,
        shape: Option<Arc<Shape>>,
        sizes: Sizes,
        latency: Duration,
    ) -> Self {
        let relay = Relay::start(&format!("desktop-{name}")).await;
        let client = |addr: SocketAddr| {
            Client::from_credential(addr.to_string(), admin_credential(), true, None)
        };
        let local = client(relay.addr());
        let probe = Arc::new(Probe::default());
        let source = probe.clone();
        let mut options = HostOptions::new(format!("desktop:{name}"));
        options.allow_control = true;
        options.allow_direct = false;
        let host = Host::builder(options)
            .backend(move || {
                Ok(SyntheticEncoder {
                    probe: source.clone(),
                    sizes,
                    latency,
                    nominal_fps: 0,
                    frames: 0,
                    last_capture: None,
                    pending: VecDeque::new(),
                })
            })
            .start(&local)
            .await
            .unwrap();
        let (remote, link) = match shape {
            Some(shape) => {
                let link = shaped_link(relay.addr(), shape).await;
                (client(link.addr()), Some(link))
            }
            None => (client(relay.addr()), None),
        };
        let viewer = connect_viewer(&remote, &host).await;
        Self {
            host,
            viewer,
            probe,
            clients: [local, remote],
            _link: link,
            _relay: relay,
        }
    }
    /// Another viewer of the host, on the relay's unshaped link.
    async fn second_viewer(&self) -> Viewer {
        connect_viewer(&self.clients[0], &self.host).await
    }
    fn expired(&self) -> u32 {
        self.probe.expired.load(Ordering::Relaxed)
    }
    async fn stop(&self) {
        self.viewer.stop().await.unwrap();
        self.host.stop().await.unwrap();
    }
}

/// A viewer of `host` through `client`, on the relay.
async fn connect_viewer(client: &Client, host: &Host) -> Viewer {
    Viewer::connect(
        client,
        host.invitation(),
        ConnectOptions {
            path: PathPolicy::RelayOnly,
            ..Default::default()
        },
        ViewerConfig {
            codecs: vec![VideoCodec::H264],
            limits: VideoLimits::for_viewer(60.0),
            fps_override: false,
            quality: QualityPreset::Balanced,
            progress: None,
        },
    )
    .await
    .unwrap()
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// The viewer's received fps, sampled once a second for `seconds`.
async fn received_fps(viewer: &Viewer, seconds: u64) -> Vec<f32> {
    let mut samples = Vec::new();
    for _ in 0..seconds {
        tokio::time::sleep(Duration::from_secs(1)).await;
        samples.push(viewer.status().stats.received_fps);
    }
    samples
}

fn mean(samples: &[f32]) -> f32 {
    samples.iter().sum::<f32>() / samples.len().max(1) as f32
}

/// A short fixed delay, as on a LAN or a nearby direct path: the pace reaches
/// the ceiling within 8 s, and the viewer then receives at least 58 fps.
#[test]
#[ignore = "explicit ~18-second short-path acceptance"]
fn lan_rtt_keeps_60() {
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
        let rig = Rig::start("lan-rtt", Some(shape), TYPICAL).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        let started = Instant::now();
        tokio::time::sleep(Duration::from_secs(8)).await;
        let ceiling = trace
            .between(started, Instant::now())
            .into_iter()
            .find(|interval| interval.pace_fps == CEILING_FPS)
            .map(|interval| interval.at - started);
        let received = received_fps(&rig.viewer, 10).await;
        let status = rig.viewer.status();
        rig.stop().await;
        println!(
            "lan_rtt one_way_ms={SHORT_PATH_MS} ceiling_after_ms={:?} received_fps_mean={:.1} received_fps={received:?} presented_fps={:.1} rtt_ms={:?} state={:?}",
            ceiling.map(|after| after.as_millis()),
            mean(&received),
            status.stats.presented_fps,
            status.stats.rtt_ms,
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert!(
            ceiling.is_some(),
            "the pace must reach the ceiling within 8 s"
        );
        assert!(mean(&received) >= 58.0, "{received:?}");
        trace.assert_never_latched();
    });
}

/// Wi-Fi stalls of about 600 ms on a short path, 6 s apart: each delays the
/// probes in flight, and none may collapse the frame rate. The pace never
/// falls below half its value before a stall, and is at 55 or more again
/// within 4 s of the stall ending. Three stalls, so that at least one delays a
/// probe even when they come only once a second.
#[test]
#[ignore = "explicit ~26-second isolated stall acceptance"]
fn isolated_stall_does_not_collapse_fps() {
    const STALL: Duration = Duration::from_millis(600);
    const SPACING: Duration = Duration::from_secs(6);
    const RECOVERY: Duration = Duration::from_secs(4);
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
        let rig = Rig::start("stall", Some(shape.clone()), TYPICAL).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        tokio::time::sleep(Duration::from_secs(8)).await;
        let mut stalls = Vec::new();
        for _ in 0..3 {
            let before = trace.last().pace_fps;
            // Every byte read from now on waits behind one held for the stall.
            shape
                .delay_ms
                .store(SHORT_PATH_MS + STALL.as_millis() as u64, Ordering::Relaxed);
            tokio::time::sleep(STALL).await;
            shape.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
            let ended = Instant::now();
            tokio::time::sleep(SPACING - STALL).await;
            let after = trace.between(ended - STALL, Instant::now());
            let lowest = after
                .iter()
                .map(|interval| interval.pace_fps)
                .min()
                .unwrap();
            // From the first interval after which the pace stays at 55 or more.
            let recovered = after
                .iter()
                .rposition(|interval| interval.pace_fps < 55)
                .map_or(Duration::ZERO, |low| {
                    after
                        .get(low + 1)
                        .map_or(SPACING, |back| back.at.saturating_duration_since(ended))
                });
            stalls.push((before, lowest, recovered));
        }
        let status = rig.viewer.status();
        rig.stop().await;
        println!(
            "isolated_stall one_way_ms={SHORT_PATH_MS} stall_ms={} (pace_before, lowest_pace, back_at_55_after)={stalls:?} received_fps={:.1} state={:?}",
            STALL.as_millis(),
            status.stats.received_fps,
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        for (before, lowest, recovered) in stalls {
            assert!(lowest * 2 >= before, "{lowest} is below half of {before}");
            assert!(recovered <= RECOVERY, "back at 55 only after {recovered:?}");
        }
        trace.assert_never_latched();
    });
}

/// An encoder, or a helper, that holds each picture for 45 ms: nearly three
/// periods at 60 fps. The capture gate must not halve the delivered rate.
#[test]
#[ignore = "explicit ~18-second slow encoder acceptance"]
fn encoder_latency_of_45ms_keeps_60() {
    traced(|trace| async move {
        let latency = Duration::from_millis(45);
        let rig = Rig::with_encoder("slow-encoder", None, TYPICAL, latency).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        tokio::time::sleep(Duration::from_secs(8)).await;
        let received = received_fps(&rig.viewer, 10).await;
        let status = rig.viewer.status();
        let last = trace.last();
        rig.stop().await;
        println!(
            "slow_encoder latency_ms={} received_fps_mean={:.1} received_fps={received:?} pace_fps={} capture_fps={:.1} skipped_ticks={} expired_encoder_outputs={} state={:?}",
            latency.as_millis(),
            mean(&received),
            last.pace_fps,
            status.stats.capture_fps,
            last.skipped_ticks,
            rig.expired(),
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert!(mean(&received) >= 58.0, "{received:?}");
        assert_eq!(rig.expired(), 0);
        trace.assert_never_latched();
    });
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
        trace.assert_never_latched();
    });
}

/// What one run of the bandwidth-limit case saw.
struct BandwidthRun {
    status: ViewerStatus,
    before: Interval,
    clamped: Option<Duration>,
    lowest_pace: u64,
    /// The encoder bitrate in each interval of the limit.
    encoder_trace: Vec<u64>,
    recovered: Option<Duration>,
    expired: u32,
}

/// A 1 Mbit/s link for 8 s after 6 s unlimited, then an unlimited one again,
/// for an encoder of `sizes`; printed as `case`.
async fn bandwidth_limit(trace: &Trace, case: &str, sizes: Sizes) -> BandwidthRun {
    let shape = Arc::new(Shape::default());
    let rig = Rig::start(case, Some(shape.clone()), sizes).await;
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
    let rate_model = RateModelRun::read(trace, &rig.probe, limited, Instant::now());
    let limit = trace.between(limited, lifted);
    let clamped = limit
        .iter()
        .find(|interval| interval.window_clamped)
        .map(|interval| interval.at - limited);
    let lowest = |value: fn(&Interval) -> u64| limit.iter().map(value).min().unwrap();
    let lowest_pace = lowest(|interval| interval.pace_fps);
    let lowest_bitrate = lowest(|interval| interval.bitrate);
    let lowest_encoder = lowest(|interval| interval.encoder_bitrate);
    let encoder_trace: Vec<u64> = limit
        .iter()
        .map(|interval| interval.encoder_bitrate)
        .collect();
    println!(
        "{case} link_mbps=1 limit_s=8 before_pace={} before_bitrate={} before_encoder_bitrate={} clamped_after_ms={:?} lowest_pace={lowest_pace} lowest_bitrate={lowest_bitrate} lowest_encoder_bitrate={lowest_encoder} recovered_after_ms={:?} final_pace={} final_bitrate={} expired_encoder_outputs={} {rate_model} state={:?}",
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
    BandwidthRun {
        status,
        before,
        clamped,
        lowest_pace,
        encoder_trace,
        recovered,
        expired: rig.expired(),
    }
}

/// A 1 Mbit/s link for 8 s, then an unlimited one: the delivery rate clamps
/// the window at once, the probe lowers fps and bits per frame, and both
/// recover once the link does. The encoder is drained throughout, and its
/// per-frame budget never latches the timestamp model.
#[test]
#[ignore = "explicit ~45-second bandwidth limit and recovery acceptance"]
fn bandwidth_limit_lowers_fps_and_bitrate_then_recovers() {
    traced(|trace| async move {
        let run = bandwidth_limit(&trace, "bandwidth_limit", TYPICAL).await;
        let BandwidthRun {
            status,
            before,
            clamped,
            lowest_pace,
            encoder_trace,
            recovered,
            expired,
        } = run;
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        // The trace samples once a second, so this is "within 1 s" to its
        // resolution.
        assert!(
            clamped.is_some_and(|after| after <= Duration::from_secs(1) + session::STATS_INTERVAL),
            "the window must clamp within 1 s of the limit: {clamped:?}"
        );
        assert!(lowest_pace <= 15, "{lowest_pace}");
        // Bits per frame may have risen past the start before the limit, and
        // go on rising for a window or two while the clamp holds the delay
        // down, so the fall is any step down from where they had reached.
        assert!(
            std::iter::once(&before.encoder_bitrate)
                .chain(&encoder_trace)
                .zip(&encoder_trace)
                .any(|(earlier, later)| later < earlier),
            "bits per frame must fall during the limit: before {} then {encoder_trace:?}",
            before.encoder_bitrate
        );
        assert!(
            recovered.is_some(),
            "the bitrate must recover to 80% of the start within 30 s"
        );
        assert_eq!(expired, 0, "a full window must never starve the encoder");
        trace.assert_never_latched();
    });
}

/// The same limit on an encoder that budgets per second. At the nominal model
/// it spends the nominal bitrate at any pace, so its pictures fill the link,
/// capture waits for credit and the delay controller takes the pace to 3 FPS
/// or below, where no 2 s of ticks holds six frames: no latch can come until
/// pictures flow again, by when the pace is climbing back. Reports whether
/// and when the latch came, and asserts that the session recovers as it does
/// on a per-frame budget.
#[test]
#[ignore = "explicit ~45-second per-second encoder measurement on a bandwidth limit"]
fn per_second_encoder_on_a_bandwidth_limit() {
    traced(|trace| async move {
        let run = bandwidth_limit(&trace, "per_second_bandwidth_limit", PER_SECOND).await;
        assert_eq!(
            run.status.state,
            ViewerState::Streaming,
            "{}",
            run.status.message
        );
        assert!(
            run.recovered.is_some(),
            "the bitrate must recover to 80% of the start within 30 s"
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
        trace.assert_never_latched();
    });
}

/// A decoder that takes 67 ms per frame: the viewer's fps cap brings the host
/// down to what it presents, while the host's own controller, its bits per
/// frame and its rate model stay where the network puts them.
#[test]
#[ignore = "explicit ~20-second slow presenter acceptance"]
fn slow_presenter_is_capped_by_fps_cap_not_host_backpressure() {
    traced(|trace| async move {
        let run = slow_presenter(&trace, "slow_presenter", TYPICAL).await;
        let last = *run.steady.last().unwrap();
        assert_eq!(last.fps, CEILING_FPS, "the network is not congested");
        assert!(
            last.encoder_bitrate >= START_BPS && !last.timestamp_model,
            "a slow presenter must not lower the host's bitrate: {last:?}"
        );
        trace.assert_never_latched();
    });
}

/// The same presenter on an encoder that budgets per second: at the nominal
/// model it spends the nominal bitrate whatever the fps cap, so the latch must
/// find it within `LATCH_WITHIN` of the pace falling, and from then on the
/// host sends about what it expects.
#[test]
#[ignore = "explicit ~20-second per-second encoder acceptance under an fps cap"]
fn per_second_encoder_latches_under_an_fps_cap() {
    traced(|trace| async move {
        let run = slow_presenter(&trace, "per_second_presenter", PER_SECOND).await;
        let last = *run.steady.last().unwrap();
        assert!(last.timestamp_model, "{last:?}");
        assert!(
            run.rate_model
                .latched_in_stretch
                .is_some_and(|after| after <= LATCH_WITHIN),
            "the latch must follow the pace within {LATCH_WITHIN:?}: {}",
            run.rate_model
        );
        assert!(
            run.rate_model
                .after
                .is_some_and(|ratio| (0.75..=1.25).contains(&ratio)),
            "after the latch the host sends what it expects: {}",
            run.rate_model
        );
    });
}

/// What one run of the slow-presenter case saw.
struct PresenterRun {
    /// The last 10 s of trace intervals.
    steady: Vec<Interval>,
    rate_model: RateModelRun,
}

/// A decoder that takes 67 ms per frame, for 20 s, on an encoder of `sizes`;
/// printed as `case`. Asserts what holds for any encoder: the viewer's fps cap
/// holds the pace at what it presents, and the session stays healthy.
async fn slow_presenter(trace: &Trace, case: &str, sizes: Sizes) -> PresenterRun {
    let rig = Rig::start(case, None, sizes).await;
    let _decoder = Decoder::start(&rig.viewer, Duration::from_millis(67));
    tokio::time::sleep(Duration::from_secs(20)).await;
    let measured = Instant::now();
    let status = rig.viewer.status();
    rig.stop().await;
    // The startup cap and then the viewer's fps cap hold the pace at or
    // below `LATCH_PACE` from the first capture on.
    let first_capture = rig.probe.paces.lock().unwrap().first().map(|(at, _)| *at);
    let rate_model = RateModelRun::read(
        trace,
        &rig.probe,
        first_capture.unwrap_or(measured),
        measured,
    );
    let steady = trace.between(measured - Duration::from_secs(10), measured);
    let mean_pace =
        steady.iter().map(|interval| interval.pace_fps).sum::<u64>() as f64 / steady.len() as f64;
    let last = *steady.last().unwrap();
    println!(
        "{case} decode_ms=67 mean_pace_fps={mean_pace:.1} caps={:?} fps={} encoder_bitrate={} presented_fps={:.1} backlog_drops={} keyframes_requested={} expired_encoder_outputs={} {rate_model} state={:?}",
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
    assert!(steady.iter().all(|interval| !interval.unresponsive));
    assert_eq!(rig.expired(), 0);
    PresenterRun { steady, rate_model }
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
        // Pictures of one size whatever the bitrate: no rate model fits
        // them, so the latch is reported, not asserted.
        let rate_model = RateModelRun::read(&trace, probe, start, Instant::now());
        println!(
            "slow_tls elapsed_s={elapsed:.3} link_kib_s={} frame_bytes={FRAME_BYTES} captured={captured} skipped_ticks={} control_ms={control_ms:.3} release_ms={release_ms:.3} revoke_ms={revoke_ms:.3} stop_ms={stop_ms:.3} {rate_model}",
            LINK_BYTES_PER_SECOND / 1024,
            trace.last().skipped_ticks
        );
    });
}

/// The link of the resync cases, and an encoder that holds each picture for
/// 45 ms, as an asynchronous one may: P-frames wait for credit in the host's
/// hand-off when an IDR joins it.
const RESYNC_LINK_BYTES_PER_SECOND: u64 = 125_000;
const RESYNC_ENCODER: Duration = Duration::from_millis(45);
/// How long a resync case lets the controllers settle on the link first.
const RESYNC_SETTLE: Duration = Duration::from_secs(6);
const RESYNC_ROUNDS: usize = 8;
/// Far longer than the viewer's least interval between two requests, so the
/// limiter holds a round's request back only for one the viewer sent itself.
const RESYNC_SPACING: Duration = Duration::from_secs(2);
const RESYNC_LIMIT: Duration = Duration::from_secs(5);

/// The counters a resync case reads before and after its rounds.
struct Counters {
    status: ViewerStatus,
    /// The host's dropped frames and the units the viewer's sink discarded.
    drops: (u64, u64),
}
impl Counters {
    fn read(viewer: &Viewer) -> Self {
        Self {
            status: viewer.status(),
            drops: viewer.drops(),
        }
    }
}

/// What a resync case reports: each round's wait and the P-frames that came
/// first, and over the rounds, the viewer's keyframe requests, the host's
/// IDRs, the frames the host dropped for this viewer and those its sink
/// discarded.
fn report_resyncs(case: &str, resyncs: &[Resync], before: &Counters, after: &Counters) {
    let mut waited: Vec<u128> = resyncs
        .iter()
        .map(|resync| resync.waited.as_millis())
        .collect();
    let rounds = format!("{waited:?}");
    waited.sort_unstable();
    let frames_first: Vec<u32> = resyncs.iter().map(|resync| resync.frames_first).collect();
    let held = resyncs.iter().filter(|resync| resync.held).count();
    let delta = |value: fn(&ViewerStats) -> u64| {
        value(&after.status.stats).saturating_sub(value(&before.status.stats))
    };
    println!(
        "{case} link_mbps=1 encoder_ms={} rounds={RESYNC_ROUNDS} answered={} waited_ms={rounds} waited_ms_median={:?} waited_ms_max={:?} frames_first={frames_first:?} frames_first_total={} held={held} keyframes_requested={} host_keyframes={} host_dropped_frames={} sink_discarded={} received_fps={:.1} state={:?}",
        RESYNC_ENCODER.as_millis(),
        resyncs.len(),
        waited.get(waited.len() / 2),
        waited.last(),
        frames_first.iter().sum::<u32>(),
        delta(|stats| stats.keyframes_requested),
        delta(|stats| stats.keyframes),
        after.drops.0.saturating_sub(before.drops.0),
        after.drops.1.saturating_sub(before.drops.1),
        after.status.stats.received_fps,
        after.status.state,
    );
}

/// A 1 Mbit/s link: every 2 s the decoder asks for an IDR, as one missing a
/// reference does, and discards the predicted frames that arrive before it.
/// Reports how long each IDR took to arrive whole and how many P-frames came
/// first, the requests the viewer sent (any beyond one a round are repeats),
/// its sink's discards, and the host's dropped frames and IDRs: a before and
/// after measurement of the host's hand-off, asserting only that the session
/// stayed up and every request was answered.
#[test]
#[ignore = "explicit ~30-second requested IDR measurement on a 1 Mbit/s link"]
fn requested_idr_on_a_slow_link() {
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape
            .rate
            .store(RESYNC_LINK_BYTES_PER_SECOND, Ordering::Relaxed);
        let rig = Rig::with_encoder("resync", Some(shape), TYPICAL, RESYNC_ENCODER).await;
        let decoder = Resyncing::start(&rig.viewer, true);
        tokio::time::sleep(RESYNC_SETTLE).await;
        let before = Counters::read(&rig.viewer);
        let resyncs = decoder.run(|| {}).await;
        let after = Counters::read(&rig.viewer);
        drop(decoder);
        rig.stop().await;
        report_resyncs("requested_idr", &resyncs, &before, &after);
        let status = after.status;
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert_eq!(resyncs.len(), RESYNC_ROUNDS, "every request is answered");
        trace.assert_never_latched();
    });
}

/// The same link for one viewer of a room of two, while the other, on an
/// unlimited link, asks for an IDR every 2 s, as a refresh does; a join or a
/// control change brings one as well. Reports, for the viewer on the slow
/// link, how long each IDR took to arrive whole after the other asked and the
/// P-frames it presented first, its sink's discards, and the host's dropped
/// frames and IDRs for it.
#[test]
#[ignore = "explicit ~30-second measurement of a room's IDR on a 1 Mbit/s link"]
fn idr_another_viewer_asked_for_on_a_slow_link() {
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape
            .rate
            .store(RESYNC_LINK_BYTES_PER_SECOND, Ordering::Relaxed);
        let rig = Rig::with_encoder("resync-room", Some(shape), TYPICAL, RESYNC_ENCODER).await;
        let decoder = Resyncing::start(&rig.viewer, false);
        let other = rig.second_viewer().await;
        let other_decoder = Decoder::start(&other, Duration::ZERO);
        tokio::time::sleep(RESYNC_SETTLE).await;
        let before = Counters::read(&rig.viewer);
        let resyncs = decoder
            .run(|| {
                other.request_keyframe(KeyframeReason::User);
            })
            .await;
        let after = Counters::read(&rig.viewer);
        let other_status = other.status();
        drop((decoder, other_decoder));
        other.stop().await.unwrap();
        rig.stop().await;
        report_resyncs("room_idr", &resyncs, &before, &after);
        println!(
            "room_idr other_received_fps={:.1} other_state={:?}",
            other_status.stats.received_fps, other_status.state
        );
        for status in [after.status, other_status] {
            assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        }
        assert_eq!(
            resyncs.len(),
            RESYNC_ROUNDS,
            "every IDR reaches the viewer on the slow link"
        );
        trace.assert_never_latched();
    });
}
