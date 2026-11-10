//! Capture and encode benchmark: `video_bench [seconds] [fps] [flags]`. Run it
//! with a visibly changing desktop, or with `--animate`. Static repeats do not
//! count as capture FPS. It measures capture and encode only, never network or
//! viewer presentation, and paces its submissions as the host's capture worker
//! does: the encoder never paces itself.
//!
//! - `--pattern-only`: animate the pattern window; no encoder runs here.
//! - `--animate`: animate the pattern window and encode it.
//! - `--backend`: drive the SDK backend instead of the encoder.
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
//!
//! It prints the largest keyframe and access unit, which `wire::MAX_PAYLOAD`
//! must hold.
#[cfg(windows)]
mod probe_support;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    bench::run()
}
#[cfg(not(windows))]
fn main() {}

/// The benchmark: its options, the paced loop and the report.
#[cfg(windows)]
mod bench {
    use crate::{pattern::Pattern, probe_support::option};
    use pb_mapper_desktop::{
        Backend, Display, DisplaySelection, NativeBackend, VideoCodec, VideoLimits, VideoRate,
        displays, native_backend,
        windows::video::{Encoded, Encoder, Statistics, h265_available},
    };
    use std::{
        fs::File,
        io::{BufWriter, Write},
        time::{Duration, Instant},
    };

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    /// The options that take a value, which is then no positional argument.
    const VALUED: [&str; 10] = [
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
    ];
    /// A step's bitrate is measured after this long, once the rate controller
    /// has settled.
    const SETTLE: Duration = Duration::from_secs(2);

    pub fn run() -> Result {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let rates = Rates::parse(&args)?;
        let display = displays()?.into_iter().next().ok_or("no display")?;
        let pattern_only = flag(&args, "--pattern-only");
        let noise = flag(&args, "--noise");
        let _animation = if option(&args, "--dump").is_some() || noise {
            Some(Pattern::new(
                display.x,
                display.y,
                display.width,
                display.height,
                noise,
            )?)
        } else if pattern_only || flag(&args, "--animate") || rates.paces.is_some() {
            Some(Pattern::new(
                display.x + 32,
                display.y + 32,
                960,
                540,
                false,
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
        let building = Instant::now();
        let encoder = if limits.backend {
            None
        } else if exact {
            Some(Encoder::new_exact(
                &display, codec, fps, bitrate, max_width, max_height,
            )?)
        } else {
            Some(Encoder::new_with_size(
                &display, codec, fps, bitrate, max_width, max_height,
            )?)
        };
        let build_ms = building.elapsed().as_secs_f64() * 1000.0;
        let mut coder = match encoder {
            Some(encoder) => Coder::Encoder(encoder),
            None => {
                let mut backend = native_backend(&DisplaySelection::Id(display.id))?;
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
            "encoder={} codec={codec:?} build_ms={build_ms:.1} display={}x{} nominal_fps={fps} max_size={max_width}x{max_height}{} target_bitrate={bitrate}",
            coder.name(),
            display.width,
            display.height,
            if exact { " exact" } else { "" },
        );

        let steps = rates.steps();
        let mut tally = Tally {
            measured: vec![0; steps.len()],
            ..Tally::default()
        };
        let start = Instant::now();
        paced(
            &rates, &limits, &steps, &mut coder, &mut sinks, &mut tally, start,
        )?;
        if let Some(file) = &mut sinks.dump {
            file.flush()?;
            println!("dumped_units={}", tally.written);
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

    fn flag(args: &[String], name: &str) -> bool {
        args.iter().any(|arg| arg == name)
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
        max_width: u32,
        max_height: u32,
        bitrate: u32,
        frames: Option<u64>,
        poll: Duration,
        pause_ms: u64,
        exact: bool,
    }
    impl Limits {
        /// The limits, and the files they name, created as they are read.
        fn parse(args: &[String], rates: &Rates, display: &Display) -> Result<(Self, Sinks)> {
            let backend = flag(args, "--backend");
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
            let limits = Self {
                backend,
                max_width,
                max_height,
                bitrate,
                frames,
                poll,
                pause_ms,
                exact,
            };
            Ok((limits, Sinks { output, dump }))
        }
    }

    /// The files the access units are written to: raw Annex B, and the
    /// length-prefixed clip.
    struct Sinks {
        output: Option<File>,
        dump: Option<BufWriter<File>>,
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
        /// The pause probe stalled the encoder, and its next unit must be a
        /// keyframe.
        paused: bool,
        awaiting_recovery: bool,
        first_reported: bool,
    }
    impl Tally {
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

    /// Each step runs the run's seconds at its pace, as the host's capture
    /// worker paces: a submission on each tick, every ready unit drained in
    /// between, until `--frames` units are out.
    fn paced(
        rates: &Rates,
        limits: &Limits,
        steps: &[u32],
        coder: &mut Coder,
        sinks: &mut Sinks,
        tally: &mut Tally,
        start: Instant,
    ) -> Result {
        let mut current_bitrate = limits.bitrate;
        for (step, &pace) in steps.iter().enumerate() {
            let step_start = Instant::now();
            let period = Duration::from_secs(1) / pace;
            let mut next_tick = step_start;
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
                let tick = now >= next_tick;
                if tick {
                    next_tick = (next_tick + period).max(now + period * 3 / 4);
                }
                let force =
                    tick && tally.forced == 0 && start.elapsed() > Duration::from_millis(1500);
                if force {
                    tally.forced = 1;
                    tally.pending_idr = Some(now);
                }
                let mut ready = Vec::new();
                match coder {
                    Coder::Encoder(encoder) => {
                        encoder.set_rate(rate)?;
                        while let Some(frame) = encoder.drain()? {
                            ready.push(frame);
                        }
                        if tick {
                            encoder.submit(force)?;
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
                    }
                }
                for frame in ready {
                    tally.record(&frame, step, step_start, sinks, limits.pause_ms)?;
                }
                let idle = if limits.backend {
                    limits.poll
                } else {
                    Duration::from_millis(1)
                };
                std::thread::sleep(
                    next_tick
                        .saturating_duration_since(Instant::now())
                        .min(idle),
                );
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
        if let Some(paces) = &rates.paces {
            // `Nominal` encoders budget each frame from the nominal rate, so their
            // bitrate falls with the pace; `Timestamp` encoders hold the target.
            let (bitrate, fps) = (limits.bitrate, rates.fps);
            let window = (Duration::from_secs(rates.seconds) - SETTLE).as_secs_f64();
            let ratios: Vec<(f64, f64)> = paces
                .iter()
                .zip(&tally.measured)
                .map(|(&pace, &bytes)| {
                    let produced = bytes as f64 * 8.0 / window;
                    let nominal = f64::from(bitrate) * f64::from(pace) / f64::from(fps);
                    println!(
                        "pace={pace} produced_mbps={:.3} nominal_expected_mbps={:.3} nominal_ratio={:.3} timestamp_ratio={:.3}",
                        produced / 1e6,
                        nominal / 1e6,
                        produced / nominal,
                        produced / f64::from(bitrate)
                    );
                    (produced / nominal, produced / f64::from(bitrate))
                })
                .collect();
            let score = |pick: fn(&(f64, f64)) -> f64| {
                ratios
                    .iter()
                    .map(|ratio| pick(ratio).ln().abs())
                    .sum::<f64>()
            };
            let nominal = score(|ratio| ratio.0) <= score(|ratio| ratio.1);
            let within = ratios
                .iter()
                .map(|ratio| if nominal { ratio.0 } else { ratio.1 })
                .all(|ratio| (0.75..=1.25).contains(&ratio));
            println!(
                "rate_model={} within_25_percent={within} gop={}",
                if nominal { "nominal" } else { "timestamp" },
                stats.gop
            );
            if !within {
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
        core::w,
    };

    /// A topmost window that repaints every display refresh, at physical
    /// desktop coordinates: a changing colour, or fresh noise.
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
            noise: bool,
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
                let noise = noise.then(|| Noise::new(dc, width, height)).flatten();
                let mut frame = 0u32;
                let mut message = MSG::default();
                while !done.load(Ordering::Acquire) {
                    while PeekMessageW(&mut message, Some(window), 0, 0, PM_REMOVE).as_bool() {
                        let _ = DispatchMessageW(&message);
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
}
