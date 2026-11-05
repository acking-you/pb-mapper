//! Run with a visibly changing desktop. Static repeats do not count as capture
//! FPS. This measures capture/encode only, never network or viewer presentation.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use pb_mapper_desktop::{
        displays,
        video::windows::{Encoded, Encoder},
    };
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    let seconds: u64 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "10".into())
        .parse()?;
    if !(1..=300).contains(&seconds) {
        return Err("benchmark duration must be 1..300 seconds".into());
    }
    let fps: u32 = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "120".into())
        .parse()?;
    let display = displays()?.into_iter().next().ok_or("no display")?;
    let pattern_only = std::env::args().any(|arg| arg == "--pattern-only");
    let _animation = if pattern_only || std::env::args().any(|arg| arg == "--animate") {
        Some(pattern::Pattern::new(display.x, display.y)?)
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
    let backend_mode = std::env::args().any(|arg| arg == "--backend");
    let args: Vec<_> = std::env::args().collect();
    let max_width = args
        .windows(2)
        .find(|pair| pair[0] == "--max-width")
        .map(|pair| pair[1].parse())
        .transpose()?
        .unwrap_or(display.width);
    let max_height = args
        .windows(2)
        .find(|pair| pair[0] == "--max-height")
        .map(|pair| pair[1].parse())
        .transpose()?
        .unwrap_or(display.height);
    let bitrate = args
        .windows(2)
        .find(|pair| pair[0] == "--bitrate")
        .map(|pair| pair[1].parse())
        .transpose()?
        .unwrap_or(24_000_000);
    let mut encoder = if backend_mode {
        None
    } else {
        Some(Encoder::new_with_size(
            &display, fps, bitrate, max_width, max_height,
        )?)
    };
    let mut backend = if backend_mode {
        let mut backend = pb_mapper_desktop::native_backend(display.id)?;
        backend.configure_video(pb_mapper_desktop::VideoPreferences {
            max_width,
            max_height,
            max_fps: fps,
            bitrate,
        })?;
        Some(backend)
    } else {
        None
    };
    println!(
        "encoder={} display={}x{} target_fps={fps} max_size={max_width}x{max_height} target_bitrate={bitrate}",
        encoder.as_ref().map_or("full SDK backend", Encoder::name),
        display.width,
        display.height
    );
    let start = Instant::now();
    let mut first = true;
    let poll_us: u64 = args
        .windows(2)
        .find(|pair| pair[0] == "--poll-us")
        .map(|pair| pair[1].parse())
        .transpose()?
        .unwrap_or(8_333);
    if !(500..=100_000).contains(&poll_us) {
        return Err("poll interval must be 500..100000 microseconds".into());
    }
    let mut output = args
        .windows(2)
        .find(|pair| pair[0] == "--output")
        .map(|pair| std::fs::File::create(&pair[1]))
        .transpose()?;
    let mut changed_bitrate = false;
    let mut requested_idr = false;
    let mut pending_idr = None;
    let mut keyframes = 0;
    let mut backend_stats = pb_mapper_desktop::video::windows::Statistics::default();
    let mut context_us = 0u128;
    let mut capture_us = 0u128;
    let mut calls = 0u64;
    let pause_ms: u64 = args
        .windows(2)
        .find(|pair| pair[0] == "--pause-ms")
        .map(|pair| pair[1].parse())
        .transpose()?
        .unwrap_or(0);
    if pause_ms != 0 && (!(300..=1000).contains(&pause_ms) || backend_mode) {
        return Err("pause probe requires the encoder mode and 300..1000 ms".into());
    }
    let mut paused = false;
    let mut awaiting_recovery = false;
    while start.elapsed() < Duration::from_secs(seconds) {
        if pause_ms != 0
            && !paused
            && start.elapsed() > Duration::from_secs(1)
            && encoder.as_ref().is_some_and(|encoder| {
                let stats = encoder.statistics();
                stats.submitted > stats.encoded
            })
        {
            // Deliberately expire an outstanding GPU output, as a renderer
            // credit stall can do. The next exported frame must be independent.
            std::thread::sleep(Duration::from_millis(pause_ms));
            paused = true;
            awaiting_recovery = true;
        }
        let iteration = Instant::now();
        let force = !requested_idr && start.elapsed() > Duration::from_millis(1500);
        if force {
            requested_idr = true;
            pending_idr = Some(Instant::now());
        }
        if !changed_bitrate && start.elapsed() > Duration::from_secs(2) {
            if let Some(encoder) = &mut encoder {
                encoder.set_bitrate(bitrate.min(12_000_000))?;
            }
            changed_bitrate = true;
        }
        let frame = if let Some(encoder) = &mut encoder {
            encoder.next_frame(force)?
        } else if let Some(backend) = &mut backend {
            let before = Instant::now();
            let context = backend.context()?;
            context_us += before.elapsed().as_micros();
            let before = Instant::now();
            let frame = backend
                .capture_video(
                    &context,
                    pb_mapper_desktop::VideoCodec::H264,
                    force,
                    if changed_bitrate {
                        bitrate.min(12_000_000)
                    } else {
                        bitrate
                    },
                )?
                .map(|frame| Encoded {
                    bytes: frame.bytes,
                    width: frame.format.width,
                    height: frame.format.height,
                    keyframe: frame.format.keyframe,
                    duplicate: frame.format.duplicate,
                    pts_us: frame.format.pts_us,
                });
            capture_us += before.elapsed().as_micros();
            calls += 1;
            frame
        } else {
            None
        };
        if let Some(frame) = frame {
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
            if frame.keyframe {
                keyframes += 1;
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
        } else if !backend_mode {
            std::thread::sleep(Duration::from_millis(1));
        }
        if backend_mode {
            std::thread::sleep(Duration::from_micros(poll_us).saturating_sub(iteration.elapsed()));
        }
    }
    let stats = encoder.as_ref().map_or(backend_stats, Encoder::statistics);
    if pending_idr.is_some() {
        return Err("hardware encoder did not honor requested keyframe".into());
    }
    if pause_ms != 0 && (!paused || awaiting_recovery) {
        return Err("pause probe did not observe independent recovery".into());
    }
    let elapsed = start.elapsed().as_secs_f64();
    if backend_mode {
        println!(
            "seconds={elapsed:.3} keyframes={keyframes} encoded={} encoded_fps={:.2} unique_encoded_fps={:.2} bitrate_mbps={:.2} polls={} mean_context_ms={:.3} mean_capture_video_ms={:.3} (includes initial encoder startup)",
            stats.encoded,
            stats.encoded as f64 / elapsed,
            stats.unique_encoded as f64 / elapsed,
            stats.bytes as f64 * 8.0 / elapsed / 1_000_000.0,
            calls,
            context_us as f64 / calls.max(1) as f64 / 1000.0,
            capture_us as f64 / calls.max(1) as f64 / 1000.0
        );
        return Ok(());
    }
    println!(
        "seconds={elapsed:.3} keyframes={keyframes} unique_captures={} unique_capture_fps={:.2} encoded={} encoded_fps={:.2} unique_encoded_fps={:.2} bitrate_mbps={:.2} mean_encode_ms={:.3} max_in_flight={}",
        stats.unique_captures,
        stats.unique_captures as f64 / elapsed,
        stats.encoded,
        stats.encoded as f64 / elapsed,
        stats.unique_encoded as f64 / elapsed,
        stats.bytes as f64 * 8.0 / elapsed / 1_000_000.0,
        stats.encode_time_us as f64 / stats.encoded.max(1) as f64 / 1000.0,
        stats.max_in_flight
    );
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
            UI::WindowsAndMessaging::*,
        },
        core::w,
    };

    pub struct Pattern {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }
    impl Pattern {
        pub fn new(x: i32, y: i32) -> Result<Self, Box<dyn std::error::Error>> {
            let stop = Arc::new(AtomicBool::new(false));
            let done = stop.clone();
            let (ready, started) = std::sync::mpsc::sync_channel(1);
            let thread = std::thread::spawn(move || unsafe {
                let window = match CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("STATIC"),
                    w!("pb-mapper capture benchmark"),
                    WS_POPUP | WS_VISIBLE | WS_DISABLED,
                    x + 32,
                    y + 32,
                    960,
                    540,
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
                let mut frame = 0u32;
                let mut message = MSG::default();
                while !done.load(Ordering::Acquire) {
                    while PeekMessageW(&mut message, Some(window), 0, 0, PM_REMOVE).as_bool() {
                        let _ = DispatchMessageW(&message);
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
                            right: 960,
                            bottom: 540,
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
}
