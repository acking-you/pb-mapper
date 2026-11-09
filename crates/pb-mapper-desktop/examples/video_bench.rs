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
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use pb_mapper_desktop::{
        Backend, DisplaySelection, VideoCodec, VideoRate, displays,
        windows::video::{Encoded, Encoder, Statistics},
    };
    use std::{
        io::Write,
        time::{Duration, Instant},
    };

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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let value = |name: &str| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].as_str())
    };
    let mut positional = Vec::new();
    let mut operand = false;
    for arg in &args {
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
    let paces: Option<Vec<u32>> = value("--pace")
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
        && (seconds < 3 || paces.is_empty() || paces.iter().any(|pace| !(1..=fps).contains(pace)))
    {
        return Err("--pace needs 3 s or more per step, and paces of 1..fps".into());
    }
    let codec = match value("--codec").unwrap_or("h264") {
        "h264" => VideoCodec::H264,
        "h265" => VideoCodec::H265,
        other => return Err(format!("unknown codec {other}: h264 or h265").into()),
    };
    let display = displays()?.into_iter().next().ok_or("no display")?;
    let pattern_only = flag("--pattern-only");
    let noise = flag("--noise");
    let _animation = if value("--dump").is_some() || noise {
        Some(pattern::Pattern::new(
            display.x,
            display.y,
            display.width,
            display.height,
            noise,
        )?)
    } else if pattern_only || flag("--animate") || paces.is_some() {
        Some(pattern::Pattern::new(
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
            "Animating benchmark window for {seconds}s; no encoder is running in this process"
        );
        std::thread::sleep(Duration::from_secs(seconds));
        return Ok(());
    }
    let backend_mode = flag("--backend");
    let max_width = value("--max-width")
        .map(str::parse)
        .transpose()?
        .unwrap_or(display.width);
    let max_height = value("--max-height")
        .map(str::parse)
        .transpose()?
        .unwrap_or(display.height);
    let bitrate: u32 =
        value("--bitrate")
            .map(str::parse)
            .transpose()?
            .unwrap_or(if paces.is_some() {
                4_000_000
            } else {
                24_000_000
            });
    let frames: Option<u64> = value("--frames").map(str::parse).transpose()?;
    let poll = Duration::from_micros(
        value("--poll-us")
            .map(str::parse)
            .transpose()?
            .unwrap_or(8_333),
    );
    if !(Duration::from_micros(500)..=Duration::from_millis(100)).contains(&poll) {
        return Err("poll interval must be 500..100000 microseconds".into());
    }
    let pause_ms: u64 = value("--pause-ms")
        .map(str::parse)
        .transpose()?
        .unwrap_or(0);
    if pause_ms != 0 && (!(300..=1000).contains(&pause_ms) || backend_mode) {
        return Err("pause probe requires the encoder mode and 300..1000 ms".into());
    }
    let mut output = value("--output").map(std::fs::File::create).transpose()?;
    let mut dump = value("--dump")
        .map(std::fs::File::create)
        .transpose()?
        .map(std::io::BufWriter::new);
    let exact = flag("--exact");
    if exact && backend_mode {
        return Err("--exact drives the encoder; a backend never upscales".into());
    }
    if codec == VideoCodec::H265 {
        // What a host asks before it offers H.265: probed once per adapter.
        for _ in 0..2 {
            let asked = Instant::now();
            let usable = pb_mapper_desktop::windows::video::h265_available(display.id);
            println!(
                "h265_available={usable} probe_ms={:.1}",
                asked.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
    let building = Instant::now();
    let mut encoder = if backend_mode {
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
    let mut backend = if backend_mode {
        let mut backend = pb_mapper_desktop::native_backend(&DisplaySelection::Id(display.id))?;
        backend.configure_video(
            pb_mapper_desktop::VideoLimits {
                max_width,
                max_height,
                max_fps: fps,
            },
            fps,
        )?;
        Some(backend)
    } else {
        None
    };
    println!(
        "encoder={} codec={codec:?} build_ms={build_ms:.1} display={}x{} nominal_fps={fps} max_size={max_width}x{max_height}{} target_bitrate={bitrate}",
        encoder.as_ref().map_or("full SDK backend", Encoder::name),
        display.width,
        display.height,
        if exact { " exact" } else { "" },
    );

    // Each step runs `seconds` at its pace; its bitrate is measured after the
    // first 2 s, once the rate controller has settled.
    let steps = paces.clone().unwrap_or_else(|| vec![fps]);
    let settle = Duration::from_secs(2);
    let mut measured = vec![0u64; steps.len()];
    let start = Instant::now();
    let mut first = true;
    let mut written = 0u64;
    let mut forced = 0u32;
    let mut pending_idr = None;
    let mut keyframes = 0;
    let (mut max_keyframe, mut max_unit) = (0, 0);
    let mut current_bitrate = bitrate;
    let mut backend_stats = Statistics::default();
    let mut context_us = 0u128;
    let mut capture_us = 0u128;
    let mut calls = 0u64;
    let mut paused = false;
    let mut awaiting_recovery = false;
    for (step, &pace) in steps.iter().enumerate() {
        let step_start = Instant::now();
        let period = Duration::from_secs(1) / pace;
        let mut next_tick = step_start;
        while step_start.elapsed() < Duration::from_secs(seconds)
            && frames.is_none_or(|frames| written < frames)
        {
            if pause_ms != 0
                && !paused
                && start.elapsed() > Duration::from_secs(1)
                && encoder.as_ref().is_some_and(|encoder| {
                    let stats = encoder.statistics();
                    stats.submitted > stats.encoded
                })
            {
                // Deliberately expire an outstanding GPU output, as a long
                // stall can do. The next exported frame must be independent.
                std::thread::sleep(Duration::from_millis(pause_ms));
                paused = true;
                awaiting_recovery = true;
            }
            if paces.is_none() && start.elapsed() > Duration::from_secs(2) {
                current_bitrate = bitrate.min(12_000_000);
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
            let force = tick && forced == 0 && start.elapsed() > Duration::from_millis(1500);
            if force {
                forced = 1;
                pending_idr = Some(now);
            }
            let mut ready = Vec::new();
            if let Some(encoder) = &mut encoder {
                encoder.set_rate(rate)?;
                while let Some(frame) = encoder.drain()? {
                    ready.push(frame);
                }
                if tick {
                    encoder.submit(force)?;
                }
            } else if let Some(backend) = &mut backend {
                let before = Instant::now();
                let context = backend.context()?;
                context_us += before.elapsed().as_micros();
                let before = Instant::now();
                let frame = if tick {
                    backend.capture_video(&context, codec, force, rate)?
                } else {
                    backend.drain_video(&context, Duration::ZERO)?
                };
                capture_us += before.elapsed().as_micros();
                calls += 1;
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
            for frame in ready {
                if awaiting_recovery {
                    if !frame.keyframe {
                        return Err("expired GPU output leaked dependent video".into());
                    }
                    println!("pause_recovery_keyframe=true pause_ms={pause_ms}");
                    awaiting_recovery = false;
                }
                backend_stats.encoded += 1;
                backend_stats.unique_encoded += u64::from(!frame.duplicate);
                backend_stats.bytes += frame.bytes.len() as u64;
                backend_stats.gop = frame.gop;
                if step_start.elapsed() >= settle {
                    measured[step] += frame.bytes.len() as u64;
                }
                max_unit = max_unit.max(frame.bytes.len());
                if frame.keyframe {
                    keyframes += 1;
                    max_keyframe = max_keyframe.max(frame.bytes.len());
                    if let Some(request) = pending_idr.take() {
                        println!(
                            "forced_keyframe_ms={:.3}",
                            request.elapsed().as_secs_f64() * 1000.0
                        );
                    }
                }
                if let Some(file) = &mut output {
                    file.write_all(&frame.bytes)?;
                }
                if let Some(file) = &mut dump {
                    file.write_all(&u32::try_from(frame.bytes.len())?.to_be_bytes())?;
                    file.write_all(&frame.bytes)?;
                }
                if first {
                    println!(
                        "first_access_unit={} keyframe={} size={}x{}",
                        frame.bytes.len(),
                        frame.keyframe,
                        frame.width,
                        frame.height
                    );
                    first = false;
                }
                written += 1;
            }
            let idle = if backend_mode {
                poll
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
    if let Some(file) = &mut dump {
        file.flush()?;
        println!("dumped_units={written}");
    }
    let stats = encoder.as_ref().map_or(backend_stats, Encoder::statistics);
    if pending_idr.is_some() {
        return Err("hardware encoder did not honor requested keyframe".into());
    }
    if pause_ms != 0 && (!paused || awaiting_recovery) {
        return Err("pause probe did not observe independent recovery".into());
    }
    // 0: the encoder reports none. A finite GOP under 5 s fails.
    if stats.gop != 0 && u64::from(stats.gop) < 5 * u64::from(fps) {
        return Err(format!("encoder GOP is {} frames, under 5 s", stats.gop).into());
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!("max_keyframe_bytes={max_keyframe} max_access_unit_bytes={max_unit}");
    if backend_mode {
        println!(
            "seconds={elapsed:.3} keyframes={keyframes} forced={forced} gop={} encoded={} encoded_fps={:.2} unique_encoded_fps={:.2} bitrate_mbps={:.2} polls={} mean_context_ms={:.3} mean_capture_video_ms={:.3} (includes initial encoder startup)",
            stats.gop,
            stats.encoded,
            stats.encoded as f64 / elapsed,
            stats.unique_encoded as f64 / elapsed,
            stats.bytes as f64 * 8.0 / elapsed / 1_000_000.0,
            calls,
            context_us as f64 / calls.max(1) as f64 / 1000.0,
            capture_us as f64 / calls.max(1) as f64 / 1000.0
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
    if let Some(paces) = &paces {
        // `Nominal` encoders budget each frame from the nominal rate, so their
        // bitrate falls with the pace; `Timestamp` encoders hold the target.
        let window = (Duration::from_secs(seconds) - settle).as_secs_f64();
        let ratios: Vec<(f64, f64)> = paces
            .iter()
            .zip(&measured)
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
#[cfg(not(windows))]
fn main() {}

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
