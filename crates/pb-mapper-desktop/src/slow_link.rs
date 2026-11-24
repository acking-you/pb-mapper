//! Explicit acceptance over a real relay and TLS, with a synthetic
//! asynchronous encoder and a simulated decoder: pipeline observations, not a
//! GPU benchmark or real rendered fps. Each case shapes the viewer's link to
//! the relay and reads the host's once-a-second `desktop_feedback_interval`
//! trace; `direct_quic_on_lossy_links` instead carries the session over one
//! QUIC connection with the direct transport, through a lossy hop. The cases
//! run one at a time. Run with:
//! cargo test -p pb-mapper-desktop -- --ignored --nocapture slow_link
use crate::*;
use pb_mapper_client::sdk::{
    Client, CongestionControl, ConnectOptions, PathPolicy, direct_transport_config,
};
use pb_mapper_testkit::{
    LossyShape, LossyUdp, Relay, Shape, ShapedLink, admin_credential, lossy_udp, lossy_udp_socket,
    shaped_link_each_way,
};
use std::{
    collections::{BTreeSet, VecDeque},
    future::Future,
    net::SocketAddr,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tracing_subscriber::{Layer, filter::Targets, layer::SubscriberExt};

/// Balanced's start at 1080p and 60 fps: 66 667 bits per frame (perf 5.9.1).
const START_BPS: u64 = 4_000_000;
/// Balanced's most and fewest bits per frame at 1080p.
const BPF_MAX: u64 = 133_333;
const BPF_FLOOR: u64 = 16_667;
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
const FRAMES: &str = "pb_mapper_desktop::frames";

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
    /// Thread C's cap at its encoder's throughput, as the QoS last read it.
    encoder_cap: Option<u64>,
    /// The encoder's median dwell over the host's last second.
    encoder_dwell_ms: Option<u64>,
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
    bpf: u64,
    window_frames: u64,
    window_bytes: u64,
    /// The controller's action, as `Adaptation`'s debug name.
    adaptation: &'static str,
    delay_ms: Option<u64>,
    ack_queue_ms: u64,
    rtt_ms: Option<u64>,
    /// The receipts' one-way queues, from a viewer that stamps arrival times.
    forward_queue_ms: Option<u64>,
    reverse_queue_ms: Option<u64>,
    /// The fps cap the receipts set from the delivery rate, if any.
    rate_fps: Option<u64>,
    /// The receipts' standing queue.
    standing_ms: Option<u64>,
    /// The receipts' rate cuts since the session began.
    cuts: u64,
    /// What a rebase added to the probes' baseline, if anything.
    rebase_ms: Option<u64>,
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
            encoder_cap: None,
            encoder_dwell_ms: None,
            sent_bitrate: 0,
            window_clamped: false,
            unresponsive: false,
            timestamp_model: false,
            skipped_ticks: 0,
            keyframes: 0,
            latched: false,
            bpf: 0,
            window_frames: 0,
            window_bytes: 0,
            adaptation: "",
            delay_ms: None,
            ack_queue_ms: 0,
            rtt_ms: None,
            forward_queue_ms: None,
            reverse_queue_ms: None,
            rate_fps: None,
            standing_ms: None,
            cuts: 0,
            rebase_ms: None,
        }
    }

    /// One line of what the host traced, `t_ms` after `from`, in `phase`.
    fn print(&self, case: &str, from: Instant, phase: &str) {
        let millis = |value: Option<u64>| value.map_or_else(|| "-".to_owned(), |ms| ms.to_string());
        println!(
            "{case} t_ms={} phase={phase} pace_fps={} fps={} rate_fps={} bpf={} encoder_bitrate={} sent_bitrate={} clamped={} adaptation={} window_frames={} window_bytes={} rtt_ms={} delay_ms={} ack_queue_ms={} forward_queue_ms={} reverse_queue_ms={} standing_ms={} cuts={} rebase_ms={}",
            self.at.saturating_duration_since(from).as_millis(),
            self.pace_fps,
            self.fps,
            millis(self.rate_fps),
            self.bpf,
            self.encoder_bitrate,
            self.sent_bitrate,
            self.window_clamped,
            self.adaptation,
            self.window_frames,
            self.window_bytes,
            millis(self.rtt_ms),
            millis(self.delay_ms),
            self.ack_queue_ms,
            millis(self.forward_queue_ms),
            millis(self.reverse_queue_ms),
            millis(self.standing_ms),
            self.cuts,
            millis(self.rebase_ms),
        );
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
            "encoder_cap" => self.encoder_cap = Some(value),
            "encoder_dwell_ms" => self.encoder_dwell_ms = Some(value),
            "skipped_ticks" => self.skipped_ticks = value,
            "keyframes" => self.keyframes = value,
            "bpf" => self.bpf = value,
            "window_frames" => self.window_frames = value,
            "window_bytes" => self.window_bytes = value,
            "delay_ms" => self.delay_ms = Some(value),
            "ack_queue_ms" => self.ack_queue_ms = value,
            "rtt_ms" => self.rtt_ms = Some(value),
            "rate_fps" => self.rate_fps = Some(value),
            "standing_ms" => self.standing_ms = Some(value),
            "cuts" => self.cuts = value,
            "rebase_ms" => self.rebase_ms = Some(value),
            "forward_queue_ms" => self.forward_queue_ms = Some(value),
            "reverse_queue_ms" => self.reverse_queue_ms = Some(value),
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
        match field.name() {
            "rate_model" => self.timestamp_model = format!("{value:?}") == "Timestamp",
            "action" => {
                let action = format!("{value:?}");
                self.adaptation = [
                    "Startup",
                    "Steady",
                    "Raising",
                    "LoweringFps",
                    "LoweringBitrate",
                    "Unresponsive",
                    "Paused",
                ]
                .into_iter()
                .find(|name| *name == action)
                .unwrap_or("?");
            }
            _ => {}
        }
    }
}

/// A change of thread C's cap at its encoder's throughput, as the host's
/// `desktop_encoder_cap` traced it.
#[derive(Clone, Debug, Default)]
struct CapChange {
    at: Option<Instant>,
    event: bool,
    action: String,
    cap: Option<u64>,
    asked_fps: u64,
    full_ticks: u64,
    units: u64,
    dwell_before_us: Option<u64>,
    dwell_after_us: Option<u64>,
}
impl CapChange {
    /// `action:cap` with the second's figures, `t_ms` after `from`.
    fn line(&self, from: Instant) -> String {
        let ms = |us: Option<u64>| {
            us.map_or_else(|| "-".to_owned(), |us| format!("{:.1}", us as f64 / 1000.0))
        };
        format!(
            "t_ms={} {}:{} asked={} full_ticks={} units={} dwell_before_ms={} dwell_after_ms={}",
            self.at
                .map_or(0, |at| at.saturating_duration_since(from).as_millis()),
            self.action,
            self.cap
                .map_or_else(|| "-".to_owned(), |cap| cap.to_string()),
            self.asked_fps,
            self.full_ticks,
            self.units,
            ms(self.dwell_before_us),
            ms(self.dwell_after_us),
        )
    }
}
impl tracing::field::Visit for CapChange {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "event" => self.event = value == "desktop_encoder_cap",
            "action" => self.action = value.to_owned(),
            _ => {}
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "cap" => self.cap = Some(value),
            "asked_fps" => self.asked_fps = value,
            "full_ticks" => self.full_ticks = value,
            "units" => self.units = value,
            "dwell_before_us" => self.dwell_before_us = Some(value),
            "dwell_after_us" => self.dwell_after_us = Some(value),
            _ => {}
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

/// One accepted presentation, as the viewer's `desktop_frame_view` traced it,
/// in microseconds since its sink began.
#[derive(Clone, Copy, Debug)]
struct View {
    at: Instant,
    view: bool,
    received_us: u64,
    handed_us: u64,
    presented_us: u64,
}
impl View {
    fn new(at: Instant) -> Self {
        Self {
            at,
            view: false,
            received_us: 0,
            handed_us: 0,
            presented_us: 0,
        }
    }
    /// From receipt to the hand-off: the wait in the sink.
    fn sink_wait_us(&self) -> u64 {
        self.handed_us.saturating_sub(self.received_us)
    }
    /// From receipt to presentation.
    fn presented_after_us(&self) -> u64 {
        self.presented_us.saturating_sub(self.received_us)
    }
}
impl tracing::field::Visit for View {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "received_us" => self.received_us = value,
            "handed_us" => self.handed_us = value,
            "presented_us" => self.presented_us = value,
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.view = format!("{value:?}") == "desktop_frame_view";
        }
    }
}

/// The host's feedback trace of one case, and the viewer's presentations
/// when the case asked for them.
#[derive(Clone, Default)]
struct Trace {
    intervals: Arc<Mutex<Vec<Interval>>>,
    /// When the host latched the timestamp rate model.
    latched: Arc<Mutex<Option<Instant>>>,
    views: Arc<Mutex<Vec<View>>>,
    /// Each change of the cap at the encoder's throughput.
    caps: Arc<Mutex<Vec<CapChange>>>,
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
    /// The presentations traced in `from..to`.
    fn views(&self, from: Instant, to: Instant) -> Vec<View> {
        let views = self.views.lock().unwrap();
        views
            .iter()
            .filter(|view| (from..to).contains(&view.at))
            .copied()
            .collect()
    }
}
impl<S: tracing::Subscriber> Layer<S> for Trace {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() == FRAMES {
            let mut view = View::new(Instant::now());
            event.record(&mut view);
            if view.view {
                self.views.lock().unwrap().push(view);
            }
            return;
        }
        let mut interval = Interval::new(Instant::now());
        event.record(&mut interval);
        if interval.feedback {
            self.intervals.lock().unwrap().push(interval);
        } else if interval.latched {
            self.latched.lock().unwrap().get_or_insert(interval.at);
        } else {
            let mut change = CapChange {
                at: Some(interval.at),
                ..CapChange::default()
            };
            event.record(&mut change);
            if change.event {
                self.caps.lock().unwrap().push(change);
            }
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
    traced_with(false, case);
}

/// `traced`, with the viewer's presentations in the trace as well.
fn traced_views<F: Future<Output = ()>>(case: impl FnOnce(Trace) -> F) {
    traced_with(true, case);
}

fn traced_with<F: Future<Output = ()>>(views: bool, case: impl FnOnce(Trace) -> F) {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let trace = Trace::default();
    let mut targets = Targets::new().with_target(FEEDBACK, tracing::Level::DEBUG);
    if views {
        targets = targets.with_target(FRAMES, tracing::Level::TRACE);
    }
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry().with(trace.clone().with_filter(targets)),
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
    /// When each picture was captured: the one of presentation time
    /// `(n + 1) × PTS_STEP_US` at `n`.
    captured: Mutex<Vec<Instant>>,
}

/// The synthetic encoder's presentation time per picture.
const PTS_STEP_US: u64 = 16_667;

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
/// `ENCODER_EXPIRY` is lost. A `serial` one, as an MFT slower than the pace
/// that still takes input, starts each picture once the one before it is
/// out, so `latency` is its service time. No decoder reads the bytes.
struct SyntheticEncoder {
    probe: Arc<Probe>,
    sizes: Sizes,
    latency: Duration,
    serial: bool,
    nominal_fps: u32,
    frames: u64,
    last_capture: Option<Instant>,
    /// `(submitted, ready, unit)`.
    pending: VecDeque<(Instant, Instant, EncodedFrame)>,
}
impl SyntheticEncoder {
    fn ready(&mut self) -> Option<EncodedFrame> {
        while let Some((submitted, ready, _)) = self.pending.front() {
            if Instant::now() < *ready {
                return None;
            }
            let age = submitted.elapsed();
            let (_, _, mut frame) = self.pending.pop_front()?;
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
        self.probe.captured.lock().unwrap().push(now);
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
                pts_us: self.frames * PTS_STEP_US,
                ..VideoFormat::default()
            },
            bytes: vec![0x65; bytes],
        };
        let submitted = Instant::now();
        let start = match self.pending.back() {
            Some((_, ready, _)) if self.serial => submitted.max(*ready),
            _ => submitted,
        };
        self.pending
            .push_back((submitted, start + self.latency, frame));
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

/// A thread that traces into the case's `Trace`, as a native consumer's
/// thread is outside the runtime.
fn traced_thread(body: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
    let dispatch = tracing::dispatcher::get_default(tracing::Dispatch::clone);
    std::thread::spawn(move || tracing::dispatcher::with_default(&dispatch, body))
}

/// One stall of a `Decoder`: when it began and ended, and when the first unit
/// received after it began was presented.
#[derive(Clone, Copy, Debug, Default)]
struct Stall {
    began: Option<Instant>,
    ended: Option<Instant>,
    first_fresh: Option<Instant>,
}

/// How a `Decoder` takes its units; a case may change it while it runs.
#[derive(Default)]
struct Pace {
    /// Each unit's decode time, in µs.
    decode_us: AtomicU64,
    /// A stall to take once, holding the next unit, in µs.
    stall_us: AtomicU64,
    stall: Mutex<Stall>,
}
impl Pace {
    fn new(decode: Duration) -> Arc<Self> {
        let pace = Arc::new(Self::default());
        pace.set(decode);
        pace
    }
    fn set(&self, decode: Duration) {
        self.decode_us
            .store(decode.as_micros() as u64, Ordering::Relaxed);
    }
    fn stall(&self, stall: Duration) {
        self.stall_us
            .store(stall.as_micros() as u64, Ordering::Release);
    }
    fn stalled(&self) -> Stall {
        *self.stall.lock().unwrap()
    }
}

/// The first unit a `Decoder` presented: when, its bytes, and whether it was
/// an IDR.
#[derive(Clone, Copy, Debug)]
struct FirstPresented {
    at: Instant,
    bytes: usize,
    keyframe: bool,
}

/// A simulated decoder on its own thread, as a native consumer runs: it pulls
/// a unit, spends its `Pace` on it, then presents it.
struct Decoder {
    stop: Arc<AtomicBool>,
    sizes: Arc<Mutex<BTreeSet<(u32, u32)>>>,
    first: Arc<Mutex<Option<FirstPresented>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Decoder {
    fn start(viewer: &Viewer, decode: Duration) -> Self {
        Self::paced(viewer, Pace::new(decode))
    }
    fn paced(viewer: &Viewer, pace: Arc<Pace>) -> Self {
        let consumer = viewer.video_sink().attach().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let sizes = Arc::new(Mutex::new(BTreeSet::new()));
        let first = Arc::new(Mutex::new(None));
        let (done, seen, shown) = (stop.clone(), sizes.clone(), first.clone());
        let thread = traced_thread(move || {
            while !done.load(Ordering::Acquire) {
                match consumer.next(Duration::from_millis(50), wire::MAX_PAYLOAD) {
                    Pull::Unit(unit) => {
                        seen.lock().unwrap().insert((unit.width, unit.height));
                        let stall = pace.stall_us.swap(0, Ordering::AcqRel);
                        if stall > 0 {
                            let began = Instant::now();
                            std::thread::sleep(Duration::from_micros(stall));
                            *pace.stall.lock().unwrap() = Stall {
                                began: Some(began),
                                ended: Some(Instant::now()),
                                first_fresh: None,
                            };
                        }
                        std::thread::sleep(Duration::from_micros(
                            pace.decode_us.load(Ordering::Relaxed),
                        ));
                        if consumer.presented(unit.generation, unit.frame_id) {
                            shown.lock().unwrap().get_or_insert(FirstPresented {
                                at: Instant::now(),
                                bytes: unit.bytes.len(),
                                keyframe: unit.keyframe,
                            });
                            let mut stall = pace.stall.lock().unwrap();
                            if stall.first_fresh.is_none()
                                && stall.began.is_some_and(|began| unit.received_at >= began)
                            {
                                stall.first_fresh = Some(Instant::now());
                            }
                        }
                    }
                    Pull::Closed => break,
                    _ => {}
                }
            }
        });
        Self {
            stop,
            sizes,
            first,
            thread: Some(thread),
        }
    }
    fn sizes(&self) -> Vec<(u32, u32)> {
        self.sizes.lock().unwrap().iter().copied().collect()
    }
    fn first(&self) -> Option<FirstPresented> {
        *self.first.lock().unwrap()
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

/// A simulated decoder that queues inside itself, as the Linux runner's
/// GStreamer pipeline does: one thread pulls a unit only while fewer than
/// `depth` are inside, another decodes them in order, `decode` each, and
/// presents them.
struct Pipelined {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}
impl Pipelined {
    fn start(viewer: &Viewer, depth: usize, decode: Duration) -> Self {
        let consumer = Arc::new(viewer.video_sink().attach().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let inside = Arc::new((Mutex::new(0usize), Condvar::new()));
        let (units, queued) = mpsc::channel::<VideoUnit>();
        let feeder = {
            let (consumer, stop, inside) = (consumer.clone(), stop.clone(), inside.clone());
            traced_thread(move || {
                let (count, room) = &*inside;
                while !stop.load(Ordering::Acquire) {
                    let full = {
                        let count = count.lock().unwrap();
                        let (count, _) = room
                            .wait_timeout_while(count, Duration::from_millis(50), |count| {
                                *count >= depth
                            })
                            .unwrap();
                        *count >= depth
                    };
                    if full {
                        continue;
                    }
                    match consumer.next(Duration::from_millis(50), wire::MAX_PAYLOAD) {
                        Pull::Unit(unit) => {
                            *count.lock().unwrap() += 1;
                            if units.send(unit).is_err() {
                                break;
                            }
                        }
                        Pull::Closed => break,
                        _ => {}
                    }
                }
            })
        };
        let decoder = {
            let stop = stop.clone();
            traced_thread(move || {
                let (count, room) = &*inside;
                while !stop.load(Ordering::Acquire) {
                    let Ok(unit) = queued.recv_timeout(Duration::from_millis(50)) else {
                        continue;
                    };
                    std::thread::sleep(decode);
                    consumer.presented(unit.generation, unit.frame_id);
                    *count.lock().unwrap() -= 1;
                    room.notify_one();
                }
            })
        };
        Self {
            stop,
            threads: vec![feeder, decoder],
        }
    }
}
impl Drop for Pipelined {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
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
    /// When the viewer began `Viewer::connect`.
    connecting: Instant,
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
        Self::at_fps(name, shape, sizes, latency, CEILING_FPS as u32).await
    }
    /// A rig whose viewer asks for `fps`: its display's rate at 60, otherwise
    /// the user's own choice, which is every preset's ceiling.
    async fn at_fps(
        name: &str,
        shape: Option<Arc<Shape>>,
        sizes: Sizes,
        latency: Duration,
        fps: u32,
    ) -> Self {
        let link = shape.map(|shape| (shape.clone(), shape));
        Self::build(name, link, sizes, (latency, false), fps).await
    }
    /// `at_fps` with an encoder that, when `serial`, serves its pictures one
    /// at a time, `latency` each.
    async fn with_serial(
        name: &str,
        shape: Option<Arc<Shape>>,
        latency: Duration,
        serial: bool,
        fps: u32,
    ) -> Self {
        let link = shape.map(|shape| (shape.clone(), shape));
        Self::build(name, link, TYPICAL, (latency, serial), fps).await
    }
    /// A rig whose viewer's link is shaped `up`, toward the relay, apart from
    /// `down`, toward the viewer.
    async fn each_way(name: &str, up: Arc<Shape>, down: Arc<Shape>, sizes: Sizes) -> Self {
        Self::build(
            name,
            Some((up, down)),
            sizes,
            (ENCODE_TIME, false),
            CEILING_FPS as u32,
        )
        .await
    }
    /// `encoder` is the latency and whether the encoder is serial.
    async fn build(
        name: &str,
        link: Option<(Arc<Shape>, Arc<Shape>)>,
        sizes: Sizes,
        encoder: (Duration, bool),
        fps: u32,
    ) -> Self {
        let (latency, serial) = encoder;
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
                    serial,
                    nominal_fps: 0,
                    frames: 0,
                    last_capture: None,
                    pending: VecDeque::new(),
                })
            })
            .start(&local)
            .await
            .unwrap();
        let (remote, link) = match link {
            Some((up, down)) => {
                let link = shaped_link_each_way(relay.addr(), up, down).await;
                (client(link.addr()), Some(link))
            }
            None => (client(relay.addr()), None),
        };
        let connecting = Instant::now();
        let viewer = connect_viewer_at(&remote, &host, fps).await;
        Self {
            host,
            viewer,
            connecting,
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
    connect_viewer_at(client, host, CEILING_FPS as u32).await
}

/// A viewer of `host` through `client`, on the relay, asking for `fps`.
async fn connect_viewer_at(client: &Client, host: &Host, fps: u32) -> Viewer {
    Viewer::connect(
        client,
        host.invitation(),
        ConnectOptions {
            path: PathPolicy::RelayOnly,
            ..Default::default()
        },
        ViewerConfig {
            codecs: vec![VideoCodec::H264],
            limits: VideoLimits::for_viewer(f64::from(fps)),
            fps_override: u64::from(fps) != CEILING_FPS,
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
/// Reports, over 30 s, when bits per frame reached Balanced's maximum.
#[test]
#[ignore = "explicit ~30-second short-path acceptance"]
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
        let bpf_max = first_after(&trace, started, Duration::from_secs(30), |interval| {
            interval.bpf >= BPF_MAX
        })
        .await;
        let last = trace.last();
        rig.stop().await;
        println!(
            "lan_rtt one_way_ms={SHORT_PATH_MS} ceiling_after_ms={:?} received_fps_mean={:.1} received_fps={received:?} presented_fps={:.1} rtt_ms={:?} bpf_max_after_ms={:?} last_bpf={} cuts={} state={:?}",
            ceiling.map(|after| after.as_millis()),
            mean(&received),
            status.stats.presented_fps,
            status.stats.rtt_ms,
            bpf_max.map(|after| after.as_millis()),
            last.bpf,
            last.cuts,
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
/// probe even when they come only once a second. A stall is no congestion:
/// the receipts never cut the rate for one.
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
        let first = trace.last();
        let mut stalls = Vec::new();
        let mut encoder = Vec::new();
        for _ in 0..3 {
            let before = trace.last().pace_fps;
            let before_encoder = trace.last().encoder_bitrate;
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
            let lowest_encoder = after
                .iter()
                .map(|interval| interval.encoder_bitrate)
                .min()
                .unwrap();
            encoder.push((before_encoder, lowest_encoder));
        }
        let last = trace.last();
        let status = rig.viewer.status();
        rig.stop().await;
        println!(
            "isolated_stall one_way_ms={SHORT_PATH_MS} stall_ms={} (pace_before, lowest_pace, back_at_55_after)={stalls:?} (encoder_bitrate_before, lowest_encoder_bitrate)={encoder:?} cuts={} received_fps={:.1} state={:?}",
            STALL.as_millis(),
            last.cuts - first.cuts,
            status.stats.received_fps,
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        for (before, lowest, recovered) in stalls {
            assert!(lowest * 2 >= before, "{lowest} is below half of {before}");
            assert!(recovered <= RECOVERY, "back at 55 only after {recovered:?}");
        }
        assert_eq!(last.cuts, first.cuts, "a stall is no congestion to cut for");
        // Nor does a 3 s window step bits per frame down for it: the
        // receipts own them.
        for (before, lowest) in encoder {
            assert!(
                lowest >= before,
                "the encoder bitrate fell to {lowest} from {before}"
            );
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

/// A 120 fps session on a short path (the user's own rate, so every preset's
/// ceiling) whose encoder cannot keep up with 120: first one that serves a
/// picture each 11 ms, one at a time, while it takes up to thread C's four,
/// as a Media Foundation encoder slower than the pace does; then one that
/// keeps each picture 45 ms but takes many at once. Reports for each, from
/// 8 s to 48 s (past the serial encoder's first probe, about 30 s after its
/// cap, and the pipelined one's second cap, 30 s after the first), the
/// encoder's dwell, the received fps, the pace and the skipped ticks a
/// second, then every change of the host's cap at the encoder's throughput
/// (`desktop_encoder_cap`). Once both have run, the serial encoder's units
/// must wait no more than about two service times (25 ms, with thread C's
/// 2 ms drain poll) at the median and in all but 5 of the 40 seconds, at
/// 75 fps or more received; the pipelined one, whose cap the guard lifts,
/// keeps 75 or more.
#[test]
#[ignore = "explicit ~100-second 120 fps slow encoder measurement"]
fn lan_rtt_keeps_120() {
    /// What one encoder's run left for the assertions.
    struct Outcome {
        serial: bool,
        state: ViewerState,
        message: String,
        dwell_p50: u32,
        dwell: Vec<u32>,
        received: Vec<f32>,
    }
    let outcomes = Mutex::new(Vec::new());
    for (case, latency, serial) in [
        ("serial_11ms", Duration::from_millis(11), true),
        ("pipelined_45ms", Duration::from_millis(45), false),
    ] {
        let outcomes = &outcomes;
        traced(|trace| async move {
            let shape = Arc::new(Shape::default());
            shape.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
            let name = format!("lan-120-{case}");
            let rig = Rig::with_serial(&name, Some(shape), latency, serial, 120).await;
            let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
            let started = Instant::now();
            tokio::time::sleep(Duration::from_secs(8)).await;
            let (mut received, mut dwell, mut pace, mut skipped) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let stats = rig.viewer.status().stats;
                received.push(stats.received_fps);
                dwell.push(stats.encoder_dwell_ms.unwrap_or(0));
                pace.push(stats.pace_fps);
                skipped.push(stats.skipped_per_s);
            }
            let status = rig.viewer.status();
            let caps = trace.caps.lock().unwrap().clone();
            rig.stop().await;
            let mut sorted = dwell.clone();
            sorted.sort_unstable();
            let dwell_p50 = sorted[sorted.len() / 2];
            let over = dwell.iter().filter(|&&ms| ms > 25).count();
            println!(
                "lan_rtt_keeps_120 encoder={case} one_way_ms={SHORT_PATH_MS} seconds=8..48 received_fps_mean={:.1} encoder_dwell_ms_p50={dwell_p50} encoder_dwell_ms_mean={:.1} seconds_over_25ms={over} skipped_per_s_mean={:.1} encoder_dwell_ms={dwell:?} received_fps={received:?} pace_fps={pace:?} skipped_per_s={skipped:?} expired_encoder_outputs={} state={:?}",
                mean(&received),
                dwell.iter().map(|&ms| f64::from(ms)).sum::<f64>() / dwell.len() as f64,
                mean(&skipped),
                rig.expired(),
                status.state
            );
            for change in &caps {
                println!(
                    "lan_rtt_keeps_120 encoder={case} desktop_encoder_cap {}",
                    change.line(started)
                );
            }
            outcomes.lock().unwrap().push(Outcome {
                serial,
                state: status.state,
                message: status.message,
                dwell_p50,
                dwell,
                received,
            });
        });
    }
    for outcome in outcomes.into_inner().unwrap() {
        let Outcome {
            serial,
            state,
            message,
            dwell_p50,
            dwell,
            received,
        } = outcome;
        assert_eq!(state, ViewerState::Streaming, "{message}");
        if serial {
            assert!(dwell_p50 <= 25, "{dwell:?}");
            assert!(
                dwell.iter().filter(|&&ms| ms > 25).count() <= 5,
                "{dwell:?}"
            );
            assert!(mean(&received) >= 75.0, "{received:?}");
        } else {
            assert!(mean(&received) >= 75.0, "{received:?}");
        }
    }
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
        let started = Instant::now();
        tokio::time::sleep(Duration::from_secs(20)).await;
        let status = rig.viewer.status();
        let last = trace.last();
        rig.stop().await;
        let ceiling = trace
            .between(started, Instant::now())
            .into_iter()
            .find(|interval| interval.pace_fps == CEILING_FPS)
            .map(|interval| interval.at - started);
        let sizes = decoder.sizes();
        let configured = rig.probe.configured.load(Ordering::Relaxed);
        println!(
            "fixed_wan roundtrip_ms=350 ceiling_after_ms={:?} cuts={} pace_fps={} bitrate={} encoder_bitrate={} presented_fps={:.1} received_fps={:.1} rtt_ms={:?} keyframes={} sizes={sizes:?} configured={configured} expired_encoder_outputs={} state={:?}",
            ceiling.map(|after| after.as_millis()),
            last.cuts,
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

/// How long a link-limit case lets the session run unlimited first.
#[derive(Clone, Copy)]
enum Settle {
    /// This long.
    For(Duration),
    /// Until bits per frame reach Balanced's maximum, or this long.
    BpfMax(Duration),
}

/// What one run of a link-limit case saw.
struct LimitRun {
    status: ViewerStatus,
    /// The host's last interval before the limit.
    before: Interval,
    /// When bits per frame reached Balanced's maximum, after the session
    /// began, for a case that waited for it.
    bpf_max: Option<Duration>,
    limited: Instant,
    lifted: Instant,
    /// The host's intervals traced while the link was limited.
    limit: Vec<Interval>,
    /// The viewer's received fps at the end of each second of the limit.
    received: Vec<(Instant, f32)>,
    recovered: Option<Duration>,
    expired: u32,
    rate_model: RateModelRun,
}
impl LimitRun {
    /// The limit's last `LAST_SECONDS` seconds.
    const LAST_SECONDS: Duration = Duration::from_secs(5);

    fn lowest(&self, value: fn(&Interval) -> u64) -> u64 {
        self.limit.iter().map(value).min().unwrap()
    }
    fn highest(&self, value: fn(&Interval) -> u64) -> u64 {
        self.limit.iter().map(value).max().unwrap()
    }
    /// The intervals traced over the limit's last seconds.
    fn last_intervals(&self) -> Vec<Interval> {
        let from = self.lifted - Self::LAST_SECONDS;
        self.limit
            .iter()
            .filter(|interval| interval.at >= from)
            .copied()
            .collect()
    }
    /// The viewer's received fps over the limit's last seconds.
    fn last_received(&self) -> Vec<f32> {
        let from = self.lifted - Self::LAST_SECONDS;
        self.received
            .iter()
            .filter(|(at, _)| *at > from)
            .map(|(_, fps)| *fps)
            .collect()
    }
    /// When the window first clamped, after the limit began.
    fn clamped(&self) -> Option<Duration> {
        self.limit
            .iter()
            .find(|interval| interval.window_clamped)
            .map(|interval| interval.at - self.limited)
    }
    /// The receipts' cuts during the limit.
    fn cuts(&self) -> u64 {
        self.limit
            .last()
            .map_or(0, |last| last.cuts.saturating_sub(self.before.cuts))
    }
    /// The encoder bitrate in each interval of the limit.
    fn encoder_trace(&self) -> Vec<u64> {
        self.limit
            .iter()
            .map(|interval| interval.encoder_bitrate)
            .collect()
    }
    /// The figures the record compares, on one line.
    fn print(&self, case: &str, link_bytes_per_second: u64) {
        let last = self.last_intervals();
        let mean_ack = |intervals: &[Interval]| {
            intervals
                .iter()
                .map(|interval| interval.ack_queue_ms)
                .sum::<u64>() as f64
                / intervals.len().max(1) as f64
        };
        // An interval's figures cover the second before it.
        let sent: Vec<u64> = self
            .limit
            .iter()
            .filter(|interval| interval.at >= self.limited + session::STATS_INTERVAL)
            .map(|interval| interval.sent_bitrate)
            .collect();
        let sent_over_link = sent.iter().sum::<u64>() as f64
            / sent.len().max(1) as f64
            / (link_bytes_per_second * 8) as f64;
        println!(
            "{case} summary link_kbit_s={} before_pace={} before_bpf={} before_encoder_bitrate={} bpf_max_after_ms={:?} clamped_after_ms={:?} lowest_pace={} lowest_bpf={} lowest_encoder_bitrate={} mean_pace_last5={:.1} received_fps_last5={:.1} ack_queue_p95_ms={:?} ack_queue_peak_ms={} ack_queue_mean_last5={:.0} sent_over_link={:.2} cuts={} recovered_after_ms={:?} expired_encoder_outputs={} {} state={:?}",
            link_bytes_per_second * 8 / 1000,
            self.before.pace_fps,
            self.before.bpf,
            self.before.encoder_bitrate,
            self.bpf_max.map(|after| after.as_millis()),
            self.clamped().map(|after| after.as_millis()),
            self.lowest(|interval| interval.pace_fps),
            self.lowest(|interval| interval.bpf),
            self.lowest(|interval| interval.encoder_bitrate),
            mean_pace(&last),
            mean(&self.last_received()),
            p95(self
                .limit
                .iter()
                .map(|interval| interval.ack_queue_ms)
                .collect()),
            self.highest(|interval| interval.ack_queue_ms),
            mean_ack(&last),
            sent_over_link,
            self.cuts(),
            self.recovered.map(|after| after.as_millis()),
            self.expired,
            self.rate_model,
            self.status.state
        );
    }
}

/// A link limited to `rate` bytes a second for `limit`, after it ran
/// unlimited as `settle` says, then unlimited again, for an encoder of
/// `sizes`; printed as `case`.
async fn link_limit(
    trace: &Trace,
    case: &str,
    sizes: Sizes,
    settle: Settle,
    rate: u64,
    limit: Duration,
) -> LimitRun {
    let shape = Arc::new(Shape::default());
    let rig = Rig::start(case, Some(shape.clone()), sizes).await;
    let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
    let started = Instant::now();
    let bpf_max = match settle {
        Settle::For(settle) => {
            tokio::time::sleep(settle).await;
            None
        }
        Settle::BpfMax(longest) => {
            first_after(trace, started, longest, |interval| interval.bpf >= BPF_MAX).await
        }
    };
    let before = trace.last();
    let limited = Instant::now();
    shape.rate.store(rate, Ordering::Relaxed);
    let mut received = Vec::new();
    for _ in 0..limit.as_secs() {
        tokio::time::sleep(Duration::from_secs(1)).await;
        received.push((Instant::now(), rig.viewer.status().stats.received_fps));
    }
    shape.rate.store(0, Ordering::Relaxed);
    let lifted = Instant::now();
    let recovered = first_after(trace, lifted, Duration::from_secs(30), |interval| {
        interval.bitrate * 5 >= START_BPS * 4
    })
    .await;
    let status = rig.viewer.status();
    rig.stop().await;
    let phase = |at: Instant| if at < lifted { "limited" } else { "lifted" };
    for interval in trace.between(limited, Instant::now()) {
        interval.print(case, limited, phase(interval.at));
    }
    let run = LimitRun {
        status,
        before,
        bpf_max,
        limited,
        lifted,
        limit: trace.between(limited, lifted),
        received,
        recovered,
        expired: rig.expired(),
        rate_model: RateModelRun::read(trace, &rig.probe, limited, Instant::now()),
    };
    run.print(case, rate);
    run
}

/// The 1 Mbit/s link of the bandwidth-limit cases, for 8 s after 6 s
/// unlimited.
async fn bandwidth_limit(trace: &Trace, case: &str, sizes: Sizes) -> LimitRun {
    link_limit(
        trace,
        case,
        sizes,
        Settle::For(Duration::from_secs(6)),
        125_000,
        Duration::from_secs(8),
    )
    .await
}

/// A 1 Mbit/s link for 8 s, then an unlimited one: the window clamps at
/// once, the receipts cut bits per frame to Balanced's floor and the pace to
/// what the link delivers at it, so the viewer keeps receiving 35 FPS or
/// more over the limit's last 5 s, and both recover once the link does. The
/// encoder is drained throughout, and its per-frame budget never latches the
/// timestamp model.
#[test]
#[ignore = "explicit ~45-second bandwidth limit and recovery acceptance"]
fn bandwidth_limit_cuts_bits_per_frame_then_recovers() {
    traced(|trace| async move {
        let run = bandwidth_limit(&trace, "bandwidth_limit", TYPICAL).await;
        let status = &run.status;
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        // The trace samples once a second, so this is "within 1 s" to its
        // resolution.
        let clamped = run.clamped();
        assert!(
            clamped.is_some_and(|after| after <= Duration::from_secs(1) + session::STATS_INTERVAL),
            "the window must clamp within 1 s of the limit: {clamped:?}"
        );
        assert!(
            run.limit.iter().any(|interval| interval.bpf <= BPF_FLOOR),
            "bits per frame must reach the floor: {:?}",
            run.limit
        );
        let (pace, received) = (mean_pace(&run.last_intervals()), mean(&run.last_received()));
        assert!(pace >= 40.0, "the pace fell to {pace:.1}");
        assert!(received >= 35.0, "the viewer received {received:.1} fps");
        // Bits per frame may have risen past the start before the limit, so
        // the fall is any step down from where they had reached.
        let encoder_trace = run.encoder_trace();
        assert!(
            std::iter::once(&run.before.encoder_bitrate)
                .chain(&encoder_trace)
                .zip(&encoder_trace)
                .any(|(earlier, later)| later < earlier),
            "bits per frame must fall during the limit: before {} then {encoder_trace:?}",
            run.before.encoder_bitrate
        );
        assert!(
            run.recovered.is_some(),
            "the bitrate must recover to 80% of the start within 30 s"
        );
        assert_eq!(
            run.expired, 0,
            "a full window must never starve the encoder"
        );
        trace.assert_never_latched();
    });
}

/// A 3 Mbit/s link for 10 s after 6 s unlimited: below what the session
/// sends, but far above Balanced's floor at 60 FPS. Bits per frame absorb
/// it: the pace stays at 55 or more from 1 s into the limit, and the viewer
/// receives 55 fps or more over its last 5 s (a full window skips capture
/// ticks at any pace).
#[test]
#[ignore = "explicit ~25-second moderate limit acceptance"]
fn moderate_limit_keeps_60() {
    traced(|trace| async move {
        let run = link_limit(
            &trace,
            "moderate_limit",
            TYPICAL,
            Settle::For(Duration::from_secs(6)),
            375_000,
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(
            run.status.state,
            ViewerState::Streaming,
            "{}",
            run.status.message
        );
        let from = run.limited + Duration::from_secs(1);
        for interval in run.limit.iter().filter(|interval| interval.at >= from) {
            assert!(interval.pace_fps >= 55, "the pace fell: {interval:?}");
        }
        let received = run.last_received();
        assert!(
            mean(&received) >= 55.0,
            "the viewer received less: {received:?}"
        );
        assert!(
            run.recovered.is_some(),
            "the bitrate must recover to 80% of the start within 30 s"
        );
        assert_eq!(run.expired, 0);
        trace.assert_never_latched();
    });
}

/// An unlimited link until bits per frame reach their maximum (8 Mbit/s at
/// 60 FPS, or for at most 40 s), then 5 Mbit/s for 10 s: the queue the drop
/// leaves peaks at 300 ms or less and is at 100 ms or less from 2 s in.
#[test]
#[ignore = "explicit ~30 to 70-second sudden drop acceptance"]
fn sudden_drop() {
    traced(|trace| async move {
        let run = link_limit(
            &trace,
            "sudden_drop",
            TYPICAL,
            Settle::BpfMax(Duration::from_secs(40)),
            625_000,
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(
            run.status.state,
            ViewerState::Streaming,
            "{}",
            run.status.message
        );
        let peak = run.highest(|interval| interval.ack_queue_ms);
        assert!(peak <= 300, "the queue peaked at {peak} ms");
        let from = run.limited + Duration::from_secs(2);
        for interval in run.limit.iter().filter(|interval| interval.at >= from) {
            assert!(interval.ack_queue_ms <= 100, "{interval:?}");
        }
        assert_eq!(run.expired, 0);
        trace.assert_never_latched();
    });
}

/// A route that becomes 60 ms longer each way, 120 ms on the round trip,
/// with no path change reported: no congestion. At most two receipt cuts
/// before the longer delay becomes the receipts' baseline, and the encoder
/// bitrate back where it was within 15 s.
#[test]
#[ignore = "explicit ~30-second added route delay acceptance"]
fn added_route_delay() {
    const ADDED_MS: u64 = 60;
    const BACK_WITHIN: Duration = Duration::from_secs(15);
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
        let rig = Rig::start("route-delay", Some(shape.clone()), TYPICAL).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        let started = Instant::now();
        tokio::time::sleep(Duration::from_secs(8)).await;
        let before = trace.last();
        let changed = Instant::now();
        shape
            .delay_ms
            .store(SHORT_PATH_MS + ADDED_MS, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(20)).await;
        let status = rig.viewer.status();
        rig.stop().await;
        let phase = |at: Instant| if at < changed { "short" } else { "longer" };
        for interval in trace.between(started, Instant::now()) {
            interval.print("added_route_delay", started, phase(interval.at));
        }
        let after = trace.between(changed, Instant::now());
        let last = *after.last().unwrap();
        let cuts = last.cuts - before.cuts;
        let lowest = after
            .iter()
            .map(|interval| interval.encoder_bitrate)
            .min()
            .unwrap();
        // From the last interval below the bitrate before the change.
        let back = after
            .iter()
            .rposition(|interval| interval.encoder_bitrate < before.encoder_bitrate)
            .map_or(Some(Duration::ZERO), |low| {
                after
                    .get(low + 1)
                    .map(|back| back.at.saturating_duration_since(changed))
            });
        let rebased = after.iter().any(|interval| interval.rebase_ms.is_some());
        println!(
            "added_route_delay one_way_ms={SHORT_PATH_MS}+{ADDED_MS} before_pace={} before_encoder_bitrate={} cuts={cuts} rebased={rebased} lowest_pace={} lowest_encoder_bitrate={lowest} back_after_ms={:?} final_encoder_bitrate={} state={:?}",
            before.pace_fps,
            before.encoder_bitrate,
            after
                .iter()
                .map(|interval| interval.pace_fps)
                .min()
                .unwrap(),
            back.map(|after| after.as_millis()),
            last.encoder_bitrate,
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert!(cuts <= 2, "a longer route is no congestion: {cuts} cuts");
        // Cuts that leave the route's queue where it was end in a rebase.
        assert!(
            cuts == 0 || rebased,
            "{cuts} cuts and no rebase onto the longer route"
        );
        assert!(
            back.is_some_and(|back| back <= BACK_WITHIN),
            "the encoder bitrate must be back at {} within {BACK_WITHIN:?}: {back:?}",
            before.encoder_bitrate
        );
        trace.assert_never_latched();
    });
}

/// Stalls of 300 ms every second on a short path, for 10 s, as a policer or
/// a lossy path makes: they recur, so they are congestion, and the encoder
/// bitrate falls.
#[test]
#[ignore = "explicit ~25-second recurring stall acceptance"]
fn recurring_stalls_cut() {
    const STALL: Duration = Duration::from_millis(300);
    const EVERY: Duration = Duration::from_secs(1);
    const STALLS: u32 = 10;
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
        let rig = Rig::start("recurring-stalls", Some(shape.clone()), TYPICAL).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        tokio::time::sleep(Duration::from_secs(8)).await;
        let before = trace.last();
        let stalled = Instant::now();
        for _ in 0..STALLS {
            shape
                .delay_ms
                .store(SHORT_PATH_MS + STALL.as_millis() as u64, Ordering::Relaxed);
            tokio::time::sleep(STALL).await;
            shape.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
            tokio::time::sleep(EVERY - STALL).await;
        }
        let ended = Instant::now();
        tokio::time::sleep(Duration::from_secs(3)).await;
        let status = rig.viewer.status();
        rig.stop().await;
        let phase = |at: Instant| if at < ended { "stalls" } else { "after" };
        for interval in trace.between(stalled, Instant::now()) {
            interval.print("recurring_stalls", stalled, phase(interval.at));
        }
        let during = trace.between(stalled, ended + session::STATS_INTERVAL);
        let lowest = |value: fn(&Interval) -> u64| during.iter().map(value).min().unwrap();
        let lowest_encoder = lowest(|interval| interval.encoder_bitrate);
        let last = *during.last().unwrap();
        println!(
            "recurring_stalls stall_ms={} every_ms={} before_encoder_bitrate={} lowest_encoder_bitrate={lowest_encoder} lowest_pace={} cuts={} received_fps={:.1} state={:?}",
            STALL.as_millis(),
            EVERY.as_millis(),
            before.encoder_bitrate,
            lowest(|interval| interval.pace_fps),
            last.cuts - before.cuts,
            status.stats.received_fps,
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert!(
            lowest_encoder < before.encoder_bitrate,
            "recurring stalls must lower the encoder bitrate from {}",
            before.encoder_bitrate
        );
    });
}

/// The same limit on an encoder that budgets per second. At the nominal model
/// it spends the nominal bitrate at any pace, so a lower pace sheds nothing:
/// only a cut of bits per frame, which lowers the nominal bitrate, does.
/// Reports whether and when the latch came, and asserts that the session
/// recovers as it does on a per-frame budget.
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

/// The standing queue a busy uplink gives the viewer's side of the link (a
/// cloud backup, a photo sync, a video call), on top of `SHORT_PATH_MS`.
const REVERSE_QUEUE_MS: u64 = 300;
/// Seconds of a clean link, of the queue, and after it is lifted.
const REVERSE_CLEAN: u64 = 6;
const REVERSE_HELD: u64 = 15;
const REVERSE_LIFTED: u64 = 5;

/// What one run of a reverse-queue case saw.
struct ReverseRun {
    status: ViewerStatus,
    /// The host's last interval before the queue.
    before: Interval,
    /// The host's intervals that cover only time under the queue.
    held: Vec<Interval>,
    /// The viewer's received fps, sampled once a second under the queue
    /// until it is lifted, from the first sample that cannot cover its first
    /// second: the viewer rolls the figure up once a second on a clock of its
    /// own, so a sample 2 s in may still be that second's, the round trip in
    /// which no receipt comes back and the window has yet to learn why.
    received: Vec<f32>,
}

/// A short path both ways, then for `REVERSE_HELD` s `REVERSE_QUEUE_MS` more
/// on the viewer's uplink only, with the link toward the viewer limited to
/// `down_rate` bytes a second unless that is 0, then both lifted. Prints, as
/// `case`, the host's every interval and the viewer's received fps.
async fn reverse_queue(trace: &Trace, case: &str, down_rate: u64) -> ReverseRun {
    let (up, down) = (Arc::new(Shape::default()), Arc::new(Shape::default()));
    for shape in [&up, &down] {
        shape.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
    }
    let rig = Rig::each_way(case, up.clone(), down.clone(), TYPICAL).await;
    let _decoder = Decoder::start(&rig.viewer, Duration::ZERO);
    let started = Instant::now();
    let (mut queued, mut lifted) = (None, None);
    let mut samples = Vec::new();
    for second in 0..REVERSE_CLEAN + REVERSE_HELD + REVERSE_LIFTED {
        if second == REVERSE_CLEAN {
            up.delay_ms
                .store(SHORT_PATH_MS + REVERSE_QUEUE_MS, Ordering::Relaxed);
            down.rate.store(down_rate, Ordering::Relaxed);
            queued = Some(Instant::now());
        } else if second == REVERSE_CLEAN + REVERSE_HELD {
            up.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
            down.rate.store(0, Ordering::Relaxed);
            lifted = Some(Instant::now());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        samples.push((Instant::now(), rig.viewer.status().stats.received_fps));
    }
    let status = rig.viewer.status();
    rig.stop().await;
    let (queued, lifted) = (queued.unwrap(), lifted.unwrap());
    let phase = |at: Instant| match at {
        at if at < queued => "clean",
        at if at < lifted => "queued",
        _ => "lifted",
    };
    for interval in trace.between(started, Instant::now()) {
        interval.print(case, started, phase(interval.at));
    }
    let received: Vec<(&str, f32)> = samples
        .iter()
        .map(|(at, fps)| (phase(*at), (fps * 10.0).round() / 10.0))
        .collect();
    println!(
        "{case} up_queue_ms={REVERSE_QUEUE_MS} down_rate={down_rate} received_fps={received:?} state={:?}",
        status.state
    );
    let before = trace
        .between(started, queued)
        .last()
        .copied()
        .unwrap_or_else(|| trace.last());
    ReverseRun {
        status,
        before,
        held: trace.between(
            queued + session::STATS_INTERVAL,
            lifted + Duration::from_millis(1),
        ),
        received: samples
            .iter()
            .filter(|(at, _)| (queued + Duration::from_secs(3)..lifted).contains(at))
            .map(|(_, fps)| *fps)
            .collect(),
    }
}

/// A queue on the viewer's uplink only is no congestion of the video toward
/// it: the pace holds at the ceiling, bits per frame hold, and the window is
/// never clamped to the delivery rate, while the viewer keeps receiving the
/// ceiling's frames.
#[test]
#[ignore = "explicit ~27-second reverse-path queue acceptance"]
fn reverse_path_queue_is_not_congestion() {
    traced(|trace| async move {
        let ReverseRun {
            status,
            before,
            held,
            received,
        } = reverse_queue(&trace, "reverse_queue", 0).await;
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert_eq!(before.pace_fps, CEILING_FPS, "{before:?}");
        assert!(!held.is_empty());
        for interval in &held {
            assert!(interval.pace_fps >= 55, "the pace fell: {interval:?}");
            assert!(!interval.window_clamped, "the window clamped: {interval:?}");
            assert!(
                interval.encoder_bitrate * 10 >= before.encoder_bitrate * 9,
                "bits per frame fell from {}: {interval:?}",
                before.encoder_bitrate
            );
        }
        assert!(
            received.iter().all(|fps| *fps >= 55.0),
            "the viewer received less: {received:?}"
        );
        trace.assert_never_latched();
    });
}

/// The same uplink queue while the link toward the viewer is limited to
/// 3 Mbit/s, below what the session sends: that is congestion, and the
/// controller still cuts.
#[test]
#[ignore = "explicit ~27-second reverse-path queue with a forward limit acceptance"]
fn reverse_queue_with_forward_limit_still_cuts() {
    traced(|trace| async move {
        let ReverseRun {
            status,
            before,
            held,
            ..
        } = reverse_queue(&trace, "reverse_queue_forward_limit", 375_000).await;
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        let lowest = |value: fn(&Interval) -> u64| held.iter().map(value).min().unwrap();
        assert!(
            lowest(|interval| interval.pace_fps) < CEILING_FPS
                || lowest(|interval| interval.encoder_bitrate) < before.encoder_bitrate,
            "a forward limit must cut: before {before:?}, then {held:?}"
        );
    });
}

/// A 1 Mbit/s link from the start, with IDRs twenty times a P-frame: each
/// takes over a second to cross. The P-frames behind the session's first IDR
/// wait for it rather than age out into another, the viewer does not ask for
/// it again, and bits per frame fall as on any slow link.
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

/// The reconnect case's 1 Mbit/s link, how long each of its sessions runs,
/// and how long after a viewer stops the next one connects: within the app's
/// reconnect backoff, and after the host saw the stop.
const RECONNECT_LINK_BYTES_PER_SECOND: u64 = 125_000;
const RECONNECT_SESSION: Duration = Duration::from_secs(8);
const RECONNECT_AFTER: Duration = Duration::from_secs(3);
/// A session's start: its first three trace intervals, each covering the
/// second before it.
const START_SPAN: Duration = Duration::from_secs(3);
const START_INTERVALS: usize = 3;
/// How long the last session of the reconnect case may take to climb.
const CLIMB_LIMIT: Duration = Duration::from_secs(20);

/// How one session of the reconnect case started.
struct SessionStart {
    /// From `Viewer::connect` until its first unit was presented.
    presented_after: Option<Duration>,
    first: Option<FirstPresented>,
    /// The host's first `START_INTERVALS` intervals of the session.
    start: Vec<Interval>,
}
impl SessionStart {
    fn read(trace: &Trace, connecting: Instant, decoder: &Decoder) -> Self {
        let first = decoder.first();
        Self {
            presented_after: first.map(|first| first.at.saturating_duration_since(connecting)),
            first,
            start: trace
                .between(
                    connecting,
                    connecting + START_SPAN + session::STATS_INTERVAL,
                )
                .into_iter()
                .take(START_INTERVALS)
                .collect(),
        }
    }
    fn ack_queue_peak_ms(&self) -> Option<u64> {
        self.start
            .iter()
            .map(|interval| interval.ack_queue_ms)
            .max()
    }
    fn print(&self, case: &str, session: &str) {
        let each =
            |value: fn(&Interval) -> u64| -> Vec<u64> { self.start.iter().map(value).collect() };
        println!(
            "{case} session={session} presented_after_ms={} first_bytes={:?} first_keyframe={:?} ack_queue_peak_ms={:?} start_ack_queue_ms={:?} start_bpf={:?} start_pace_fps={:?} start_sent_bitrate={:?}",
            self.presented_after.map_or_else(
                || "-".to_owned(),
                |after| format!("{:.1}", after.as_secs_f64() * 1000.0)
            ),
            self.first.map(|first| first.bytes),
            self.first.map(|first| first.keyframe),
            self.ack_queue_peak_ms(),
            each(|interval| interval.ack_queue_ms),
            each(|interval| interval.bpf),
            each(|interval| interval.pace_fps),
            each(|interval| interval.sent_bitrate),
        );
    }
}

/// Stop the rig's viewer and its decoder, then, `RECONNECT_AFTER` after the
/// stop, connect another viewer through `client`, with a decoder. Returns
/// how soon after the stop the host's status showed no viewer, when the new
/// viewer began to connect, and its decoder.
async fn reconnect(
    rig: &mut Rig,
    client: &Client,
    decoder: Decoder,
) -> (Option<Duration>, Instant, Decoder) {
    drop(decoder);
    rig.viewer.stop().await.unwrap();
    let stopped = Instant::now();
    let noticed = loop {
        if rig.host.status().viewer.is_none() {
            break Some(stopped.elapsed());
        }
        if stopped.elapsed() >= RECONNECT_AFTER {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    tokio::time::sleep_until((stopped + RECONNECT_AFTER).into()).await;
    let connecting = Instant::now();
    rig.viewer = connect_viewer(client, &rig.host).await;
    rig.connecting = connecting;
    let decoder = Decoder::start(&rig.viewer, Duration::ZERO);
    (noticed, connecting, decoder)
}

/// A 1 Mbit/s link: a viewer watches for 8 s, stops, and 3 s later connects
/// again over the same link for 8 s; 3 s after that one stops, a viewer on
/// the relay's unlimited link connects. Reports, for each session, the time
/// from `Viewer::connect` to its first presented picture, that picture's
/// bytes, and the host's ack queue over its first 3 s; and for the last, how
/// long its encoder bitrate took to reach 80% of Balanced's start. The
/// reconnect starts at the rate the first session ended at, so its IDR is
/// smaller than the first session's.
#[test]
#[ignore = "explicit ~45-second reconnect measurement on a 1 Mbit/s link"]
fn reconnect_on_a_slow_link() {
    const CASE: &str = "reconnect";
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape
            .rate
            .store(RECONNECT_LINK_BYTES_PER_SECOND, Ordering::Relaxed);
        let mut rig = Rig::start("reconnect", Some(shape), TYPICAL).await;
        let (slow, fast) = (rig.clients[1].clone(), rig.clients[0].clone());
        let cold_at = rig.connecting;
        let decoder = Decoder::start(&rig.viewer, Duration::ZERO);
        tokio::time::sleep(RECONNECT_SESSION).await;
        let cold = SessionStart::read(&trace, cold_at, &decoder);
        let cold_status = rig.viewer.status();
        let cold_end = trace.last();
        let (cold_noticed, again_at, decoder) = reconnect(&mut rig, &slow, decoder).await;
        tokio::time::sleep(RECONNECT_SESSION).await;
        let again = SessionStart::read(&trace, again_at, &decoder);
        let again_status = rig.viewer.status();
        let again_end = trace.last();
        let (again_noticed, fast_at, decoder) = reconnect(&mut rig, &fast, decoder).await;
        let climbed = first_after(&trace, fast_at, CLIMB_LIMIT, |interval| {
            interval.encoder_bitrate * 5 >= START_BPS * 4
        })
        .await;
        // Its start's last interval is traced by then.
        tokio::time::sleep_until((fast_at + START_SPAN + session::STATS_INTERVAL).into()).await;
        let fast_start = SessionStart::read(&trace, fast_at, &decoder);
        let fast_status = rig.viewer.status();
        drop(decoder);
        rig.stop().await;
        for (phase, from, to) in [
            ("cold", cold_at, cold_at + RECONNECT_SESSION),
            ("again", again_at, again_at + RECONNECT_SESSION),
            ("fast", fast_at, Instant::now()),
        ] {
            for interval in trace.between(from, to) {
                interval.print(CASE, from, phase);
            }
        }
        cold.print(CASE, "cold");
        again.print(CASE, "again");
        fast_start.print(CASE, "fast");
        println!(
            "{CASE} summary link_kbit_s={} cold_end_bpf={} cold_end_pace={} host_saw_stop_ms={:?} again_end_bpf={} again_end_pace={} host_saw_second_stop_ms={:?} fast_at_80pct_of_start_after_ms={:?}",
            RECONNECT_LINK_BYTES_PER_SECOND * 8 / 1000,
            cold_end.bpf,
            cold_end.pace_fps,
            cold_noticed.map(|after| after.as_millis()),
            again_end.bpf,
            again_end.pace_fps,
            again_noticed.map(|after| after.as_millis()),
            climbed.map(|after| after.as_millis()),
        );
        for status in [&cold_status, &again_status, &fast_status] {
            assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        }
        assert!(
            cold_noticed.is_some() && again_noticed.is_some(),
            "the host must see each stop before the next viewer connects"
        );
        for start in [&cold, &again, &fast_start] {
            assert!(
                start.first.is_some_and(|first| first.keyframe),
                "a session's first picture is its IDR"
            );
        }
        let bytes = |start: &SessionStart| start.first.map_or(0, |first| first.bytes);
        assert!(
            bytes(&again) < bytes(&cold),
            "the reconnect's IDR must be smaller than the first session's"
        );
        assert!(
            climbed.is_some(),
            "the last viewer's bitrate must reach 80% of the start within {CLIMB_LIMIT:?}"
        );
        trace.assert_never_latched();
    });
}

fn mean_pace(intervals: &[Interval]) -> f64 {
    intervals
        .iter()
        .map(|interval| interval.pace_fps)
        .sum::<u64>() as f64
        / intervals.len().max(1) as f64
}

fn caps(intervals: &[Interval]) -> Vec<Option<u64>> {
    intervals
        .iter()
        .map(|interval| interval.viewer_cap)
        .collect()
}

/// The 95th percentile, by rank.
fn p95(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    let rank = (values.len() * 95).div_ceil(100);
    values.get(rank.checked_sub(1)?).copied()
}

/// A decoder that takes 67 ms per frame: the viewer's fps cap brings the host
/// down to just below what it decodes, while the host's own controller, its
/// bits per frame and its rate model stay where the network puts them. Once
/// the cap has found the decoder, after the first 10 s, it holds the pace at
/// 13 to 15 without a backlog episode or a keyframe request.
#[test]
#[ignore = "explicit ~60-second slow presenter acceptance"]
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
#[ignore = "explicit ~60-second per-second encoder acceptance under an fps cap"]
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

/// The first interval in `from..` that `found` accepts, waiting up to `limit`
/// for it; how long after `from` it was traced.
async fn first_after(
    trace: &Trace,
    from: Instant,
    limit: Duration,
    found: impl Fn(&Interval) -> bool,
) -> Option<Duration> {
    loop {
        let first = trace
            .between(from, Instant::now())
            .into_iter()
            .find(|interval| found(interval));
        if first.is_some() || from.elapsed() > limit {
            return first.map(|interval| interval.at - from);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A decoder that takes 67 ms per frame for 20 s, then 5 ms: the fps cap
/// that held the host down to the slow decoder must go once it is fast.
/// Reports how long after the speed-up the host's trace showed no cap, which
/// must be no longer than before the cap remembered capacities.
#[test]
#[ignore = "explicit ~45-second decoder speed-up measurement"]
fn slow_decoder_speeds_up() {
    traced(|trace| async move {
        let rig = Rig::start("speeds-up", None, TYPICAL).await;
        let pace = Pace::new(Duration::from_millis(67));
        let _decoder = Decoder::paced(&rig.viewer, pace.clone());
        tokio::time::sleep(Duration::from_secs(20)).await;
        pace.set(Duration::from_millis(5));
        let faster = Instant::now();
        let removed = first_after(&trace, faster, Duration::from_secs(40), |interval| {
            interval.viewer_cap.is_none()
        })
        .await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let status = rig.viewer.status();
        rig.stop().await;
        println!(
            "slow_decoder_speeds_up decode_ms=67->5 at_s=20 cap_removed_after_ms={:?} caps_slow={:?} caps_fast={:?} backlog_drops={} keyframes_requested={} received_fps={:.1} state={:?}",
            removed.map(|after| after.as_millis()),
            caps(&trace.between(faster - Duration::from_secs(10), faster)),
            caps(&trace.between(faster, Instant::now())),
            status.stats.backlog_drops,
            status.stats.keyframes_requested,
            status.stats.received_fps,
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        // The slowest of three runs before the cap remembered the decoder's
        // capacity (docs/benchmarks/2026-11-24-viewer-fps-cap).
        assert!(
            removed.is_some_and(|after| after <= Duration::from_secs(12)),
            "the cap must go within 12 s of the decoder getting fast: {removed:?}"
        );
    });
}

/// What one run of the slow-presenter case saw.
struct PresenterRun {
    /// The trace intervals after the first 10 s.
    steady: Vec<Interval>,
    rate_model: RateModelRun,
}

/// A decoder that takes 67 ms per frame, for 60 s, on an encoder of `sizes`;
/// printed as `case`. Asserts what holds for any encoder: once the viewer's
/// fps cap has found the decoder, after the first 10 s, it holds the pace at
/// 13 to 15 without a backlog episode or a keyframe request, and the session
/// stays healthy.
async fn slow_presenter(trace: &Trace, case: &str, sizes: Sizes) -> PresenterRun {
    let rig = Rig::start(case, None, sizes).await;
    let _decoder = Decoder::start(&rig.viewer, Duration::from_millis(67));
    tokio::time::sleep(Duration::from_secs(10)).await;
    let settled = (Instant::now(), rig.viewer.status());
    tokio::time::sleep(Duration::from_secs(50)).await;
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
    let steady = trace.between(settled.0, measured);
    let mean_pace = mean_pace(&steady);
    let last = *steady.last().unwrap();
    let backlog_drops = status.stats.backlog_drops - settled.1.stats.backlog_drops;
    let keyframes_requested =
        status.stats.keyframes_requested - settled.1.stats.keyframes_requested;
    println!(
        "{case} decode_ms=67 seconds=10..60 mean_pace_fps={mean_pace:.1} caps={:?} fps={} encoder_bitrate={} presented_fps={:.1} backlog_drops={backlog_drops} keyframes_requested={keyframes_requested} first_10s_backlog_drops={} first_10s_keyframes_requested={} expired_encoder_outputs={} {rate_model} state={:?}",
        caps(&steady),
        last.fps,
        last.encoder_bitrate,
        status.stats.presented_fps,
        settled.1.stats.backlog_drops,
        settled.1.stats.keyframes_requested,
        rig.expired(),
        status.state
    );
    assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
    assert!(
        steady.iter().all(|interval| interval.viewer_cap.is_some()),
        "the fps cap must hold the host down"
    );
    assert!((13.0..=15.0).contains(&mean_pace), "{mean_pace}");
    assert_eq!(
        (backlog_drops, keyframes_requested),
        (0, 0),
        "the cap stays below the decoder after the first 10 s"
    );
    assert!(steady.iter().all(|interval| !interval.unresponsive));
    assert_eq!(rig.expired(), 0);
    PresenterRun { steady, rate_model }
}

/// A decoder that queues up to four units inside itself, as the Linux
/// runner's GStreamer pipeline does, and decodes each in 67 ms: what it
/// holds is lag the sink's queue does not see. Once the cap has found it,
/// after the first 10 s, no backlog episode comes. Reports the pace, the caps
/// and the 95th percentile from receipt to presentation.
#[test]
#[ignore = "explicit ~30-second pipelined slow consumer acceptance"]
fn pipelined_slow_consumer() {
    traced_views(|trace| async move {
        let rig = Rig::start("pipelined", None, TYPICAL).await;
        let _decoder = Pipelined::start(&rig.viewer, 4, Duration::from_millis(67));
        tokio::time::sleep(Duration::from_secs(10)).await;
        let settled = (Instant::now(), rig.viewer.status());
        tokio::time::sleep(Duration::from_secs(20)).await;
        let measured = Instant::now();
        let status = rig.viewer.status();
        rig.stop().await;
        let steady = trace.between(settled.0, measured);
        let views = trace.views(settled.0, measured);
        let backlog_drops = status.stats.backlog_drops - settled.1.stats.backlog_drops;
        let keyframes_requested =
            status.stats.keyframes_requested - settled.1.stats.keyframes_requested;
        println!(
            "pipelined_slow_consumer depth=4 decode_ms=67 seconds=10..30 mean_pace_fps={:.1} caps={:?} presented={} presented_after_p95_ms={:?} backlog_drops={backlog_drops} keyframes_requested={keyframes_requested} first_10s_backlog_drops={} presented_fps={:.1} state={:?}",
            mean_pace(&steady),
            caps(&steady),
            views.len(),
            p95(views.iter().map(View::presented_after_us).collect()).map(|us| us / 1000),
            settled.1.stats.backlog_drops,
            status.stats.presented_fps,
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert_eq!(
            backlog_drops, 0,
            "the cap stays below the decoder after the first 10 s"
        );
    });
}

/// A 120 fps session (the user's own rate, so every preset's ceiling) whose
/// decoder takes 10 ms a unit, just short of the rate. Reports, from 10 s to
/// 30 s, the pace and the caps, the sink's wait (receipt to hand-off) at the
/// 95th percentile, backlog episodes and units the sink discarded.
#[test]
#[ignore = "explicit ~30-second 120 fps near-capacity measurement"]
fn fast_consumer_at_120() {
    traced_views(|trace| async move {
        let rig = Rig::at_fps("fast-120", None, TYPICAL, ENCODE_TIME, 120).await;
        let _decoder = Decoder::start(&rig.viewer, Duration::from_millis(10));
        tokio::time::sleep(Duration::from_secs(10)).await;
        let settled = (Instant::now(), Counters::read(&rig.viewer));
        tokio::time::sleep(Duration::from_secs(20)).await;
        let measured = Instant::now();
        let after = Counters::read(&rig.viewer);
        rig.stop().await;
        let steady = trace.between(settled.0, measured);
        let views = trace.views(settled.0, measured);
        let (before, status) = (&settled.1.status.stats, &after.status.stats);
        println!(
            "fast_consumer_at_120 decode_ms=10 seconds=10..30 mean_pace_fps={:.1} caps={:?} presented={} sink_wait_p95_ms={:?} presented_after_p95_ms={:?} backlog_drops={} keyframes_requested={} sink_discarded={} presented_fps={:.1} state={:?}",
            mean_pace(&steady),
            caps(&steady),
            views.len(),
            p95(views.iter().map(View::sink_wait_us).collect()).map(|us| us as f64 / 1000.0),
            p95(views.iter().map(View::presented_after_us).collect()).map(|us| us as f64 / 1000.0),
            status.backlog_drops - before.backlog_drops,
            status.keyframes_requested - before.keyframes_requested,
            after.drops.1 - settled.1.drops.1,
            status.presented_fps,
            after.status.state
        );
        assert_eq!(
            after.status.state,
            ViewerState::Streaming,
            "{}",
            after.status.message
        );
    });
}

/// A 120 fps session on a short path whose decoder, otherwise fast, takes
/// 20 ms a unit for one second: the fps cap it brings must go again.
/// Reports how long after that second the viewer received 115 fps or more:
/// under 10 s, with raises scaled to the ceiling.
#[test]
#[ignore = "explicit ~40-second 120 fps backlog recovery measurement"]
fn injected_backlog_at_120_recovers() {
    traced(|trace| async move {
        let shape = Arc::new(Shape::default());
        shape.delay_ms.store(SHORT_PATH_MS, Ordering::Relaxed);
        let rig = Rig::at_fps("backlog-120", Some(shape), TYPICAL, ENCODE_TIME, 120).await;
        let pace = Pace::new(Duration::from_millis(1));
        let _decoder = Decoder::paced(&rig.viewer, pace.clone());
        let started = Instant::now();
        let at_120 = first_after(&trace, started, Duration::from_secs(15), |interval| {
            interval.pace_fps >= 120
        })
        .await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let before = rig.viewer.status().stats.received_fps;
        pace.set(Duration::from_millis(20));
        tokio::time::sleep(Duration::from_secs(1)).await;
        pace.set(Duration::from_millis(1));
        let ended = Instant::now();
        let recovered = loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if rig.viewer.status().stats.received_fps >= 115.0 {
                break Some(ended.elapsed());
            }
            if ended.elapsed() > Duration::from_secs(30) {
                break None;
            }
        };
        let status = rig.viewer.status();
        rig.stop().await;
        println!(
            "injected_backlog_at_120 one_way_ms={SHORT_PATH_MS} decode_ms=1,20_for_1s,1 pace_120_after_ms={:?} received_fps_before={before:.1} received_115_after_ms={:?} caps={:?} backlog_drops={} keyframes_requested={} state={:?}",
            at_120.map(|after| after.as_millis()),
            recovered.map(|after| after.as_millis()),
            caps(&trace.between(ended - Duration::from_secs(1), Instant::now())),
            status.stats.backlog_drops,
            status.stats.keyframes_requested,
            status.state
        );
        assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
        assert!(at_120.is_some(), "the pace must reach 120 first");
        // Raised by 5 a second, it took 15 s; by 20, it must take under 10.
        assert!(
            recovered.is_some_and(|after| after < Duration::from_secs(10)),
            "received fps must come back to 115 within 10 s: {recovered:?}"
        );
    });
}

/// A 2 ms decoder that stalls once for 400 ms, 10 s into the session, as a
/// decoder thread can behind GPU contention. Reports the time from the
/// stall's end to the first presentation of a unit received after it began,
/// the longest interval between presentations, and the keyframe requests and
/// backlog episodes the stall brought. The decoder drains what queued, so the
/// fresh picture comes within 100 ms of the stall's end, IDR or not.
async fn consumer_stall(trace: Trace, name: &str, link_bytes_per_second: u64) {
    const STALL: Duration = Duration::from_millis(400);
    let shape = Arc::new(Shape::default());
    shape.rate.store(link_bytes_per_second, Ordering::Relaxed);
    let rig = Rig::start(name, Some(shape), TYPICAL).await;
    let pace = Pace::new(Duration::from_millis(2));
    let _decoder = Decoder::paced(&rig.viewer, pace.clone());
    let started = Instant::now();
    tokio::time::sleep(Duration::from_secs(10)).await;
    let before = rig.viewer.status();
    let pace_fps = trace.last().pace_fps;
    pace.stall(STALL);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let status = rig.viewer.status();
    rig.stop().await;
    let stall = pace.stalled();
    let fresh_after = stall
        .ended
        .zip(stall.first_fresh)
        .map(|(ended, fresh)| fresh.saturating_duration_since(ended));
    let intervals = trace.between(started, Instant::now());
    println!(
        "consumer_stall_drains link_kib_s={} stall_ms={} pace_fps={pace_fps} fresh_presented_after_stall_ms={:?} present_interval_max_ms={} keyframes_requested={} backlog_drops={} received_fps={:.1} paces={:?} caps={:?} state={:?}",
        link_bytes_per_second / 1024,
        STALL.as_millis(),
        fresh_after.map(|after| after.as_secs_f64() * 1000.0),
        status.stats.present_interval.max_ms,
        status.stats.keyframes_requested - before.stats.keyframes_requested,
        status.stats.backlog_drops - before.stats.backlog_drops,
        status.stats.received_fps,
        intervals
            .iter()
            .map(|interval| interval.pace_fps)
            .collect::<Vec<_>>(),
        caps(&intervals),
        status.state
    );
    assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
    assert!(stall.ended.is_some(), "the decoder must have stalled");
    // The decoder drains what queued during its stall instead of waiting for
    // the IDR the stall asked for.
    assert!(
        fresh_after.is_some_and(|after| after < Duration::from_millis(100)),
        "a unit received after the stall began must be presented within 100 ms of its end: {fresh_after:?}"
    );
}

#[test]
#[ignore = "explicit ~15-second consumer stall measurement"]
fn consumer_stall_drains() {
    traced(|trace| consumer_stall(trace, "stall-drains", 0));
}

/// The same on a 1 Mbit/s link, where an IDR takes a while to cross.
#[test]
#[ignore = "explicit ~15-second consumer stall measurement on a 1 Mbit/s link"]
fn consumer_stall_drains_on_a_slow_link() {
    traced(|trace| consumer_stall(trace, "stall-drains-slow", 125_000));
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

/// The name the closed loop's QUIC certificate carries.
const QUIC_NAME: &str = "localhost";

/// The host's end and the viewer's of one QUIC connection with pb-mapper's
/// direct transport, and the viewer's config: the host's end sends under
/// `congestion` and the viewer's under Cubic, as a direct session's do.
fn quic_ends(
    congestion: CongestionControl,
) -> (quinn::Endpoint, quinn::Endpoint, quinn::ClientConfig) {
    use quinn::rustls;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let (rcgen::CertifiedKey { cert, .. }, key) =
        rcgen::generate_simple_self_signed(vec![QUIC_NAME.into()]).unwrap();
    let cert = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server).unwrap(),
    ));
    server.transport_config(Arc::new(direct_transport_config(congestion).unwrap()));
    let mut client = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client).unwrap(),
    ));
    client.transport_config(Arc::new(
        direct_transport_config(CongestionControl::Cubic).unwrap(),
    ));
    let endpoint = |server: Option<quinn::ServerConfig>| {
        let socket = lossy_udp_socket(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let mut config = quinn::EndpointConfig::default();
        config.grease_quic_bit(false);
        quinn::Endpoint::new(config, server, socket, Arc::new(quinn::TokioRuntime)).unwrap()
    };
    (endpoint(Some(server)), endpoint(None), client)
}

/// A relay the host registers on, a host of the synthetic encoder, and a
/// viewer that reaches the host's listener as a direct session does: over
/// one QUIC connection through a lossy hop, the host's end dialing its
/// listener and sending the video under the controller the case names.
struct QuicRig {
    host: Host,
    viewer: Viewer,
    probe: Arc<Probe>,
    /// The host's end of the connection, whose path stats are read.
    sending: quinn::Connection,
    hop: LossyUdp,
    /// The host's end's forwarding, aborted with the rig.
    _forward: tokio::task::JoinSet<()>,
    _endpoints: [quinn::Endpoint; 2],
    _client: Client,
    _relay: Relay,
}
impl QuicRig {
    async fn start(name: &str, congestion: CongestionControl, shape: Arc<LossyShape>) -> Self {
        let relay = Relay::start(&format!("desktop-{name}")).await;
        let client =
            Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
        let probe = Arc::new(Probe::default());
        let source = probe.clone();
        let mut options = HostOptions::new(format!("desktop:{name}"));
        options.allow_control = true;
        options.allow_direct = false;
        let host = Host::builder(options)
            .backend(move || {
                Ok(SyntheticEncoder {
                    probe: source.clone(),
                    sizes: TYPICAL,
                    latency: ENCODE_TIME,
                    serial: false,
                    nominal_fps: 0,
                    frames: 0,
                    last_capture: None,
                    pending: VecDeque::new(),
                })
            })
            .start(&client)
            .await
            .unwrap();
        let (host_end, viewer_end, config) = quic_ends(congestion);
        let hop = lossy_udp(host_end.local_addr().unwrap(), shape).await;
        let connecting = viewer_end
            .connect_with(config, hop.addr(), QUIC_NAME)
            .unwrap();
        let (to_host, sending) =
            tokio::join!(connecting, async { host_end.accept().await.unwrap().await });
        let (to_host, sending) = (to_host.unwrap(), sending.unwrap());
        // The host's end dials the listener for the stream the viewer
        // opens, as a direct provider dials its local service.
        let mut forward = tokio::task::JoinSet::new();
        let (accepting, listening) = (sending.clone(), host.listening());
        forward.spawn(async move {
            let Ok((send, recv)) = accepting.accept_bi().await else {
                return;
            };
            let Ok(mut service) = tokio::net::TcpStream::connect(listening).await else {
                return;
            };
            let _ = service.set_nodelay(true);
            let mut quic = tokio::io::join(recv, send);
            let _ = tokio::io::copy_bidirectional(&mut quic, &mut service).await;
        });
        let (send, recv) = to_host.open_bi().await.unwrap();
        let invitation = host.invitation();
        let tls = security::client(invitation.fingerprint)
            .unwrap()
            .connect(
                quinn::rustls::pki_types::ServerName::try_from(security::NAME).unwrap(),
                tokio::io::join(recv, send),
            )
            .await
            .unwrap();
        let viewer = Viewer::over_stream_for_test(
            tls,
            invitation.token_text(),
            ViewerConfig {
                codecs: vec![VideoCodec::H264],
                limits: VideoLimits::for_viewer(CEILING_FPS as f64),
                fps_override: false,
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
            sending,
            hop,
            _forward: forward,
            _endpoints: [host_end, viewer_end],
            _client: client,
            _relay: relay,
        }
    }
    async fn stop(&self) {
        self.viewer.stop().await.unwrap();
        self.host.stop().await.unwrap();
    }
}

/// One unit a `Timed` decoder took: when it was received whole, how long
/// after its capture, and whether it was an IDR.
#[derive(Clone, Copy)]
struct Taken {
    received: Instant,
    latency: Duration,
    keyframe: bool,
}

/// A decoder without decode time that presents every unit and notes how
/// long after its capture, as the synthetic encoder noted it, each was
/// received whole.
struct Timed {
    stop: Arc<AtomicBool>,
    taken: Arc<Mutex<Vec<Taken>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Timed {
    fn start(viewer: &Viewer, probe: Arc<Probe>) -> Self {
        let consumer = viewer.video_sink().attach().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let taken = Arc::new(Mutex::new(Vec::new()));
        let (done, noted) = (stop.clone(), taken.clone());
        let thread = traced_thread(move || {
            while !done.load(Ordering::Acquire) {
                match consumer.next(Duration::from_millis(50), wire::MAX_PAYLOAD) {
                    Pull::Unit(unit) => {
                        let captured =
                            (unit.pts_us / PTS_STEP_US)
                                .checked_sub(1)
                                .and_then(|index| {
                                    probe.captured.lock().unwrap().get(index as usize).copied()
                                });
                        if let Some(captured) = captured {
                            noted.lock().unwrap().push(Taken {
                                received: unit.received_at,
                                latency: unit.received_at.saturating_duration_since(captured),
                                keyframe: unit.keyframe,
                            });
                        }
                        consumer.presented(unit.generation, unit.frame_id);
                    }
                    Pull::Closed => break,
                    _ => {}
                }
            }
        });
        Self {
            stop,
            taken,
            thread: Some(thread),
        }
    }
    fn taken(&self) -> Vec<Taken> {
        self.taken.lock().unwrap().clone()
    }
}
impl Drop for Timed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

/// The links the closed loop runs on: a name, the round trip in ms, the
/// loss each way and the policer's rate in bit/s (0 for none).
const QUIC_LINKS: [(&str, u64, f64, u64); 5] = [
    ("rtt60_loss0.1", 60, 0.001, 0),
    ("rtt60_loss1", 60, 0.01, 0),
    ("rtt120_loss0.1", 120, 0.001, 0),
    ("rtt120_loss1", 120, 0.01, 0),
    ("rtt60_policer5", 60, 0.0, 5_000_000),
];
/// How long a session runs on its lossy link before it is measured, and
/// for how long.
const QUIC_SETTLE: Duration = Duration::from_secs(6);
const QUIC_MEASURE: Duration = Duration::from_secs(20);
/// How often the viewer asks for an IDR, as a refresh does.
const QUIC_IDR_EVERY: Duration = Duration::from_secs(2);

/// `values`' `p`th percentile by nearest rank, in ms.
fn percentile_ms(values: &mut [Duration], p: f64) -> Option<f64> {
    values.sort_unstable();
    let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
    values
        .get(rank.max(1) - 1)
        .map(|value| value.as_secs_f64() * 1000.0)
}

/// The closed loop over a direct QUIC connection: the real session, its
/// receipt-driven rate control on, with the host's end under Cubic and
/// then BBR on each of `QUIC_LINKS`, where loss is random and each way, or
/// a policer drops what its bucket cannot pay for. After `QUIC_SETTLE` the
/// viewer asks for an IDR every `QUIC_IDR_EVERY` for `QUIC_MEASURE`.
/// Reports, per run: P-frame and IDR latency from capture to received
/// whole, the wait from each request to its IDR, the received fps, the
/// host's mean sent bitrate and pace and its rate cuts, and the host's
/// packets sent and lost, congestion events and window from quinn. It
/// asserts only that each session stayed up.
#[test]
#[ignore = "explicit ~5-minute measurement of the host's direct QUIC under Cubic and BBR on lossy links"]
fn direct_quic_on_lossy_links() {
    for (link, rtt_ms, loss, policer_bps) in QUIC_LINKS {
        for congestion in [CongestionControl::Cubic, CongestionControl::Bbr] {
            traced(|trace| async move {
                let shape = LossyShape::new(0.0, Duration::from_millis(rtt_ms) / 2, policer_bps);
                let name = format!("quic-{link}-{}", congestion.as_str());
                let rig = QuicRig::start(&name, congestion, shape.clone()).await;
                shape.set_loss(loss);
                let timed = Timed::start(&rig.viewer, rig.probe.clone());
                tokio::time::sleep(QUIC_SETTLE).await;
                let started = Instant::now();
                let before = rig.sending.stats().path;
                let mut asked = Vec::new();
                let mut fps = Vec::new();
                while started.elapsed() < QUIC_MEASURE {
                    asked.push(Instant::now());
                    rig.viewer.request_keyframe(KeyframeReason::User);
                    tokio::time::sleep(QUIC_IDR_EVERY).await;
                    fps.push(rig.viewer.status().stats.received_fps);
                }
                let ended = Instant::now();
                let after = rig.sending.stats().path;
                let status = rig.viewer.status();
                let taken = timed.taken();
                drop(timed);
                let [up, down] = rig.hop.counts();
                rig.stop().await;

                let within: Vec<Taken> = taken
                    .iter()
                    .filter(|unit| (started..ended).contains(&unit.received))
                    .copied()
                    .collect();
                let latencies = |keyframe: bool| -> Vec<Duration> {
                    within
                        .iter()
                        .filter(|unit| unit.keyframe == keyframe)
                        .map(|unit| unit.latency)
                        .collect()
                };
                let (mut frames, mut idrs) = (latencies(false), latencies(true));
                let mut waits: Vec<Duration> = asked
                    .iter()
                    .filter_map(|asked| {
                        taken
                            .iter()
                            .find(|unit| unit.keyframe && unit.received >= *asked)
                            .map(|unit| unit.received - *asked)
                    })
                    .collect();
                let answered = waits.len();
                let intervals = trace.between(started, ended);
                let average = |value: fn(&Interval) -> u64| {
                    intervals.iter().map(value).sum::<u64>() as f64 / intervals.len().max(1) as f64
                };
                let ms = |value: Option<f64>| {
                    value.map_or_else(|| "-".to_owned(), |ms| format!("{ms:.1}"))
                };
                let sent = after.sent_packets - before.sent_packets;
                let lost = after.lost_packets - before.lost_packets;
                println!(
                    "direct_quic link={link} cc={} p_frames={} p_p50={} p_p95={} p_p99={} idrs={} idr_p50={} idr_p95={} asked={} answered={answered} idr_wait_p50={} idr_wait_p95={} received_fps_mean={:.1} sent_bitrate_mean={:.0} pace_mean={:.1} cuts={} sent_packets={sent} lost_packets={lost} lost_pct={:.2} congestion_events={} cwnd={} hop_lost={} hop_policed={} state={:?}",
                    congestion.as_str(),
                    frames.len(),
                    ms(percentile_ms(&mut frames, 50.0)),
                    ms(percentile_ms(&mut frames, 95.0)),
                    ms(percentile_ms(&mut frames, 99.0)),
                    idrs.len(),
                    ms(percentile_ms(&mut idrs, 50.0)),
                    ms(percentile_ms(&mut idrs, 95.0)),
                    asked.len(),
                    ms(percentile_ms(&mut waits, 50.0)),
                    ms(percentile_ms(&mut waits, 95.0)),
                    mean(&fps),
                    average(|interval| interval.sent_bitrate),
                    average(|interval| interval.pace_fps),
                    intervals.last().map_or(0, |interval| interval.cuts),
                    100.0 * lost as f64 / sent.max(1) as f64,
                    after.congestion_events - before.congestion_events,
                    after.cwnd,
                    up.lost + down.lost,
                    up.policed + down.policed,
                    status.state,
                );
                assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
            });
        }
    }
}
