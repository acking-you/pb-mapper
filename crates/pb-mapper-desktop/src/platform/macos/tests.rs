//! What runs on a Mac without a person: the encoder, and the displays and
//! modes as Core Graphics lists them. Capture and injection need the
//! owner's Screen Recording and Accessibility grants, which no CI runner
//! has; the manual checklist covers them.
#![cfg(target_os = "macos")]

use super::{desktop, encoder::Encoder, layout, modes::MacModes};
use crate::{
    Key, VideoCodec, Waker,
    codec::{
        annexb::{AccessUnit, ParameterSets},
        rate_fit::Fit,
    },
    platform::DisplayModes,
    scheduler::{Waiter, Wake},
};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use std::{
    io::Write,
    ptr::NonNull,
    time::{Duration, Instant},
};

fn picture(width: usize, height: usize) -> CFRetained<CVPixelBuffer> {
    let mut out = std::ptr::null_mut();
    // SAFETY: `out` is a local the call fills.
    let status = unsafe {
        CVPixelBufferCreate(
            None,
            width,
            height,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            None,
            NonNull::from(&mut out),
        )
    };
    assert_eq!(status, 0);
    // SAFETY: a Create-rule result.
    unsafe { CFRetained::from_raw(NonNull::new(out).unwrap()) }
}

/// The session encodes NV12 as Annex B: a keyframe exactly where one is
/// asked for, each carrying its SPS and PPS, and nothing but P-frames
/// between them (an unbounded GOP); the output wakes thread C. A unit the
/// encoder drops, or outputs later than `LATE_OUTPUT` and so discards (a
/// cold software encoder on a busy runner can take that long for its
/// first), leaves no output and a keyframe due: it is submitted again as
/// thread C would, as a keyframe.
#[test]
fn keyframes_come_only_when_asked_and_carry_their_parameter_sets() {
    let wake = Wake::new();
    let waiter = Waiter::new(&wake).unwrap();
    let mut encoder = Encoder::new(
        VideoCodec::H264,
        640,
        360,
        30,
        2_000_000,
        &Waker(wake.clone()),
    )
    .expect("VideoToolbox makes an H.264 session on every Mac, in software if need be");
    let picture = picture(640, 360);
    let asked = [true, false, false, false, true, false];
    for (frame, asked) in asked.into_iter().enumerate() {
        let mut keyframe = asked;
        let mut submitted = Instant::now();
        encoder.encode(&picture, keyframe, frame > 0).unwrap();
        let deadline = submitted + Duration::from_secs(3);
        let mut retries = 0;
        let unit = loop {
            // Read before the queue: an output leaves `pending` and joins the
            // queue under one lock, so nothing pending and nothing queued is
            // a unit that came to nothing.
            let idle = encoder.pending() == 0;
            if let Some(unit) = encoder.drain().unwrap() {
                break unit;
            }
            if idle {
                assert!(encoder.keyframe_due() || !keyframe, "frame {frame}");
                retries += 1;
                assert!(retries <= 3, "frame {frame} discarded {retries} times");
                // Past the harness's capture, so that the log says so.
                let _ = writeln!(
                    std::io::stderr(),
                    "{}: frame {frame} discarded {:?} after its submit; submitted again as a keyframe",
                    module_path!(),
                    submitted.elapsed()
                );
                keyframe = true;
                submitted = Instant::now();
                encoder.encode(&picture, keyframe, frame > 0).unwrap();
                continue;
            }
            assert!(Instant::now() < deadline, "no output for frame {frame}");
            // A discard wakes nothing: look again shortly.
            waiter
                .wait(deadline.min(Instant::now() + Duration::from_millis(10)))
                .unwrap();
        };
        let mut ranges = Vec::new();
        let parsed = AccessUnit::parse(&unit.bytes, &mut ranges).unwrap();
        assert_eq!(parsed.keyframe(VideoCodec::H264), keyframe, "frame {frame}");
        assert_eq!(unit.format.keyframe, keyframe, "frame {frame}");
        assert_eq!((unit.format.width, unit.format.height), (640, 360));
        if keyframe {
            let mut sets = ParameterSets::new(VideoCodec::H264);
            sets.observe(&parsed).unwrap();
            assert!(sets.complete() && sets.carried(&parsed), "frame {frame}");
        }
    }
    assert_eq!(encoder.pending(), 0);
}

/// Thread C's waits end on time where the kernel coalesces the process's
/// timers: on a CI runner a 2 ms condition-variable wait ended 8 ms late,
/// while this waiter's critical timer ends within about 1 ms.
#[test]
fn a_2ms_wait_is_not_coalesced() {
    let wake = Wake::new();
    let waiter = Waiter::new(&wake).unwrap();
    let mut late: Vec<Duration> = (0..21)
        .map(|_| {
            let deadline = Instant::now() + Duration::from_millis(2);
            while Instant::now() < deadline {
                waiter.wait(deadline).unwrap();
            }
            Instant::now().saturating_duration_since(deadline)
        })
        .collect();
    late.sort_unstable();
    let median = late[late.len() / 2];
    assert!(median < Duration::from_millis(2), "{late:?}");
}

/// A test process's main thread waits on the tests and runs no main queue,
/// as a command-line host's may not: the layout read asked of it waits at
/// most `FIRST_READ`, a second ask waits not at all, and named keys keep
/// their ANSI places (the runner's US layout has them there too). Never a
/// hang, never a panic.
#[test]
fn the_layout_read_never_waits_on_an_absent_main_loop() {
    let started = Instant::now();
    layout::request(layout::FIRST_READ);
    layout::request(layout::FIRST_READ);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(layout::current().code(Key::A), 0x00);
}

/// Every active display is named by its UUID and measured in its current
/// mode's pixels, and its modes list. A runner without a display skips.
#[test]
fn displays_are_named_by_uuid_and_measured_in_pixels() {
    let displays = desktop::displays().unwrap();
    if displays.is_empty() {
        // Past the harness's capture, so that the log says so.
        let _ = writeln!(
            std::io::stderr(),
            "skipped {}: this Mac has no active display",
            module_path!()
        );
        return;
    }
    for display in displays {
        let lengths: Vec<usize> = display.name.split('-').map(str::len).collect();
        assert_eq!(lengths, [8, 4, 4, 4, 12], "{}", display.name);
        let mut modes = MacModes;
        let current = modes.current(&display.name).unwrap();
        assert_eq!(
            (current.width, current.height),
            (display.width, display.height),
            "{display:?}"
        );
        assert!(current.scale >= 1);
        modes.list(&display.name).unwrap();
    }
}

/// Only a ScreenCaptureKit refusal or user stop ends capture consent.
#[test]
fn screen_capture_consent_is_classified_by_domain_and_code() {
    use super::capture::failure;
    use crate::{FailureKind, Permission};
    use objc2_foundation::{NSError, NSString};
    use objc2_screen_capture_kit::{SCStreamErrorCode, SCStreamErrorDomain};

    if !super::capture::available() {
        return;
    }
    let classify = |domain: &NSString, code: SCStreamErrorCode| {
        // SAFETY: the error has no userInfo dictionary.
        let err = unsafe { NSError::errorWithDomain_code_userInfo(domain, code.0, None) };
        failure(Some(&err))
    };
    // SAFETY: ScreenCaptureKit is available above.
    let domain = unsafe { SCStreamErrorDomain };
    assert_eq!(
        classify(domain, SCStreamErrorCode::UserDeclined).permission(),
        Some(Permission::ScreenCapture)
    );
    assert_eq!(
        classify(domain, SCStreamErrorCode::UserStopped).kind(),
        FailureKind::ConsentRevoked
    );
    assert_eq!(
        classify(
            &NSString::from_str("test.error"),
            SCStreamErrorCode::UserStopped
        )
        .kind(),
        FailureKind::CaptureFailed
    );
}

/// The stream leaves the pointer to the viewer and fills what the display
/// does not cover with opaque black, as NV12 has no alpha. It stays sRGB,
/// as every viewer shows it, and SDR where the system offers HDR.
#[test]
fn capture_without_a_pointer_stays_opaque() {
    use objc2::ClassType;
    use objc2_core_graphics::{CGColor, kCGColorSpaceSRGB};
    use objc2_screen_capture_kit::{SCCaptureDynamicRange, SCStreamConfiguration};

    if !super::capture::available() {
        return;
    }
    let prepared = super::capture::configuration(64, 64, true);
    let black = CGColor::new_srgb(0.0, 0.0, 0.0, 1.0);
    // SAFETY: the configuration is ours and holds its background for these
    // reads; the dynamic range is read only where its selector exists.
    unsafe {
        assert!(!prepared.config.showsCursor());
        let background = prepared.config.backgroundColor();
        assert!(CGColor::equal_to_color(Some(&background), Some(&black)));
        assert_eq!(
            prepared.config.colorSpaceName().to_string(),
            kCGColorSpaceSRGB.to_string()
        );
        if SCStreamConfiguration::class().responds_to(objc2::sel!(captureDynamicRange)) {
            assert_eq!(
                prepared.config.captureDynamicRange(),
                SCCaptureDynamicRange::SDR
            );
        }
    }
}

/// Each key a viewer can press keeps, on its key-down, every Fn, keypad and
/// Help flag Core Graphics makes it with: an arrow's are what Mission
/// Control's Control-arrow looks for.
#[test]
fn key_downs_keep_the_flags_core_graphics_gives_them() {
    use super::keys::{FUNCTION, HELP, NUMERIC_PAD, kept_key_flags, usage_code};
    use objc2_core_graphics::{CGEvent, CGEventSource, CGEventSourceStateID};

    // A private state: each key's own flags, whatever this Mac holds down.
    let source = CGEventSource::new(CGEventSourceStateID::Private).unwrap();
    for code in (0..=u16::MAX).filter_map(usage_code) {
        let event = CGEvent::new_keyboard_event(Some(&source), code, true).unwrap();
        let made = CGEvent::flags(Some(&event)).0 & (FUNCTION | HELP | NUMERIC_PAD);
        assert_eq!(made & !kept_key_flags(code, true), 0, "key {code:#x}");
    }
}

/// VideoToolbox's rate model on this Mac, which `video_bench --pace`
/// measures for Media Foundation: does the encoder budget each frame as its
/// share of the bitrate at the nominal rate (`nominal`), or per second of
/// presentation time (`timestamp`)? Each codec runs at 1080p, 1440p and 4K,
/// with low-latency rate control and without, at a nominal 60 FPS and
/// 4 Mbit/s, paced as thread C paces at 60, 30, 15 and 5 FPS for `STEP`
/// each, on IOSurface pictures covered in fresh grain: a fine quantizer
/// spends a lot on it, a coarse one nothing, so the encoder's budget rather
/// than the content should set each step's bitrate. It prints each step's
/// pictures submitted and units out (an encoder may drop pictures to hold
/// its rate), its dwell, its bitrate once settled against both models, the
/// fit, and the largest unit after a `PAUSE` without pictures; each
/// encoder's `macos_encoder_opened` line says whether it took the data-rate
/// cap; and a summary line a case. A case that fits neither model within
/// 25% is a finding, not a failure: what it informs reads every case.
///
/// Minutes of one Mac's hardware: run it in release mode on a quiet
/// machine, one test at a time:
/// `cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture
/// --test-threads=1 video_toolbox_rate_model`.
#[test]
#[ignore = "a measurement: about 2.5 minutes of H.264 encoding, in release mode"]
fn video_toolbox_rate_model_h264() {
    rate_model_sweeps(VideoCodec::H264);
}

/// `video_toolbox_rate_model_h264` for H.265, on the hardware encoder.
#[test]
#[ignore = "a measurement: about 2.5 minutes of H.265 encoding, in release mode"]
fn video_toolbox_rate_model_h265() {
    rate_model_sweeps(VideoCodec::H265);
}

/// The measurement's nominal rate and bitrate: `video_bench`'s defaults.
const NOMINAL_FPS: u32 = 60;
const BITRATE: u32 = 4_000_000;
const PACES: [u32; 4] = [60, 30, 15, 5];
/// How long each pace runs; its bitrate counts after `SETTLE`.
const STEP: Duration = Duration::from_secs(6);
const SETTLE: Duration = Duration::from_secs(2);
/// How long pictures stop after the sweep before three more come.
const PAUSE: Duration = Duration::from_secs(1);
const SIZES: [(u32, u32); 3] = [(1920, 1080), (2560, 1440), (3840, 2160)];
/// The pictures painted in turn: one is painted again only once its unit
/// is out.
const RING: usize = 3;

/// A line past the harness's capture, so that the run's log has it.
fn report(line: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Every size, with low-latency rate control and without, then a summary.
fn rate_model_sweeps(codec: VideoCodec) {
    if !super::encoder::encodes(codec) {
        report(format_args!(
            "skipped: this Mac does not encode {codec:?} on hardware"
        ));
        return;
    }
    let mut cases = Vec::new();
    for (width, height) in SIZES {
        for low_latency in [true, false] {
            let fit = rate_model_sweep(codec, width, height, low_latency);
            cases.push((width, height, low_latency, fit));
        }
    }
    for (width, height, low_latency, fit) in cases {
        let (model, within) = fit
            .as_ref()
            .map_or(("refused", false), |fit| (fit.model(), fit.within));
        report(format_args!(
            "summary codec={codec:?} size={width}x{height} low_latency={low_latency} rate_model={model} within_25_percent={within}"
        ));
    }
}

/// One encoder's sweep and pause, or `None` where VideoToolbox refused it.
fn rate_model_sweep(codec: VideoCodec, width: u32, height: u32, low_latency: bool) -> Option<Fit> {
    report(format_args!(
        "case codec={codec:?} size={width}x{height} low_latency={low_latency} nominal_fps={NOMINAL_FPS} bitrate={BITRATE}"
    ));
    let wake = Wake::new();
    let waiter = Waiter::new(&wake).unwrap();
    // The encoder's own line, with whether it took the data-rate cap.
    let opened = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .finish();
    let built = tracing::subscriber::with_default(opened, || {
        Encoder::with_low_latency(
            codec,
            width,
            height,
            NOMINAL_FPS,
            BITRATE,
            &Waker(wake.clone()),
            low_latency,
        )
    });
    let encoder = match built {
        Ok(encoder) => encoder,
        Err(failure) => {
            report(format_args!("refused: {failure}"));
            return None;
        }
    };
    let (width, height) = (width as usize, height as usize);
    let mut pacer = Pacer {
        encoder,
        waiter,
        ring: [(); RING].map(|()| surface(width, height)),
        frame: 0,
        skipped: 0,
    };
    let mut sweep = Vec::new();
    for pace in PACES {
        let start = Instant::now();
        pacer.run(Some(pace), start + SETTLE, |_| {});
        let (submitted, skipped) = (pacer.frame, pacer.skipped);
        let (mut bytes, mut keyframes, mut dwell) = (0u64, 0u32, Vec::new());
        pacer.run(Some(pace), start + STEP, |unit| {
            bytes += unit.bytes.len() as u64;
            keyframes += u32::from(unit.format.keyframe);
            dwell.push(unit.format.encode_us);
        });
        dwell.sort_unstable();
        let percentile = |share: f64| {
            let index = ((dwell.len() as f64 * share) as usize).min(dwell.len().saturating_sub(1));
            dwell.get(index).map_or(0.0, |us| f64::from(*us) / 1000.0)
        };
        report(format_args!(
            "pace={pace} submitted={} units={} keyframes={keyframes} skipped_ticks={} bits_per_unit={:.0} dwell_p50_ms={:.2} dwell_p95_ms={:.2}",
            pacer.frame - submitted,
            dwell.len(),
            pacer.skipped - skipped,
            bytes as f64 * 8.0 / dwell.len().max(1) as f64,
            percentile(0.5),
            percentile(0.95),
        ));
        sweep.push((pace, bytes as f64 * 8.0 / (STEP - SETTLE).as_secs_f64()));
    }
    let fit = Fit::new(BITRATE, NOMINAL_FPS, &sweep);
    for step in &fit.steps {
        report(format_args!(
            "pace={} produced_mbps={:.3} nominal_expected_mbps={:.3} nominal_ratio={:.3} timestamp_ratio={:.3}",
            step.pace,
            step.produced_bps / 1e6,
            step.nominal_bps / 1e6,
            step.nominal,
            step.timestamp,
        ));
    }
    report(format_args!(
        "rate_model={} within_25_percent={}",
        fit.model(),
        fit.within
    ));
    // A timestamp encoder may spend a pause's budget on the picture after it,
    // which DataRateLimits bounds only over its second.
    pacer.run(None, Instant::now() + PAUSE, |_| {});
    let mut largest = 0;
    let mut after_pause = |unit: &crate::EncodedFrame| {
        if !unit.format.keyframe {
            largest = largest.max(unit.bytes.len());
        }
    };
    let resumed = Instant::now();
    pacer.run(
        Some(NOMINAL_FPS),
        resumed + Duration::from_millis(40),
        &mut after_pause,
    );
    pacer.run(None, resumed + Duration::from_millis(500), &mut after_pause);
    let budget = BITRATE / NOMINAL_FPS;
    report(format_args!(
        "after_pause largest_unit_bits={} frame_budget_bits={budget} ratio={:.2}",
        largest * 8,
        (largest * 8) as f64 / f64::from(budget),
    ));
    Some(fit)
}

/// Thread C's pacing of one encoder over a ring of pictures, as
/// `video_bench` paces Media Foundation.
struct Pacer {
    encoder: Encoder,
    waiter: Waiter,
    ring: [CFRetained<CVPixelBuffer>; RING],
    /// Pictures submitted.
    frame: u64,
    /// Ticks with a full encoder, which submitted nothing.
    skipped: u64,
}
impl Pacer {
    /// Until `until`: a picture each tick at `pace`, none while `None`; each
    /// unit out goes to `unit`.
    fn run(
        &mut self,
        pace: Option<u32>,
        until: Instant,
        mut unit: impl FnMut(&crate::EncodedFrame),
    ) {
        let mut next_tick = Instant::now();
        loop {
            while let Some(out) = self.encoder.drain().unwrap() {
                unit(&out);
            }
            let now = Instant::now();
            if now >= until {
                return;
            }
            let Some(pace) = pace else {
                self.waiter.wait(until).unwrap();
                continue;
            };
            if now >= next_tick {
                let period = Duration::from_secs(1) / pace;
                next_tick = (next_tick + period).max(now + period * 3 / 4);
                if self.encoder.pending() < RING {
                    let picture = &self.ring[self.frame as usize % RING];
                    paint(picture, self.frame);
                    let keyframe = self.frame == 0 || self.encoder.keyframe_due();
                    self.encoder.encode(picture, keyframe, false).unwrap();
                    self.frame += 1;
                } else {
                    self.skipped += 1;
                }
            }
            self.waiter.wait(next_tick.min(until)).unwrap();
        }
    }
}

/// An IOSurface-backed NV12 picture, which the encoder reads in place as it
/// reads ScreenCaptureKit's.
fn surface(width: usize, height: usize) -> CFRetained<CVPixelBuffer> {
    let none = super::cf::dictionary(&[]);
    // SAFETY: a static key.
    let attributes =
        super::cf::dictionary(&[(unsafe { kCVPixelBufferIOSurfacePropertiesKey }, &none)]);
    let mut out = std::ptr::null_mut();
    // SAFETY: `out` is a local the call fills; the attributes outlive it.
    let status = unsafe {
        CVPixelBufferCreate(
            None,
            width,
            height,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            Some(&attributes),
            NonNull::from(&mut out),
        )
    };
    assert_eq!(status, 0);
    // SAFETY: a Create-rule result.
    unsafe { CFRetained::from_raw(NonNull::new(out).unwrap()) }
}

/// Picture `frame` of the pattern: rows of block text scrolling up the left
/// half, a still ramp on the right, grey chroma, and fresh grain of ±16 over
/// all of the luma, which no reference predicts.
fn paint(picture: &CVPixelBuffer, frame: u64) {
    let width = CVPixelBufferGetWidth(picture);
    let mut grain = frame.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    // SAFETY: locked around the writes.
    assert_eq!(
        unsafe { CVPixelBufferLockBaseAddress(picture, CVPixelBufferLockFlags(0)) },
        0
    );
    for plane in 0..2 {
        let stride = CVPixelBufferGetBytesPerRowOfPlane(picture, plane);
        let rows = CVPixelBufferGetHeightOfPlane(picture, plane);
        let base = CVPixelBufferGetBaseAddressOfPlane(picture, plane).cast::<u8>();
        assert!(!base.is_null() && stride >= width);
        // SAFETY: the locked plane's rows, `stride` bytes each.
        let bytes = unsafe { std::slice::from_raw_parts_mut(base, stride * rows) };
        for (y, row) in bytes.chunks_exact_mut(stride).enumerate() {
            // NV12's chroma interleaves U and V: as many bytes a row as luma.
            let row = &mut row[..width];
            if plane == 1 {
                row.fill(128);
                continue;
            }
            text_and_ramp(row, y, frame);
            for chunk in row.chunks_mut(8) {
                grain ^= grain << 13;
                grain ^= grain >> 7;
                grain ^= grain << 17;
                for (pixel, noise) in chunk.iter_mut().zip(grain.to_le_bytes()) {
                    *pixel = pixel.saturating_add(noise & 31).saturating_sub(16);
                }
            }
        }
    }
    // SAFETY: locked above.
    unsafe { CVPixelBufferUnlockBaseAddress(picture, CVPixelBufferLockFlags(0)) };
}

/// Luma row `y` of picture `frame`: 8 × 16 glyph cells, most of them inked,
/// scrolling up 4 rows a picture on the left half; a ramp on the right.
fn text_and_ramp(row: &mut [u8], y: usize, frame: u64) {
    let half = row.len() / 2;
    let line = y as u64 + frame * 4;
    let (text_row, inner_y) = (line / 16, line % 16);
    let (text, ramp) = row.split_at_mut(half);
    for (x, pixel) in text.iter_mut().enumerate() {
        let cell = (x / 8) as u64;
        let mut hash =
            text_row.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ cell.wrapping_mul(0xC2B2_AE3D);
        hash ^= hash >> 29;
        let inked = hash & 3 != 0 && (2..14).contains(&inner_y) && (1..7).contains(&(x % 8));
        *pixel = if inked { 48 } else { 208 };
    }
    for (x, pixel) in ramp.iter_mut().enumerate() {
        *pixel = (16 + (half + x + y) / 8 % 224) as u8;
    }
}
