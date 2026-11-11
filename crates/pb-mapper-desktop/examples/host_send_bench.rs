//! Heap allocations, and the bytes they ask for, per video unit on the remote
//! desktop's paths, end to end in one process: a relay, a host whose H.264
//! "encoder" returns one 4 KiB unit per capture, and a viewer whose renderer
//! presents every unit at once. The first window streams video only; the
//! second also moves the pointer every 4 ms under control, which feeds the
//! host's input lane and its input wait window. Allocations are counted for
//! the whole process, so the viewer's and the relay's are in every figure:
//! compare runs of this harness with each other only. A reallocation counts
//! as one allocation of its new size.
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
    Backend, DesktopContext, Display, EncodedFrame, Host, HostOptions, Input, Pull, QualityPreset,
    Result, VideoCodec, VideoFormat, VideoLimits, VideoRate, Viewer, ViewerConfig,
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

/// An H.264 desktop: every capture is one opaque unit, an IDR when asked.
struct Encoded {
    frames: u64,
    inputs: Arc<AtomicU64>,
}

impl Backend for Encoded {
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

    fn input(&mut self, _: &Input) -> Result<()> {
        self.inputs.fetch_add(1, Ordering::Relaxed);
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
    let mut options = HostOptions::new("desktop:bench");
    options.allow_control = true;
    options.allow_direct = false;
    let host = Host::builder(options)
        .backend(move || {
            Ok(Encoded {
                frames: 0,
                inputs: applied.clone(),
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

    stop.store(true, Ordering::Release);
    viewer.stop().await.unwrap();
    renderer.join().unwrap();
    host.stop().await.unwrap();
}
