//! Heap allocations, and the bytes they ask for, per video unit on the remote
//! desktop's paths, end to end in one process: a relay, a host whose H.264
//! "encoder" returns one 4 KiB unit per capture, and a viewer whose renderer
//! presents every unit at once. The first window streams video only; the
//! second also moves the pointer every 4 ms under control, which feeds the
//! host's input lane and its input wait window; the third adds a second
//! controlling viewer moving its pointer in tandem, exercising zero-copy video
//! fan-out and `peer_cursors` broadcasts (`units_per_s` stays measured per host
//! encoded unit via the first viewer). Allocations are counted for
//! the whole process, so the viewer's and the relay's are in every figure:
//! compare runs of this harness with each other only. A reallocation counts
//! as one allocation of its new size.
//!
//! The host reads shapes (`cursor_shapes`) but shows no pointer through the
//! cases above, as before. Then it does, and the pointer takes another
//! shape in each half of the display: `shape` times the first viewer's move
//! into the other half until its view names the new shape, and
//! `host_pointer` moves the host's pointer as someone at the host would and
//! times until a viewer that only watches sees it there, in a room of that
//! viewer alone at 60 and at 120 FPS. Each sample waits 20 to 60 ms after
//! the last, so it lands anywhere in the host's pointer cadence; the times
//! include the 100 µs this thread polls the view at.
//!
//! ```text
//! cargo run --release -p pb-mapper-desktop --example host_send_bench
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use pb_mapper_desktop::{
    Backend, BackendCaps, CursorKind, CursorPosition, CursorShape, Description, DesktopContext,
    DesktopView, Display, EncodedFrame, Host, HostOptions, Input, Pull, QualityPreset, Result,
    VideoCodec, VideoFormat, VideoLimits, VideoRate, Viewer, ViewerConfig,
};
use pb_mapper_testkit::{Relay, admin_credential};

/// The system allocator, counting each allocation and reallocation and the
/// bytes each asks for.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

fn count(bytes: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ALLOCATED.fetch_add(bytes as u64, Ordering::Relaxed);
}

// SAFETY: every method forwards its arguments unchanged to `System`, which
// upholds the `GlobalAlloc` contract; counting touches no memory it returns.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        // SAFETY: the caller's `layout` meets `alloc`'s requirements.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        // SAFETY: as for `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size);
        // SAFETY: `ptr` came from this allocator with `layout`, as the caller
        // guarantees, and this allocator is `System`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as for `realloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const UNIT_BYTES: usize = 4096;
const WINDOW: Duration = Duration::from_secs(5);
const POINTER_EVERY: Duration = Duration::from_millis(4);

/// The pointer shows (`Pointer`'s top bit) at the x and y in its low bits.
const SHOWN: u64 = 1 << 63;

/// Where the host's pointer is, for the backend and the benchmark thread;
/// it shows only once the pointer cases start.
#[derive(Clone, Default)]
struct Pointer(Arc<AtomicU64>);

impl Pointer {
    fn show(&self, x: u32, y: u32) {
        self.0.store(
            SHOWN | (u64::from(x) << 32) | u64::from(y),
            Ordering::Release,
        );
    }

    fn at(&self) -> Option<(u32, u32)> {
        let packed = self.0.load(Ordering::Acquire);
        (packed & SHOWN != 0).then_some(((packed >> 32) as u32 & 0x7fff_ffff, packed as u32))
    }
}

/// An H.264 desktop: every capture is one opaque unit, an IDR when asked.
/// Its pointer goes where the viewer moves it, and is an arrow on the left
/// half of the display and a hand on the right.
struct Encoded {
    frames: u64,
    inputs: Arc<AtomicU64>,
    pointer: Pointer,
    /// The half whose shape `cursor_shape` last returned.
    shown: Option<u32>,
}

impl Backend for Encoded {
    fn describe(&mut self, _: bool) -> Result<Description> {
        let mut caps = BackendCaps::default();
        caps.cursor_shapes = true;
        Ok(Description {
            display: self.display()?,
            caps,
        })
    }

    fn display(&mut self) -> Result<Display> {
        Ok(Display {
            id: 1,
            name: "bench desktop".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        })
    }

    fn capture_video(
        &mut self,
        _: &DesktopContext,
        _: VideoCodec,
        keyframe: bool,
        _: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        self.frames += 1;
        Ok(Some(EncodedFrame {
            format: VideoFormat {
                codec: VideoCodec::H264,
                width: 1920,
                height: 1080,
                keyframe,
                pts_us: self.frames * 16_667,
                ..Default::default()
            },
            bytes: vec![1; UNIT_BYTES],
        }))
    }

    fn cursor(&mut self, _: &DesktopContext) -> Result<Option<CursorPosition>> {
        Ok(self.pointer.at().map(|(x, y)| CursorPosition {
            x: x as i32,
            y: y as i32,
            visible: true,
        }))
    }

    fn cursor_shape(&mut self, _: &DesktopContext) -> Result<Option<CursorShape>> {
        let Some((x, _)) = self.pointer.at() else {
            return Ok(None);
        };
        let half = u32::from(x >= 960);
        if self.shown == Some(half) {
            return Ok(None);
        }
        self.shown = Some(half);
        let kind = if half == 0 {
            CursorKind::Arrow
        } else {
            CursorKind::Hand
        };
        CursorShape::new(kind, 8, 8, (0, 0), vec![half as u8 + 1; 8 * 8 * 4]).map(Some)
    }

    fn input(&mut self, input: &Input) -> Result<()> {
        self.inputs.fetch_add(1, Ordering::Relaxed);
        if let (Input::Move { x, y }, Some(_)) = (input, self.pointer.at()) {
            self.pointer.show(*x, *y);
        }
        Ok(())
    }

    fn release(&mut self) {}
}

/// Pull and present every unit on a thread of its own, as a native renderer
/// does, counting them into `presented`.
fn render(
    viewer: &Viewer,
    presented: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let consumer = viewer.video_sink().attach().unwrap();
    std::thread::spawn(move || {
        let mut capacity = 1 << 20;
        while !stop.load(Ordering::Acquire) {
            match consumer.next(Duration::from_millis(50), capacity) {
                Pull::Unit(unit) => {
                    if consumer.presented(unit.generation, unit.frame_id) {
                        presented.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Pull::NeedCapacity(needed) => capacity = needed,
                Pull::Timeout | Pull::Paused => {}
                Pull::Closed => break,
            }
        }
    })
}

/// What one window counted.
struct Tally {
    allocations: u64,
    allocated: u64,
    presented: u64,
    inputs: u64,
    at: Instant,
}

impl Tally {
    fn now(presented: &AtomicU64, inputs: &AtomicU64) -> Self {
        Self {
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            allocated: ALLOCATED.load(Ordering::Relaxed),
            presented: presented.load(Ordering::Relaxed),
            inputs: inputs.load(Ordering::Relaxed),
            at: Instant::now(),
        }
    }

    fn report(&self, case: &str, end: &Self) {
        let units = end.presented - self.presented;
        let inputs = end.inputs - self.inputs;
        let allocations = end.allocations - self.allocations;
        let allocated = end.allocated - self.allocated;
        let seconds = end.at.duration_since(self.at).as_secs_f64();
        let per_unit = units.max(1) as f64;
        println!(
            "case={case} units_per_s={:.1} inputs_per_s={:.1} allocs_per_s={:.0} allocs_per_unit={:.2} alloc_bytes_per_unit={:.0}",
            units as f64 / seconds,
            inputs as f64 / seconds,
            allocations as f64 / seconds,
            allocations as f64 / per_unit,
            allocated as f64 / per_unit,
        );
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let relay = Relay::start("host-send-bench").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
    let inputs = Arc::new(AtomicU64::new(0));
    let applied = inputs.clone();
    let pointer = Pointer::default();
    let host_pointer = pointer.clone();
    let mut options = HostOptions::new("desktop:bench");
    options.allow_control = true;
    options.allow_direct = false;
    let host = Host::builder(options)
        .backend(move || {
            Ok(Encoded {
                frames: 0,
                inputs: applied.clone(),
                pointer: host_pointer.clone(),
                shown: None,
            })
        })
        .start(&client)
        .await
        .unwrap();
    let viewer = Viewer::connect(
        &client,
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
    .unwrap();
    let presented = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let renderer = render(&viewer, presented.clone(), stop.clone());
    viewer.request_control(true).unwrap();
    let started = Instant::now();
    while !(viewer.status().controlling && presented.load(Ordering::Relaxed) > 0) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no picture under control"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Past the first probes and a full delivery window, so the credit window
    // and the pace are in their steady state.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let start = Tally::now(&presented, &inputs);
    tokio::time::sleep(WINDOW).await;
    let end = Tally::now(&presented, &inputs);
    start.report("video", &end);

    let start = Tally::now(&presented, &inputs);
    let mut tick = tokio::time::interval(POINTER_EVERY);
    let mut x = 0;
    while start.at.elapsed() < WINDOW {
        tick.tick().await;
        x = (x + 7) % 1920;
        let view = viewer.view();
        // A move that finds the picture not yet fresh is dropped, as a
        // user's would be.
        let _ = viewer.input(view.generation, &[Input::Move { x, y: 540 }]);
    }
    let end = Tally::now(&presented, &inputs);
    start.report("video+pointer", &end);

    let viewer2 = Viewer::connect(
        &client,
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
    .unwrap();
    let presented2 = Arc::new(AtomicU64::new(0));
    let renderer2 = render(&viewer2, presented2.clone(), stop.clone());
    viewer2.request_control(true).unwrap();
    let started = Instant::now();
    while !(viewer2.status().controlling && presented2.load(Ordering::Relaxed) > 0) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "second viewer got no picture under control"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    let start = Tally::now(&presented, &inputs);
    let mut tick = tokio::time::interval(POINTER_EVERY);
    let mut x = 0;
    while start.at.elapsed() < WINDOW {
        tick.tick().await;
        x = (x + 7) % 1920;
        let v1 = viewer.view();
        let _ = viewer.input(v1.generation, &[Input::Move { x, y: 360 }]);
        let v2 = viewer2.view();
        let _ = viewer2.input(
            v2.generation,
            &[Input::Move {
                x: 1919 - x,
                y: 720,
            }],
        );
    }
    let end = Tally::now(&presented, &inputs);
    start.report("2viewers+pointer", &end);

    // One distinct move at a time. The relay, host and observer run on the
    // runtime workers while this benchmark thread samples the public view.
    // Timing includes the polling interval and excludes UI vsync and display.
    let mut latencies = Vec::with_capacity(256);
    for x in 0..256 {
        let generation = viewer.view().generation;
        let start = Instant::now();
        viewer
            .input(generation, &[Input::Move { x, y: 123 }])
            .unwrap();
        loop {
            let peer = viewer2.view();
            if peer.peer_count == 1 && peer.peer_x[0] == x as i32 && peer.peer_y[0] == 123 {
                latencies.push(start.elapsed().as_micros());
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "peer pointer timed out"
            );
            std::thread::sleep(Duration::from_micros(100));
        }
    }
    latencies.sort_unstable();
    println!(
        "case=peer_cursor samples={} p50_us={} p95_us={} p99_us={}",
        latencies.len(),
        latencies[127],
        latencies[243],
        latencies[253]
    );

    // The pointer shows from here, on the left, an arrow; the first viewer's
    // move into the other half gives it the other shape.
    pointer.show(480, 540);
    let shape_of = |viewer: &Viewer| viewer.view().cursor_shape;
    let started = Instant::now();
    while shape_of(&viewer) == 0 {
        assert!(started.elapsed() < Duration::from_secs(3), "no shape");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let mut latencies = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        tokio::time::sleep(spread(sample)).await;
        let x = if sample % 2 == 0 { 1440 } else { 480 };
        let before = shape_of(&viewer);
        let generation = viewer.view().generation;
        let start = Instant::now();
        viewer
            .input(generation, &[Input::Move { x, y: 540 }])
            .unwrap();
        while shape_of(&viewer) == before {
            assert!(start.elapsed() < Duration::from_secs(3), "shape timed out");
            std::thread::sleep(Duration::from_micros(100));
        }
        latencies.push(start.elapsed().as_micros());
    }
    report_latencies("shape", "", &mut latencies);

    stop.store(true, Ordering::Release);
    viewer2.stop().await.unwrap();
    viewer.stop().await.unwrap();
    renderer2.join().unwrap();
    renderer.join().unwrap();

    // Someone at the host moves its pointer; a viewer that only watches, the
    // room's only one, sees it at the host's pointer cadence for its pace.
    for fps in [60, 120] {
        let watcher = Viewer::connect(
            &client,
            host.invitation(),
            ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..Default::default()
            },
            ViewerConfig {
                codecs: vec![VideoCodec::H264],
                limits: VideoLimits::for_viewer(f64::from(fps)),
                fps_override: true,
                quality: QualityPreset::Balanced,
                progress: None,
            },
        )
        .await
        .unwrap();
        let shown = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let renderer = render(&watcher, shown.clone(), stop.clone());
        let started = Instant::now();
        while shown.load(Ordering::Relaxed) == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the watcher got no picture"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let start = Tally::now(&shown, &inputs);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let end = Tally::now(&shown, &inputs);
        let units_per_s = (end.presented - start.presented) as f64
            / end.at.duration_since(start.at).as_secs_f64();
        let mut latencies = Vec::with_capacity(SAMPLES);
        for sample in 0..SAMPLES {
            tokio::time::sleep(spread(sample)).await;
            let x = 100 + (sample as u32 * 13) % 1700;
            let start = Instant::now();
            pointer.show(x, 300);
            loop {
                let view = watcher.view();
                if view.flags & DesktopView::FLAG_CURSOR_VISIBLE != 0
                    && view.cursor_x == x as i32
                    && view.cursor_y == 300
                {
                    break;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(3),
                    "host pointer timed out"
                );
                std::thread::sleep(Duration::from_micros(100));
            }
            latencies.push(start.elapsed().as_micros());
        }
        report_latencies(
            "host_pointer",
            &format!(" pace={fps} units_per_s={units_per_s:.1}"),
            &mut latencies,
        );
        stop.store(true, Ordering::Release);
        watcher.stop().await.unwrap();
        renderer.join().unwrap();
    }
    host.stop().await.unwrap();
}

/// Samples in each pointer case.
const SAMPLES: usize = 128;

/// The wait before `sample`: 20 to 60 ms, spread over the host's pointer
/// cadence.
fn spread(sample: usize) -> Duration {
    Duration::from_millis(20 + (sample as u64 * 7) % 41)
}

fn report_latencies(case: &str, extra: &str, latencies: &mut [u128]) {
    latencies.sort_unstable();
    let at = |q: f64| latencies[((latencies.len() - 1) as f64 * q).round() as usize];
    println!(
        "case={case}{extra} samples={} p50_us={} p90_us={} p99_us={} max_us={}",
        latencies.len(),
        at(0.5),
        at(0.9),
        at(0.99),
        at(1.0),
    );
}
