//! Capture and encode benchmark: `video_bench [seconds] [fps] [flags]`. Run it
//! with a visibly changing desktop, or with `--animate`. Static repeats do not
//! count as capture FPS. It measures capture and encode only, never network or
//! viewer presentation, and paces its submissions as the host's capture worker
//! does: the encoder never paces itself, and a tick that finds nothing new
//! stays armed and is asked again.
//!
//! - `--pattern-only`: animate the pattern window; no encoder runs here.
//! - `--animate`: animate the pattern window and encode it.
//! - `--backend`: drive the SDK backend instead of the encoder.
//! - `--grid`: pace on the even grid, as thread C did before it anchored
//!   ticks on presents (each consume undated), while still measuring the
//!   wait from each dated present: the baseline arm of a pacing comparison.
//! - `--pace 60,30,15,5`: encode `seconds` at each pace, at the nominal `fps`,
//!   and classify the encoder's rate model against the produced bitrate. It
//!   animates the pattern. The budget per frame (bitrate / nominal fps) must
//!   lie between the pattern's floor and its demand, about 50 and 90 kbit, or
//!   the content rather than the encoder decides the bitrate: the defaults
//!   here are 4 Mbit/s at a nominal 60 fps.
//! - `--dump <path>`: write `u32 BE length | access unit` records, the clip the
//!   synthetic test source plays. The pattern covers the whole display, so the
//!   clip holds pattern frames only, never desktop pixels.
//! - `--frames <n>`: stop after `n` access units.
//! - `--codec h264|h265`: the encoder; H.264 by default.
//! - `--exact`: encode exactly `--max-width` × `--max-height`, upscaling a
//!   smaller display, to measure a resolution this display does not have.
//! - `--noise`: cover the whole display with fresh noise every refresh, the
//!   worst case for an access unit's size. It is encoded with the desktop.
//! - `--bitrate`, `--max-width`, `--max-height`, `--output <raw Annex B>`,
//!   `--pause-ms` (encoder recovery probe), `--poll-us` (backend polling).
//! - `--poll-events`: build the encoder without its event watcher, so that
//!   its events are polled and it is drained every `DRAIN_POLL` while it
//!   holds units, as thread C drained every encoder before the watcher and
//!   still drains one that cannot be shut down: the baseline arm of an
//!   event comparison. Not with `--backend`.
//! - `--text`: cover the whole display with a still page of GDI text in
//!   several fonts, sizes and colours, painted once (and again only when
//!   Windows asks the window to repaint): a desktop of text, the content
//!   whose sharpness at a given bitrate the encoder settings below decide.
//!   After its first picture the encoder sees only the 250 ms repeats.
//! - `--h264-profile base|high`: the H.264 profile asked for first (High by
//!   default, as a session: Constrained High, High, then Baseline where the
//!   encoder refuses them).
//! - `--qvs <0..100>`: `CODECAPI_AVEncCommonQualityVsSpeed` (0 by default,
//!   as a session).
//! - `--scenario display-remoting`: set `CODECAPI_AVScenarioInfo` to
//!   display remoting, which a session does not.
//! - `--gpu-priority`: raise the capture device's GPU thread priority and
//!   the process's GPU scheduling class to `HIGH` while the encoder lives,
//!   which a session does not. Run it beside a GPU-saturating 3D load.
//! - `--dump-source <path>`: write the NV12 picture each submit gave the
//!   encoder, read back from the GPU (`height` luma rows, then `height / 2`
//!   rows of interleaved chroma, `width` bytes each, the size the
//!   `encoder=` line's `max_size` and the first unit's `size` show), one
//!   per submitted unit, so that frame n of `--output` decodes to frame n
//!   of this file while `encoded` equals `submitted`: the reference for
//!   ffmpeg's PSNR and SSIM. Reading a picture back waits for the GPU, so
//!   time the encoder in a run without it. Not with `--backend`.
//!
//! The encoder's read-back of its settings is printed as an
//! `encoder_opened` line (`desktop_video_encoder_opened`'s fields), and each
//! unit's dwell in the encoder, submit to output, as `dwell_p50_ms` and
//! `dwell_p95_ms`; driving the encoder, so is the time from each dated
//! present to the drain that collected its unit (`present_to_output_*`,
//! which pairs units in order, so a run that discards a late unit, as
//! `--pause-ms` does, pairs the later ones wrongly).
//!
//! The loop waits as thread C does: until the next tick, the next poll of an
//! armed tick or of a polled encoder holding units (or of one with an
//! announced output still uncollected), or a wake from the encoder's event
//! watcher (each output, and the input request a waiting submit waits for),
//! and at most 100 ms.
//!
//! It prints the largest keyframe and access unit, which `wire::MAX_PAYLOAD`
//! must hold. Driving the encoder, it also prints a `case=encoder` line: the
//! heap allocations and bytes the encoder's own calls ask for, and the time
//! and the cycles its thread ran in them, per access unit; and the median and
//! mean of each of a submit that converted a capture and of a drain that
//! returned a unit; and an `encoder_events` line: whether its events were
//! pushed or polled, the submits that found no input request (`waiting`, as
//! `Submit::Waiting`) and, pushed, how long each output waited from its
//! announcement to the start of the drain that collected it
//! (`have_output_to_collect_us`, counts per bucket, and the bucket that
//! holds the median). Either way it prints the capture calls per second,
//! and the polls among them: the calls that left the tick armed, having
//! found nothing new or, as `waiting` counts apart, an encoder that had not
//! asked for input yet; how often the loop woke (`loop_wakes_per_s`, thread
//! C's wakes) and how many of those an encoder event ended; and, with
//! thread C's own pacer anchoring each tick on the present it took (unless
//! `--grid`), the wait from a present to the submit that took it (p50 and
//! p95), the share of submits whose picture was dated, and the presents no
//! submitted picture showed (`coalesced_presents`: for each dated picture,
//! the presents it covers beyond one, as thread C's `coalesced_per_s`
//! counts them). It also prints how long the first access unit took from
//! the start of the paced loop, and, driving the encoder, what the
//! duplication's first frame reported (`first_acquire_*`).
#[cfg(windows)]
mod probe_support;
// The `--pace` classifier, shared with the macOS encoder's measurement test.
#[cfg(windows)]
#[path = "../src/codec/rate_fit.rs"]
mod rate_fit;
// Thread C's capture pacer itself.
#[cfg(windows)]
#[path = "../src/pacer.rs"]
mod pacer;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    bench::run()
}
#[cfg(not(windows))]
fn main() {}

/// The system allocator, counting each allocation and reallocation and the
/// bytes each asks for.
#[cfg(windows)]
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};

    pub struct Counting;

    static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static ALLOCATED: AtomicU64 = AtomicU64::new(0);

    fn count(bytes: usize) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Allocations and bytes asked for so far, process-wide.
    pub fn now() -> (u64, u64) {
        (
            ALLOCATIONS.load(Ordering::Relaxed),
            ALLOCATED.load(Ordering::Relaxed),
        )
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
}

/// The benchmark: its options, the paced loop and the report.
#[cfg(windows)]
mod bench {
    use crate::{
        pacer::Pacer,
        pattern::{Fill, Pattern},
        probe_support::option,
    };
    use pb_mapper_desktop::{
        Backend, DRAIN_POLL, Display, DisplaySelection, NativeBackend, VideoCodec, VideoLimits,
        VideoRate, displays, native_backend,
        windows::video::{
            Encoded, Encoder, EncoderOptions, EncoderWake, H264Profile, PICKUP_BOUNDS_US,
            Statistics, Submit, h265_available,
        },
    };
    use std::{
        fs::File,
        io::{BufWriter, Write},
        time::{Duration, Instant},
    };

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    /// The options that take a value, which is then no positional argument.
    const VALUED: [&str; 14] = [
        "--max-width",
        "--max-height",
        "--bitrate",
        "--poll-us",
        "--output",
        "--pause-ms",
        "--pace",
        "--dump",
        "--frames",
        "--codec",
        "--h264-profile",
        "--qvs",
        "--scenario",
        "--dump-source",
    ];
    /// A step's bitrate is measured after this long, once the rate controller
    /// has settled.
    const SETTLE: Duration = Duration::from_secs(2);
    /// Thread C's `HOT_WINDOW`: after a new picture, a tick that found
    /// nothing new is asked again every `DRAIN_POLL` for this long, and once
    /// a period after it.
    const HOT_WINDOW: Duration = Duration::from_millis(250);
    /// Thread C's `CONTEXT_CHECK_INTERVAL`: it wakes at least this often.
    const CONTEXT_CHECK_INTERVAL: Duration = Duration::from_millis(100);

    pub fn run() -> Result {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let rates = Rates::parse(&args)?;
        let display = displays()?.into_iter().next().ok_or("no display")?;
        let pattern_only = flag(&args, "--pattern-only");
        let noise = flag(&args, "--noise");
        let text = flag(&args, "--text");
        if text && (noise || option(&args, "--dump").is_some()) {
            return Err(
                "--text covers the display with a still page: not with --noise or --dump".into(),
            );
        }
        let _animation = if option(&args, "--dump").is_some() || noise || text {
            Some(Pattern::new(
                display.x,
                display.y,
                display.width,
                display.height,
                if noise {
                    Fill::Noise
                } else if text {
                    Fill::Text
                } else {
                    Fill::Colour
                },
            )?)
        } else if pattern_only || flag(&args, "--animate") || rates.paces.is_some() {
            Some(Pattern::new(
                display.x + 32,
                display.y + 32,
                960,
                540,
                Fill::Colour,
            )?)
        } else {
            None
        };
        if pattern_only {
            println!(
                "Animating benchmark window for {}s; no encoder is running in this process",
                rates.seconds
            );
            std::thread::sleep(Duration::from_secs(rates.seconds));
            return Ok(());
        }
        let (limits, mut sinks) = Limits::parse(&args, &rates, &display)?;
        let (codec, fps) = (rates.codec, rates.fps);
        let Limits {
            max_width,
            max_height,
            bitrate,
            exact,
            ..
        } = limits;
        if codec == VideoCodec::H265 {
            // What a host asks before it offers H.265: probed once per adapter.
            for _ in 0..2 {
                let asked = Instant::now();
                let usable = h265_available(display.id);
                println!(
                    "h265_available={usable} probe_ms={:.1}",
                    asked.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
        // What the encoder's event watcher wakes, and the loop waits on.
        let wake = EncoderWake::new()?;
        let waker = (!limits.poll_events).then_some(wake.waker());
        let options = limits.options;
        let building = Instant::now();
        let mut encoder = if limits.backend {
            None
        } else if exact {
            Some(Encoder::new_exact(
                &display, codec, fps, bitrate, max_width, max_height, waker, options,
            )?)
        } else {
            Some(Encoder::new_with_size(
                &display, codec, fps, bitrate, max_width, max_height, waker, options,
            )?)
        };
        let build_ms = building.elapsed().as_secs_f64() * 1000.0;
        if let Some(encoder) = &mut encoder
            && sinks.source.is_some()
        {
            encoder.keep_source();
        }
        let mut coder = match encoder {
            Some(encoder) => Coder::Encoder(encoder),
            None => {
                let mut backend = native_backend(&DisplaySelection::Id(display.id))?;
                backend.bind(wake.waker().clone());
                backend.configure_video(
                    VideoLimits {
                        max_width,
                        max_height,
                        max_fps: fps,
                    },
                    fps,
                )?;
                Coder::Backend(backend)
            }
        };
        println!(
            "encoder={} codec={codec:?} build_ms={build_ms:.1} display={}x{} nominal_fps={fps} max_size={max_width}x{max_height}{} target_bitrate={bitrate} h264_profile={:?} qvs={} scenario={} gpu_priority={}",
            coder.name(),
            display.width,
            display.height,
            if exact { " exact" } else { "" },
            options.h264_profile,
            options.quality_vs_speed,
            if options.display_remoting {
                "display-remoting"
            } else {
                "none"
            },
            options.gpu_priority,
        );

        let steps = rates.steps();
        let mut tally = Tally {
            measured: vec![0; steps.len()],
            ..Tally::default()
        };
        let start = Instant::now();
        let run = Run {
            rates: &rates,
            limits: &limits,
            steps: &steps,
            wake: &wake,
            start,
        };
        paced(&run, &mut coder, &mut sinks, &mut tally)?;
        if let Some(file) = &mut sinks.dump {
            file.flush()?;
            println!("dumped_units={}", tally.written);
        }
        if let Some(file) = &mut sinks.source {
            file.flush()?;
            println!("source_pictures={}", tally.sources);
        }
        let stats = match &coder {
            Coder::Encoder(encoder) => encoder.statistics(),
            Coder::Backend(_) => tally.backend_stats.clone(),
        };
        if tally.pending_idr.is_some() {
            return Err("hardware encoder did not honor requested keyframe".into());
        }
        if limits.pause_ms != 0 && (!tally.paused || tally.awaiting_recovery) {
            return Err("pause probe did not observe independent recovery".into());
        }
        // 0: the encoder reports none. A finite GOP under 5 s fails.
        if stats.gop != 0 && u64::from(stats.gop) < 5 * u64::from(fps) {
            return Err(format!("encoder GOP is {} frames, under 5 s", stats.gop).into());
        }
        let elapsed = start.elapsed().as_secs_f64();
        report(&tally, &stats, &rates, &limits, elapsed)
    }

    /// The cycles this thread has run, counted only while it runs, so a
    /// preemption does not count.
    fn thread_cycles() -> u64 {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentThread() -> *mut std::ffi::c_void;
            fn QueryThreadCycleTime(thread: *mut std::ffi::c_void, cycles: *mut u64) -> i32;
        }
        let mut cycles = 0;
        // SAFETY: the current thread's pseudo-handle is always valid, and
        // `cycles` outlives the call.
        unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
        cycles
    }

    fn median(samples: &[f64]) -> f64 {
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted.get(sorted.len() / 2).copied().unwrap_or(0.0)
    }

    /// The `percent` quantile of `samples`, 0 without any.
    fn quantile(samples: &[f64], percent: usize) -> f64 {
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = (sorted.len() * percent / 100).min(sorted.len().saturating_sub(1));
        sorted.get(at).copied().unwrap_or(0.0)
    }

    fn mean(samples: &[f64]) -> f64 {
        samples.iter().sum::<f64>() / samples.len().max(1) as f64
    }

    /// The pickup histogram as `le100:n,le250:n,…,gt8000:n`.
    fn pickup_buckets(counts: &[u64]) -> String {
        let last = PICKUP_BOUNDS_US.last().copied().unwrap_or(0);
        counts
            .iter()
            .enumerate()
            .map(|(bucket, count)| match PICKUP_BOUNDS_US.get(bucket) {
                Some(bound) => format!("le{bound}:{count}"),
                None => format!("gt{last}:{count}"),
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The bucket that holds the median pickup, as its bound (`le…`), or
    /// `none` without one.
    fn pickup_median(counts: &[u64]) -> String {
        let total: u64 = counts.iter().sum();
        let mut seen = 0;
        for (bucket, count) in counts.iter().enumerate() {
            seen += count;
            if total > 0 && seen * 2 >= total {
                return match PICKUP_BOUNDS_US.get(bucket) {
                    Some(bound) => format!("le{bound}"),
                    None => format!("gt{}", PICKUP_BOUNDS_US.last().copied().unwrap_or(0)),
                };
            }
        }
        "none".into()
    }

    fn flag(args: &[String], name: &str) -> bool {
        args.iter().any(|arg| arg == name)
    }

    /// `value`, or `none` for a setting the encoder read nothing back for.
    fn or_none<T: std::fmt::Display>(value: Option<T>) -> String {
        value.map_or_else(|| "none".to_owned(), |value| value.to_string())
    }

    /// What the run asks for that needs no display: how long, at which paces
    /// and nominal rate, in which codec.
    struct Rates {
        seconds: u64,
        paces: Option<Vec<u32>>,
        fps: u32,
        codec: VideoCodec,
    }
    impl Rates {
        fn parse(args: &[String]) -> Result<Self> {
            let mut positional = Vec::new();
            let mut operand = false;
            for arg in args {
                if std::mem::take(&mut operand) {
                    continue;
                }
                operand = VALUED.contains(&arg.as_str());
                if !arg.starts_with("--") {
                    positional.push(arg.as_str());
                }
            }
            let seconds: u64 = positional.first().copied().unwrap_or("10").parse()?;
            if !(1..=300).contains(&seconds) {
                return Err("benchmark duration must be 1..300 seconds".into());
            }
            let paces: Option<Vec<u32>> = option(args, "--pace")
                .map(|list| list.split(',').map(str::parse).collect())
                .transpose()?;
            let fps: u32 = positional
                .get(1)
                .copied()
                .unwrap_or(if paces.is_some() { "60" } else { "120" })
                .parse()?;
            if !(1..=120).contains(&fps) {
                return Err("nominal fps must be 1..120".into());
            }
            if let Some(paces) = &paces
                && (seconds < 3
                    || paces.is_empty()
                    || paces.iter().any(|pace| !(1..=fps).contains(pace)))
            {
                return Err("--pace needs 3 s or more per step, and paces of 1..fps".into());
            }
            let codec = match option(args, "--codec").unwrap_or("h264") {
                "h264" => VideoCodec::H264,
                "h265" => VideoCodec::H265,
                other => return Err(format!("unknown codec {other}: h264 or h265").into()),
            };
            Ok(Self {
                seconds,
                paces,
                fps,
                codec,
            })
        }

        /// Each step's pace: the `--pace` list, else the nominal rate alone.
        fn steps(&self) -> Vec<u32> {
            self.paces.clone().unwrap_or_else(|| vec![self.fps])
        }
    }

    /// What the run asks of the encoder or backend, read once the display is
    /// known.
    struct Limits {
        /// Drive the SDK backend instead of the encoder.
        backend: bool,
        /// Pace on the even grid, never anchored on a present.
        grid: bool,
        /// Build the encoder without its event watcher: poll its events.
        poll_events: bool,
        max_width: u32,
        max_height: u32,
        bitrate: u32,
        frames: Option<u64>,
        poll: Duration,
        pause_ms: u64,
        exact: bool,
        /// The encoder's profile, quality against speed, scenario and GPU
        /// priority.
        options: EncoderOptions,
    }
    impl Limits {
        /// The limits, and the files they name, created as they are read.
        fn parse(args: &[String], rates: &Rates, display: &Display) -> Result<(Self, Sinks)> {
            let backend = flag(args, "--backend");
            let grid = flag(args, "--grid");
            let poll_events = flag(args, "--poll-events");
            if poll_events && backend {
                return Err("--poll-events builds the encoder; a backend's events wake it".into());
            }
            let max_width = option(args, "--max-width")
                .map(str::parse)
                .transpose()?
                .unwrap_or(display.width);
            let max_height = option(args, "--max-height")
                .map(str::parse)
                .transpose()?
                .unwrap_or(display.height);
            let bitrate: u32 = option(args, "--bitrate")
                .map(str::parse)
                .transpose()?
                .unwrap_or(if rates.paces.is_some() {
                    4_000_000
                } else {
                    24_000_000
                });
            let frames: Option<u64> = option(args, "--frames").map(str::parse).transpose()?;
            let poll = Duration::from_micros(
                option(args, "--poll-us")
                    .map(str::parse)
                    .transpose()?
                    .unwrap_or(8_333),
            );
            if !(Duration::from_micros(500)..=Duration::from_millis(100)).contains(&poll) {
                return Err("poll interval must be 500..100000 microseconds".into());
            }
            let pause_ms: u64 = option(args, "--pause-ms")
                .map(str::parse)
                .transpose()?
                .unwrap_or(0);
            if pause_ms != 0 && (!(300..=1000).contains(&pause_ms) || backend) {
                return Err("pause probe requires the encoder mode and 300..1000 ms".into());
            }
            let output = option(args, "--output").map(File::create).transpose()?;
            let dump = option(args, "--dump")
                .map(File::create)
                .transpose()?
                .map(BufWriter::new);
            let exact = flag(args, "--exact");
            if exact && backend {
                return Err("--exact drives the encoder; a backend never upscales".into());
            }
            let options = EncoderOptions {
                h264_profile: match option(args, "--h264-profile").unwrap_or("high") {
                    "high" => H264Profile::High,
                    "base" => H264Profile::Base,
                    other => {
                        return Err(format!("unknown H.264 profile {other}: base or high").into());
                    }
                },
                quality_vs_speed: option(args, "--qvs")
                    .map(str::parse)
                    .transpose()?
                    .unwrap_or(0),
                display_remoting: match option(args, "--scenario") {
                    None => false,
                    Some("display-remoting") => true,
                    Some(other) => {
                        return Err(format!("unknown scenario {other}: display-remoting").into());
                    }
                },
                gpu_priority: flag(args, "--gpu-priority"),
            };
            if options.quality_vs_speed > 100 {
                return Err("--qvs must be 0..100".into());
            }
            let changes_encoder = option(args, "--h264-profile").is_some()
                || option(args, "--qvs").is_some()
                || options.display_remoting
                || options.gpu_priority;
            if backend && changes_encoder {
                return Err(
                    "--h264-profile, --qvs, --scenario and --gpu-priority build the encoder; a backend takes a session's settings"
                        .into(),
                );
            }
            let source = option(args, "--dump-source")
                .map(File::create)
                .transpose()?
                .map(BufWriter::new);
            if source.is_some() && backend {
                return Err(
                    "--dump-source reads the encoder's input back; not with --backend".into(),
                );
            }
            let limits = Self {
                backend,
                grid,
                poll_events,
                max_width,
                max_height,
                bitrate,
                frames,
                poll,
                pause_ms,
                exact,
                options,
            };
            Ok((
                limits,
                Sinks {
                    output,
                    dump,
                    source,
                },
            ))
        }
    }

    /// The files the access units are written to: raw Annex B, and the
    /// length-prefixed clip; and the NV12 pictures the encoder was given.
    struct Sinks {
        output: Option<File>,
        dump: Option<BufWriter<File>>,
        source: Option<BufWriter<File>>,
    }

    /// What makes the access units: the encoder alone, or the whole SDK
    /// backend around it.
    #[expect(
        clippy::large_enum_variant,
        reason = "one per run, built once and held in place"
    )]
    enum Coder {
        Encoder(Encoder),
        Backend(NativeBackend),
    }
    impl Coder {
        fn name(&self) -> &str {
            match self {
                Self::Encoder(encoder) => encoder.name(),
                Self::Backend(_) => "full SDK backend",
            }
        }

        /// The encoder holds a submitted capture it has not output yet.
        fn in_flight(&self) -> bool {
            match self {
                Self::Encoder(encoder) => {
                    let stats = encoder.statistics();
                    stats.submitted > stats.encoded
                }
                Self::Backend(_) => false,
            }
        }

        /// Units submitted and not yet collected.
        fn pending(&self) -> usize {
            match self {
                Self::Encoder(encoder) => encoder.pending_len(),
                Self::Backend(backend) => backend.pending_video(),
            }
        }

        /// How soon the loop drains again while units are pending, if
        /// nothing wakes it first: `None` while each output wakes it.
        fn drain_poll(&self) -> Option<Duration> {
            match self {
                Self::Encoder(encoder) => encoder.drain_poll(),
                Self::Backend(backend) => backend.drain_poll(),
            }
        }

        /// When an armed tick that waits on the encoder's input request is
        /// asked again, if the request does not wake the loop first.
        fn idle_until(&self) -> Option<Instant> {
            match self {
                Self::Encoder(encoder) => encoder.idle_until(),
                Self::Backend(backend) => backend.capture_idle_until(None),
            }
        }
    }

    /// What the run counted, access unit by access unit.
    #[derive(Default)]
    struct Tally {
        /// The bytes each step produced once it settled.
        measured: Vec<u64>,
        written: u64,
        /// 1 once a keyframe was asked for, at `pending_idr`, until it came.
        forced: u32,
        pending_idr: Option<Instant>,
        keyframes: u32,
        max_keyframe: usize,
        max_unit: usize,
        /// The backend's figures, counted here; an encoder keeps its own.
        backend_stats: Statistics,
        context_us: u128,
        capture_us: u128,
        calls: u64,
        /// Capture calls (a submit, or the backend's `capture_video`), and
        /// those of them that left the tick armed, having found nothing new
        /// or an encoder that had not asked for input yet: the polls a still
        /// or slowly changing desktop, or a serial encoder, costs.
        captures: u64,
        polls: u64,
        /// The loop's waits, as thread C's, and those an encoder event
        /// ended.
        wakes: u64,
        woken: u64,
        /// Submits that spent their tick, and from each dated picture's
        /// present to the submit that took it, in milliseconds.
        submits: u64,
        present_waits: Vec<f64>,
        /// The presents the dated pictures covered beyond one each.
        coalesced: u64,
        /// From the start of the paced loop to the first access unit.
        first_unit: Option<Duration>,
        /// What the encoder's own calls allocated, and bytes asked for.
        encoder_allocations: (u64, u64),
        /// Every encoder call, an empty drain or a submit that took nothing
        /// included: microseconds, and thousands of cycles this thread ran.
        encoder_us: f64,
        encoder_kcycles: f64,
        /// Each submit that converted a capture, and each drain that returned
        /// a unit: microseconds, and thousands of cycles this thread ran.
        submit_us: Vec<f64>,
        drain_us: Vec<f64>,
        submit_kcycles: Vec<f64>,
        drain_kcycles: Vec<f64>,
        /// `--dump-source`'s NV12 pictures written, and the buffer each is
        /// read back into.
        sources: u64,
        source: Vec<u8>,
        /// Each unit's dwell in the encoder, submit to output, in
        /// milliseconds (`Encoded::encode_us`).
        dwell_ms: Vec<f64>,
        /// When the display presented each unit in the encoder, in order
        /// (`None`: undated), and from each dated present to the drain that
        /// collected its unit, in milliseconds. Driving the encoder only.
        in_encoder: std::collections::VecDeque<Option<Instant>>,
        present_to_output: Vec<f64>,
        /// The pause probe stalled the encoder, and its next unit must be a
        /// keyframe.
        paused: bool,
        awaiting_recovery: bool,
        first_reported: bool,
    }
    impl Tally {
        /// Count what an encoder call allocated since `before`, and the
        /// time and cycles it took.
        fn encoder_calls(&mut self, before: (u64, u64), took: Duration, ran: u64) {
            let after = crate::counting::now();
            self.encoder_allocations.0 += after.0 - before.0;
            self.encoder_allocations.1 += after.1 - before.1;
            self.encoder_us += took.as_secs_f64() * 1e6;
            self.encoder_kcycles += ran as f64 / 1e3;
        }

        /// One access unit of `step`: the recovery check, the statistics, the
        /// files, and the first unit's line.
        fn record(
            &mut self,
            frame: &Encoded,
            step: usize,
            step_start: Instant,
            sinks: &mut Sinks,
            pause_ms: u64,
        ) -> Result {
            if self.awaiting_recovery {
                if !frame.keyframe {
                    return Err("expired GPU output leaked dependent video".into());
                }
                println!("pause_recovery_keyframe=true pause_ms={pause_ms}");
                self.awaiting_recovery = false;
            }
            self.dwell_ms.push(f64::from(frame.encode_us) / 1e3);
            self.backend_stats.encoded += 1;
            self.backend_stats.unique_encoded += u64::from(!frame.duplicate);
            self.backend_stats.bytes += frame.bytes.len() as u64;
            self.backend_stats.gop = frame.gop;
            if step_start.elapsed() >= SETTLE {
                self.measured[step] += frame.bytes.len() as u64;
            }
            self.max_unit = self.max_unit.max(frame.bytes.len());
            if frame.keyframe {
                self.keyframes += 1;
                self.max_keyframe = self.max_keyframe.max(frame.bytes.len());
                if let Some(request) = self.pending_idr.take() {
                    println!(
                        "forced_keyframe_ms={:.3}",
                        request.elapsed().as_secs_f64() * 1000.0
                    );
                }
            }
            if let Some(file) = &mut sinks.output {
                file.write_all(&frame.bytes)?;
            }
            if let Some(file) = &mut sinks.dump {
                file.write_all(&u32::try_from(frame.bytes.len())?.to_be_bytes())?;
                file.write_all(&frame.bytes)?;
            }
            if !self.first_reported {
                println!(
                    "first_access_unit={} keyframe={} size={}x{}",
                    frame.bytes.len(),
                    frame.keyframe,
                    frame.width,
                    frame.height
                );
                self.first_reported = true;
            }
            self.written += 1;
            Ok(())
        }
    }

    /// What the paced loop runs with.
    struct Run<'a> {
        rates: &'a Rates,
        limits: &'a Limits,
        steps: &'a [u32],
        /// What the encoder's event watcher wakes.
        wake: &'a EncoderWake,
        start: Instant,
    }

    /// Each step runs the run's seconds at its pace, as the host's capture
    /// worker paces: a submission on each tick, every ready unit drained in
    /// between, until `--frames` units are out. A tick whose capture found
    /// nothing new stays armed, as thread C keeps it: it is asked again every
    /// `DRAIN_POLL` within `HOT_WINDOW` of the last new picture or while the
    /// encoder holds units, and once a period otherwise. A tick whose submit
    /// waits for the encoder's input request is asked again as the request
    /// wakes the loop, or at the encoder's `idle_until`.
    fn paced(run: &Run<'_>, coder: &mut Coder, sinks: &mut Sinks, tally: &mut Tally) -> Result {
        let Run {
            rates,
            limits,
            steps,
            wake,
            start,
        } = *run;
        let mut current_bitrate = limits.bitrate;
        let mut pacer = Pacer::new(rates.fps, start);
        for (step, &pace) in steps.iter().enumerate() {
            let step_start = Instant::now();
            pacer.set_pace(pace, step_start);
            // An armed tick's next poll, and until when polls come every
            // `DRAIN_POLL`.
            let mut poll_due: Option<Instant> = None;
            let mut hot_until = step_start;
            while step_start.elapsed() < Duration::from_secs(rates.seconds)
                && limits.frames.is_none_or(|frames| tally.written < frames)
            {
                if limits.pause_ms != 0
                    && !tally.paused
                    && start.elapsed() > Duration::from_secs(1)
                    && coder.in_flight()
                {
                    // Deliberately expire an outstanding GPU output, as a long
                    // stall can do. The next exported frame must be independent.
                    std::thread::sleep(Duration::from_millis(limits.pause_ms));
                    tally.paused = true;
                    tally.awaiting_recovery = true;
                }
                if rates.paces.is_none() && start.elapsed() > Duration::from_secs(2) {
                    current_bitrate = limits.bitrate.min(12_000_000);
                }
                let rate = VideoRate {
                    bitrate: current_bitrate,
                    pace_fps: pace,
                };
                let now = Instant::now();
                let tick = pacer.due(now)
                    && poll_due.is_none_or(|due| now >= due)
                    && coder.idle_until().is_none_or(|until| now >= until);
                let force =
                    tick && tally.forced == 0 && start.elapsed() > Duration::from_millis(1500);
                if force {
                    tally.forced = 1;
                    tally.pending_idr = Some(now);
                }
                let mut ready = Vec::new();
                // Whether the tick's capture spent it, and when the display
                // presented the picture it submitted.
                let (spent, presented) = match coder {
                    Coder::Encoder(encoder) => {
                        encoder.set_rate(rate)?;
                        loop {
                            let (before, at, ran) =
                                (crate::counting::now(), Instant::now(), thread_cycles());
                            let frame = encoder.drain()?;
                            let (took, ran) = (at.elapsed(), thread_cycles() - ran);
                            tally.encoder_calls(before, took, ran);
                            let Some(frame) = frame else {
                                break;
                            };
                            tally.drain_us.push(took.as_secs_f64() * 1e6);
                            tally.drain_kcycles.push(ran as f64 / 1e3);
                            // Units come out in the order they went in.
                            if let Some(Some(presented)) = tally.in_encoder.pop_front() {
                                let waited = Instant::now().saturating_duration_since(presented);
                                tally.present_to_output.push(waited.as_secs_f64() * 1e3);
                            }
                            ready.push(frame);
                        }
                        if tick {
                            let (before, at, ran) =
                                (crate::counting::now(), Instant::now(), thread_cycles());
                            let outcome = encoder.submit(force)?;
                            let (took, ran) = (at.elapsed(), thread_cycles() - ran);
                            tally.encoder_calls(before, took, ran);
                            if outcome == Submit::Submitted {
                                tally.submit_us.push(took.as_secs_f64() * 1e6);
                                tally.submit_kcycles.push(ran as f64 / 1e3);
                                tally
                                    .in_encoder
                                    .push_back(encoder.presented().map(|presented| presented.at));
                                // After the timed call: the read-back waits
                                // for the GPU.
                                if let Some(file) = &mut sinks.source
                                    && encoder.source_nv12(&mut tally.source)?
                                {
                                    file.write_all(&tally.source)?;
                                    tally.sources += 1;
                                }
                            }
                            (outcome.spends_tick(), encoder.presented())
                        } else {
                            (false, None)
                        }
                    }
                    Coder::Backend(backend) => {
                        let before = Instant::now();
                        let context = backend.context()?;
                        tally.context_us += before.elapsed().as_micros();
                        let before = Instant::now();
                        let frame = if tick {
                            backend.capture_video(&context, rates.codec, force, rate)?
                        } else {
                            backend.drain_video(&context, Duration::ZERO)?
                        };
                        tally.capture_us += before.elapsed().as_micros();
                        tally.calls += 1;
                        ready.extend(frame.map(|frame| Encoded {
                            codec: frame.format.codec,
                            bytes: frame.bytes,
                            width: frame.format.width,
                            height: frame.format.height,
                            keyframe: frame.format.keyframe,
                            duplicate: frame.format.duplicate,
                            pts_us: frame.format.pts_us,
                            encode_us: frame.format.encode_us,
                            gop: frame.format.gop,
                        }));
                        let spent = tick && backend.submitted_video();
                        (spent, backend.submitted_picture().filter(|_| spent))
                    }
                };
                // As thread C: a new picture this round collected makes the
                // desktop active before an armed tick's next poll is chosen.
                for frame in ready {
                    if !frame.duplicate {
                        hot_until = Instant::now() + HOT_WINDOW;
                    }
                    if tally.first_unit.is_none() {
                        tally.first_unit = Some(start.elapsed());
                    }
                    tally.record(&frame, step, step_start, sinks, limits.pause_ms)?;
                }
                if tick {
                    tally.captures += 1;
                    if spent {
                        tally.submits += 1;
                        if let Some(presented) = presented {
                            let waited = now.saturating_duration_since(presented.at);
                            tally.present_waits.push(waited.as_secs_f64() * 1e3);
                            tally.coalesced += u64::from(presented.presents.saturating_sub(1));
                        }
                        // As thread C: a dated picture anchors the next tick,
                        // unless the grid is the arm measured.
                        pacer.consume(
                            now,
                            presented
                                .filter(|_| !limits.grid)
                                .map(|presented| (presented.at, presented.presents)),
                        );
                        poll_due = None;
                    } else {
                        tally.polls += 1;
                        let done = Instant::now();
                        // A submit that waits for the encoder's input request
                        // names its own retry, and the request wakes the loop.
                        poll_due = coder.idle_until().is_none().then(|| {
                            if done < hot_until || coder.pending() > 0 {
                                done + DRAIN_POLL
                            } else {
                                done + DRAIN_POLL.max(pacer.period())
                            }
                        });
                    }
                }
                // Wait as thread C waits: for the next tick or armed poll, a
                // polled encoder's next drain while it holds units, at most a
                // context check, or the encoder's wake.
                let now = Instant::now();
                let earliest = pacer.earliest();
                let mut next = poll_due.map_or(earliest, |due| due.max(earliest));
                if let Some(until) = coder.idle_until() {
                    next = next.max(until);
                }
                if coder.pending() > 0
                    && let Some(drain) = coder.drain_poll()
                {
                    let drain = if limits.backend { limits.poll } else { drain };
                    next = next.min(now + drain);
                }
                let woken = wake.wait(next.min(now + CONTEXT_CHECK_INTERVAL))?;
                tally.wakes += 1;
                tally.woken += u64::from(woken);
            }
        }
        Ok(())
    }

    /// The run's totals and, with `--pace`, the rate model each step fits.
    fn report(
        tally: &Tally,
        stats: &Statistics,
        rates: &Rates,
        limits: &Limits,
        elapsed: f64,
    ) -> Result {
        let Tally {
            keyframes,
            forced,
            max_keyframe,
            max_unit,
            ..
        } = tally;
        println!("max_keyframe_bytes={max_keyframe} max_access_unit_bytes={max_unit}");
        if !limits.backend {
            let units = stats.encoded.max(1) as f64;
            let (allocations, bytes) = tally.encoder_allocations;
            println!(
                "case=encoder units={} allocs_per_unit={:.3} alloc_bytes_per_unit={:.0} calls_us_per_unit={:.1} calls_kcycles_per_unit={:.1} submits={} submit_p50_us={:.1} submit_mean_us={:.1} drain_p50_us={:.1} drain_mean_us={:.1} submit_p50_kcycles={:.1} submit_mean_kcycles={:.1} drain_p50_kcycles={:.1} drain_mean_kcycles={:.1}",
                stats.encoded,
                allocations as f64 / units,
                bytes as f64 / units,
                tally.encoder_us / units,
                tally.encoder_kcycles / units,
                tally.submit_us.len(),
                median(&tally.submit_us),
                mean(&tally.submit_us),
                median(&tally.drain_us),
                mean(&tally.drain_us),
                median(&tally.submit_kcycles),
                mean(&tally.submit_kcycles),
                median(&tally.drain_kcycles),
                mean(&tally.drain_kcycles),
            );
            println!(
                "encoder_events={} waiting={} waiting_per_s={:.1} have_output_to_collect_us={} have_output_to_collect_p50_us={}",
                if stats.events_pushed {
                    "pushed"
                } else {
                    "polled"
                },
                stats.waiting,
                stats.waiting as f64 / elapsed,
                pickup_buckets(&stats.pickup),
                pickup_median(&stats.pickup),
            );
            let opened = &stats.opened;
            println!(
                "encoder_opened vendor={:#06x} profile={} sps_profile={} sps_reorder={} cabac={} b_frames={} rate_control={} quality_vs_speed={} scenario={}",
                opened.vendor,
                opened.profile,
                or_none(opened.sps_profile),
                or_none(opened.sps_reorder),
                or_none(opened.cabac),
                or_none(opened.b_frames),
                or_none(opened.rate_control),
                or_none(opened.quality_vs_speed),
                or_none(opened.scenario),
            );
        }
        if limits.backend {
            println!(
                "seconds={elapsed:.3} keyframes={keyframes} forced={forced} gop={} encoded={} encoded_fps={:.2} unique_encoded_fps={:.2} bitrate_mbps={:.2} polls={} mean_context_ms={:.3} mean_capture_video_ms={:.3} (includes initial encoder startup)",
                stats.gop,
                stats.encoded,
                stats.encoded as f64 / elapsed,
                stats.unique_encoded as f64 / elapsed,
                stats.bytes as f64 * 8.0 / elapsed / 1_000_000.0,
                tally.calls,
                tally.context_us as f64 / tally.calls.max(1) as f64 / 1000.0,
                tally.capture_us as f64 / tally.calls.max(1) as f64 / 1000.0
            );
        } else {
            println!(
                "seconds={elapsed:.3} keyframes={keyframes} forced={forced} gop={} unique_captures={} unique_capture_fps={:.2} encoded={} encoded_fps={:.2} unique_encoded_fps={:.2} bitrate_mbps={:.2} mean_encode_ms={:.3} max_in_flight={}",
                stats.gop,
                stats.unique_captures,
                stats.unique_captures as f64 / elapsed,
                stats.encoded,
                stats.encoded as f64 / elapsed,
                stats.unique_encoded as f64 / elapsed,
                stats.bytes as f64 * 8.0 / elapsed / 1_000_000.0,
                stats.encode_time_us as f64 / stats.encoded.max(1) as f64 / 1000.0,
                stats.max_in_flight
            );
        }
        println!(
            "capture_calls={} capture_calls_per_s={:.1} polls={} polls_per_s={:.1} loop_wakes={} loop_wakes_per_s={:.1} woken_per_s={:.1}",
            tally.captures,
            tally.captures as f64 / elapsed,
            tally.polls,
            tally.polls as f64 / elapsed,
            tally.wakes,
            tally.wakes as f64 / elapsed,
            tally.woken as f64 / elapsed,
        );
        println!(
            "pacing={} dated_submits={} of {} present_to_submit_p50_ms={:.3} present_to_submit_p95_ms={:.3} coalesced_presents={} coalesced_presents_per_s={:.2}",
            if limits.grid { "grid" } else { "anchored" },
            tally.present_waits.len(),
            tally.submits,
            quantile(&tally.present_waits, 50),
            quantile(&tally.present_waits, 95),
            tally.coalesced,
            tally.coalesced as f64 / elapsed,
        );
        println!(
            "dwell_p50_ms={:.3} dwell_p95_ms={:.3} present_to_output_samples={} present_to_output_p50_ms={:.3} present_to_output_p95_ms={:.3}",
            quantile(&tally.dwell_ms, 50),
            quantile(&tally.dwell_ms, 95),
            tally.present_to_output.len(),
            quantile(&tally.present_to_output, 50),
            quantile(&tally.present_to_output, 95),
        );
        if let Some(first) = tally.first_unit {
            println!("first_access_unit_ms={:.1}", first.as_secs_f64() * 1e3);
        }
        if let Some(first) = stats.first_acquire {
            println!(
                "first_acquire_last_present_time={} first_acquire_accumulated_frames={} first_acquire_resource={}",
                first.last_present_time, first.accumulated_frames, first.resource
            );
        }
        if let Some(paces) = &rates.paces {
            // `Nominal` encoders budget each frame from the nominal rate, so their
            // bitrate falls with the pace; `Timestamp` encoders hold the target.
            let window = (Duration::from_secs(rates.seconds) - SETTLE).as_secs_f64();
            let sweep: Vec<(u32, f64)> = paces
                .iter()
                .zip(&tally.measured)
                .map(|(&pace, &bytes)| (pace, bytes as f64 * 8.0 / window))
                .collect();
            let fit = crate::rate_fit::Fit::new(limits.bitrate, rates.fps, &sweep);
            for step in &fit.steps {
                println!(
                    "pace={} produced_mbps={:.3} nominal_expected_mbps={:.3} nominal_ratio={:.3} timestamp_ratio={:.3}",
                    step.pace,
                    step.produced_bps / 1e6,
                    step.nominal_bps / 1e6,
                    step.nominal,
                    step.timestamp
                );
            }
            println!(
                "rate_model={} within_25_percent={} gop={}",
                fit.model(),
                fit.within,
                stats.gop
            );
            if !fit.within {
                return Err("produced bitrate is not within 25% of the rate model".into());
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
mod pattern {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread::JoinHandle,
    };
    use windows::{
        Win32::{
            Foundation::{COLORREF, RECT},
            Graphics::{Dwm::DwmFlush, Gdi::*},
            UI::{
                HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
                WindowsAndMessaging::*,
            },
        },
        core::{PCWSTR, w},
    };

    /// What the pattern window shows.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Fill {
        /// A colour that changes, and a counter, every display refresh.
        Colour,
        /// Fresh noise every display refresh.
        Noise,
        /// A still page of text, painted once and again only when Windows
        /// asks the window to repaint.
        Text,
    }

    /// A topmost window at physical desktop coordinates: a changing colour
    /// or fresh noise every display refresh, or a still page of text.
    pub struct Pattern {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }
    impl Pattern {
        pub fn new(
            x: i32,
            y: i32,
            width: u32,
            height: u32,
            fill: Fill,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            let stop = Arc::new(AtomicBool::new(false));
            let done = stop.clone();
            let (ready, started) = std::sync::mpsc::sync_channel(1);
            let (width, height) = (i32::try_from(width)?, i32::try_from(height)?);
            let thread = std::thread::spawn(move || unsafe {
                SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                let window = match CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                    w!("STATIC"),
                    w!("pb-mapper capture benchmark"),
                    WS_POPUP | WS_VISIBLE | WS_DISABLED,
                    x,
                    y,
                    width,
                    height,
                    None,
                    None,
                    None,
                    None,
                ) {
                    Ok(window) => window,
                    Err(e) => {
                        let _ = ready.send(Err(e.to_string()));
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                let dc = GetDC(Some(window));
                let _ = SetBkMode(dc, TRANSPARENT);
                let _ = SetTextColor(dc, COLORREF(0x00ffffff));
                let noise = (fill == Fill::Noise)
                    .then(|| Noise::new(dc, width, height))
                    .flatten();
                let text = (fill == Fill::Text)
                    .then(|| Text::new(dc, width, height))
                    .flatten();
                let mut frame = 0u32;
                let mut message = MSG::default();
                // The text is painted at first, and after each repaint the
                // window's own class made.
                let mut stale = true;
                while !done.load(Ordering::Acquire) {
                    while PeekMessageW(&mut message, Some(window), 0, 0, PM_REMOVE).as_bool() {
                        stale |= message.message == WM_PAINT;
                        let _ = DispatchMessageW(&message);
                    }
                    if let Some(text) = &text {
                        if std::mem::take(&mut stale) {
                            text.paint(dc);
                        }
                        // Nothing to present: DwmFlush, which waits for
                        // the next present, may not wait on a still screen.
                        std::thread::sleep(std::time::Duration::from_millis(15));
                        continue;
                    }
                    if let Some(noise) = &noise {
                        noise.paint(dc, frame);
                        frame = frame.wrapping_add(1);
                        if DwmFlush().is_err() {
                            break;
                        }
                        continue;
                    }
                    let brush = CreateSolidBrush(COLORREF(
                        ((frame * 3) & 255)
                            | (((frame * 5) & 255) << 8)
                            | (((frame * 7) & 255) << 16),
                    ));
                    let _ = FillRect(
                        dc,
                        &RECT {
                            left: 0,
                            top: 0,
                            right: width,
                            bottom: height,
                        },
                        brush,
                    );
                    let _ = DeleteObject(brush.into());
                    let text: Vec<u16> = format!("Unique desktop presentation {frame}")
                        .encode_utf16()
                        .collect();
                    let _ = TextOutW(dc, 32, 32, &text);
                    frame = frame.wrapping_add(1);
                    if DwmFlush().is_err() {
                        break;
                    }
                }
                drop(noise);
                drop(text);
                let _ = ReleaseDC(Some(window), dc);
                let _ = DestroyWindow(window);
            });
            started
                .recv_timeout(std::time::Duration::from_secs(3))?
                .map_err(std::io::Error::other)?;
            Ok(Self {
                stop,
                thread: Some(thread),
            })
        }
    }
    impl Drop for Pattern {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// A 32-bit bitmap the size of the window, refilled with xorshift noise.
    struct Noise {
        memory: HDC,
        bitmap: HBITMAP,
        bits: *mut u64,
        words: usize,
        width: i32,
        height: i32,
    }
    impl Noise {
        unsafe fn new(dc: HDC, width: i32, height: i32) -> Option<Self> {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = unsafe {
                CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?
            };
            let memory = unsafe { CreateCompatibleDC(Some(dc)) };
            unsafe { SelectObject(memory, bitmap.into()) };
            Some(Self {
                memory,
                bitmap,
                bits: bits.cast(),
                words: (width as usize * height as usize) / 2,
                width,
                height,
            })
        }
        unsafe fn paint(&self, dc: HDC, frame: u32) {
            let words = unsafe { std::slice::from_raw_parts_mut(self.bits, self.words) };
            let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ u64::from(frame).wrapping_mul(0xbf58_476d);
            for word in words {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *word = state;
            }
            let _ = unsafe {
                BitBlt(
                    dc,
                    0,
                    0,
                    self.width,
                    self.height,
                    Some(self.memory),
                    0,
                    0,
                    SRCCOPY,
                )
            };
        }
    }
    impl Drop for Noise {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.memory);
                let _ = DeleteObject(self.bitmap.into());
            }
        }
    }

    /// The lines of the text page, in turn: pangrams, code, the alphabet
    /// and the glyphs a codec blurs together first.
    const LINES: [&str; 5] = [
        "The quick brown fox jumps over the lazy dog. 0123456789 ",
        "fn encode(picture: &Nv12, budget: u32) -> Result<Unit> { cabac(budget / 8) } // ok ",
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz !@#$%^&*()[]{}<>=+-_ ",
        "Sphinx of black quartz, judge my vow: 1 l I | i ! ; : , . ' \" ` ~ rn m vv w ",
        "Pack my box with five dozen liquor jugs. 3.14159 2.71828 1.41421 0x7fff_ffff ",
    ];

    /// How each line is drawn, in turn: its height in pixels, face,
    /// weight, ink (`0x00bbggrr`) and the band behind it, if any (a dark
    /// one, as a dark theme's).
    const STYLES: [(i32, PCWSTR, FONT_WEIGHT, u32, Option<u32>); 8] = [
        (13, w!("Segoe UI"), FW_NORMAL, 0x0000_0000, None),
        (15, w!("Consolas"), FW_NORMAL, 0x0040_4040, None),
        (11, w!("Segoe UI"), FW_NORMAL, 0x0000_0000, None),
        (20, w!("Times New Roman"), FW_NORMAL, 0x00a0_3000, None),
        (
            13,
            w!("Consolas"),
            FW_NORMAL,
            0x00e0_e0e0,
            Some(0x001e_1e1e),
        ),
        (16, w!("Segoe UI"), FW_BOLD, 0x0000_00c0, None),
        (12, w!("Tahoma"), FW_NORMAL, 0x0060_6060, None),
        (28, w!("Segoe UI"), FW_NORMAL, 0x0000_0000, None),
    ];

    /// A still page of text, drawn once into a bitmap the size of the
    /// window with the system's text rendering (ClearType), and copied to
    /// the window when it needs painting.
    struct Text {
        memory: HDC,
        bitmap: HBITMAP,
        width: i32,
        height: i32,
    }
    impl Text {
        unsafe fn new(dc: HDC, width: i32, height: i32) -> Option<Self> {
            let bitmap = unsafe { CreateCompatibleBitmap(dc, width, height) };
            if bitmap.is_invalid() {
                return None;
            }
            let memory = unsafe { CreateCompatibleDC(Some(dc)) };
            unsafe { SelectObject(memory, bitmap.into()) };
            let text = Self {
                memory,
                bitmap,
                width,
                height,
            };
            unsafe { text.draw() };
            Some(text)
        }

        /// White, then line after line until the page is full, each
        /// repeated across it.
        unsafe fn draw(&self) {
            let memory = self.memory;
            unsafe {
                let _ = PatBlt(memory, 0, 0, self.width, self.height, WHITENESS);
                let _ = SetBkMode(memory, TRANSPARENT);
            }
            let mut y = 8;
            for line in 0.. {
                if y >= self.height {
                    break;
                }
                let (size, face, weight, ink, band) = STYLES[line % STYLES.len()];
                let advance = size * 3 / 2;
                if let Some(band) = band {
                    unsafe {
                        let brush = CreateSolidBrush(COLORREF(band));
                        let _ = FillRect(
                            memory,
                            &RECT {
                                left: 0,
                                top: y,
                                right: self.width,
                                bottom: y + advance,
                            },
                            brush,
                        );
                        let _ = DeleteObject(brush.into());
                    }
                }
                let words: Vec<u16> = LINES[line % LINES.len()].repeat(8).encode_utf16().collect();
                unsafe {
                    let font = CreateFontW(
                        -size,
                        0,
                        0,
                        0,
                        weight.0 as i32,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        OUT_DEFAULT_PRECIS,
                        CLIP_DEFAULT_PRECIS,
                        CLEARTYPE_QUALITY,
                        u32::from(DEFAULT_PITCH.0 | FF_DONTCARE.0),
                        face,
                    );
                    let previous = SelectObject(memory, font.into());
                    let _ = SetTextColor(memory, COLORREF(ink));
                    let _ = TextOutW(memory, 16, y + size / 4, &words);
                    SelectObject(memory, previous);
                    let _ = DeleteObject(font.into());
                }
                y += advance;
            }
        }

        unsafe fn paint(&self, dc: HDC) {
            let _ = unsafe {
                BitBlt(
                    dc,
                    0,
                    0,
                    self.width,
                    self.height,
                    Some(self.memory),
                    0,
                    0,
                    SRCCOPY,
                )
            };
        }
    }
    impl Drop for Text {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.memory);
                let _ = DeleteObject(self.bitmap.into());
            }
        }
    }
}
