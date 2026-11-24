//! What runs on a Mac without a person: the encoder, and the displays and
//! modes as Core Graphics lists them. Capture and injection need the
//! owner's Screen Recording and Accessibility grants, which no CI runner
//! has; the manual checklist covers them.
#![cfg(target_os = "macos")]

use super::{
    desktop,
    encoder::Encoder,
    layout,
    modes::MacModes,
    power,
    reuse::{REUSE, Reuse},
};
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
    cell::Cell,
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

/// What a session's start waits on once it woke a display: an awake one
/// draws, found by its UUID or, without one, by its id, and another UUID
/// never matches it, or the wait would run out at every start; nor is it
/// taken for one that sleeps, which capture would fail. Every active
/// display is online, and the lid reads on a Mac without one. Nothing is
/// woken.
#[test]
fn a_woken_display_is_found_by_the_name_it_is_bound_by() {
    use crate::Display;
    use objc2_core_graphics::CGDisplayIsAsleep;
    let online = power::online();
    for display in desktop::displays().unwrap() {
        assert!(online.contains(&display.id), "{display:?}");
        if CGDisplayIsAsleep(display.id) {
            continue;
        }
        assert!(power::drawing(&display), "{display:?}");
        assert!(!power::sleeping(display.id), "{display:?}");
        let unnamed = Display {
            name: String::new(),
            ..display.clone()
        };
        assert!(power::drawing(&unnamed), "{display:?}");
        let other = Display {
            name: "00000000-0000-0000-0000-000000000000".into(),
            ..display
        };
        assert!(!power::drawing(&other), "{other:?}");
    }
    let _ = power::lid_closed();
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

/// `MacDesktop` reads WindowServer for every check the injection boundary,
/// a display mode change and the context check make (`prepare`, `display`,
/// `display_matches`), and lets the checks around a capture or a pointer
/// read (`prepare_recent`, `display_matches_recent`) take one that passed
/// within `REUSE`; after a forget, as its own mode change makes, they read.
/// Needs no grant. A runner without a display skips; where the session
/// check fails (a locked screen), only its failure is checked, which is
/// read every time and never reused.
#[test]
fn only_capture_and_the_pointer_reuse_a_mac_desktop_check() {
    use crate::{DisplaySelection, platform::Desktop};

    let Ok(display) = desktop::resolve(&DisplaySelection::Primary) else {
        // Past the harness's capture, so that the log says so.
        let _ = writeln!(
            std::io::stderr(),
            "skipped {}: this Mac has no active display",
            module_path!()
        );
        return;
    };
    let mut mac = desktop::MacDesktop::open(&display).unwrap();
    let expected = mac.display().unwrap();
    let matches = |mac: &mut desktop::MacDesktop, recent: bool| {
        let matched = if recent {
            mac.display_matches_recent(&expected)
        } else {
            mac.display_matches(&expected)
        };
        assert!(matched.unwrap(), "the display changed during the test");
    };

    // The geometry: a fresh check reads every time, and renews what the
    // recent ones take.
    let fresh = reads_in(&mut mac, |mac| {
        matches(mac, false);
        matches(mac, false);
        mac.display().unwrap();
    });
    assert_eq!(fresh, (0, 3));
    let recent = reads_in(&mut mac, |mac| {
        mac.forget_recent();
        mac.display().unwrap();
        matches(mac, true);
        matches(mac, true);
    });
    assert_eq!(recent, (0, 1));
    let forgot = reads_in(&mut mac, |mac| {
        matches(mac, false);
        mac.forget_recent();
        matches(mac, true);
        matches(mac, true);
    });
    assert_eq!(forgot, (0, 2));

    // The session: the same, while it passes.
    if let Err(failure) = mac.prepare(|| {}) {
        let failing = reads_in(&mut mac, |mac| {
            for _ in 0..2 {
                let switched = Cell::new(false);
                assert!(mac.prepare_recent(|| switched.set(true)).is_err());
                assert!(switched.get(), "a failed check releases what is held");
            }
        });
        assert_eq!(failing, (2, 0));
        let _ = writeln!(
            std::io::stderr(),
            "{}: the session check fails here ({failure}): only its failure was checked",
            module_path!()
        );
        return;
    }
    let fresh = reads_in(&mut mac, |mac| {
        mac.prepare(|| {}).unwrap();
        mac.prepare(|| {}).unwrap();
    });
    assert_eq!(fresh, (2, 0));
    let recent = reads_in(&mut mac, |mac| {
        mac.forget_recent();
        mac.prepare(|| {}).unwrap();
        mac.prepare_recent(|| {}).unwrap();
        mac.prepare_recent(|| {}).unwrap();
    });
    assert_eq!(recent, (1, 0));
    let forgot = reads_in(&mut mac, |mac| {
        mac.prepare(|| {}).unwrap();
        mac.forget_recent();
        mac.prepare_recent(|| {}).unwrap();
        mac.prepare_recent(|| {}).unwrap();
    });
    assert_eq!(forgot, (2, 0));
}

/// The WindowServer reads, of the session and of the geometry, that `round`
/// makes of `mac` in a run that takes under `REUSE`: a run a stalled runner
/// stretches past it proves nothing, and runs again.
fn reads_in(mac: &mut desktop::MacDesktop, round: impl Fn(&mut desktop::MacDesktop)) -> (u32, u32) {
    for _ in 0..5 {
        let (before, started) = (mac.reads(), Instant::now());
        round(mac);
        let after = mac.reads();
        if started.elapsed() < REUSE {
            return (after.0 - before.0, after.1 - before.1);
        }
    }
    panic!("five runs each took {REUSE:?} or more");
}

/// What the desktop check around each capture and pointer read costs thread
/// C and WindowServer on this Mac. `PlatformBackend::verify` makes two
/// around each capture and two around each pointer read. Each reads the
/// login's session (`CGSessionCopyCurrentDictionary` and two lookups) and
/// the display's geometry (`CGDisplayBounds`, `CGDisplayCopyDisplayMode`)
/// from WindowServer, unless one that passed within `REUSE` answers
/// (`recent`, as capture and the pointer ask). It prints, for verifies that
/// read each time (`recent=false`) and that may reuse (`recent=true`):
/// - back to back, 10,000 rounds of a capture's two verifies: microseconds
///   per verify on the wall, of this thread's CPU and of WindowServer's
///   (`ps`, to 10 ms), and reads per verify where it counts them;
/// - paced for 10 s as thread C paces a 60 FPS session, two verifies every
///   1/60 s and two every 16 ms for the pointer, after an idle arm that
///   shows WindowServer's own CPU: verifies, verifies over 10 µs and
///   WindowServer reads a second, and milliseconds a second spent in
///   verifies and of this thread's and WindowServer's CPU.
///
/// While the session check passes, the verifies are `MacDesktop`'s. While
/// it fails (a locked screen, or a login away from the console), a failure
/// is never reused, so the verifies are the same reads through the same
/// `Reuse` policy with the session's answer taken as a pass, and
/// `MacDesktop`'s own recent verifies are timed as well, each of which
/// reads. Needs no Screen Recording: `cargo test --release -p
/// pb-mapper-desktop --lib -- --ignored --nocapture session_check`.
#[test]
#[ignore = "a measurement: about 35 seconds of desktop checks, in release mode"]
fn session_check_cost() {
    use crate::{DisplaySelection, platform::Desktop};

    let Ok(display) = desktop::resolve(&DisplaySelection::Primary) else {
        report(format_args!("skipped: this Mac has no active display"));
        return;
    };
    let mut mac = desktop::MacDesktop::open(&display).unwrap();
    let expected = mac.display().unwrap();
    let passing = match mac.prepare(|| {}) {
        Ok(()) => true,
        Err(failure) => {
            report(format_args!("the session check fails here: {failure}"));
            false
        }
    };
    report(format_args!(
        "display {}x{} at ({}, {}) verifies={}",
        expected.width,
        expected.height,
        expected.x,
        expected.y,
        if passing {
            "mac_desktop"
        } else {
            "reads_as_passing"
        },
    ));
    let mut bench = Bench {
        mac,
        expected,
        passing,
        reuse: Reuse::default(),
        reads: 0,
    };
    for recent in [false, true] {
        let label = format!("recent={recent}");
        back_to_back(&mut bench, &label, !passing, |bench| bench.verify(recent));
    }
    if !passing {
        let label = "mac_desktop_failing recent=true";
        back_to_back(&mut bench, label, false, |bench| {
            assert!(!bench.mac_verify(true));
        });
    }
    let wake = Wake::new();
    let waiter = Waiter::new(&wake).unwrap();
    for arm in [None, Some(false), Some(true)] {
        paced(&mut bench, &waiter, arm);
    }
}

/// Rounds of a capture's two verifies, back to back.
const ROUNDS: u32 = 10_000;
/// How long each paced arm runs.
const PACED: Duration = Duration::from_secs(10);
/// A verify slower than this read WindowServer: on the M6 that measured it,
/// a read took tens of microseconds, and a reused answer well under one.
const READ: Duration = Duration::from_micros(10);

/// The verifies `session_check_cost` times.
struct Bench {
    mac: desktop::MacDesktop,
    expected: crate::Display,
    /// Whether the session check passes here.
    passing: bool,
    /// The policy over the reads, for a session check that fails here.
    reuse: Reuse<(u32, super::geometry::Geometry)>,
    /// The WindowServer reads it made through `reuse`, which it counts
    /// while the session check fails here.
    reads: u32,
}
impl Bench {
    /// One verify: `MacDesktop`'s, or its reads through `reuse`.
    fn verify(&mut self, recent: bool) {
        if self.passing {
            assert!(
                self.mac_verify(recent),
                "the desktop changed during the measurement"
            );
            return;
        }
        let (reads, expected) = (&mut self.reads, &self.expected);
        self.reuse
            .session(Instant::now(), recent, || {
                *reads += 1;
                // The read, whatever it answers.
                let _ = desktop::available();
                Ok(())
            })
            .unwrap();
        let fits = self
            .reuse
            .matches(
                Instant::now(),
                recent,
                || {
                    *reads += 1;
                    desktop::geometry(expected.id)
                        .map(|geometry| (expected.id, geometry))
                        .ok_or_else(|| std::io::Error::other("the display is gone"))
                },
                |(id, geometry)| geometry.matches(*id, &expected.name, expected),
            )
            .unwrap();
        assert!(fits, "the display changed during the measurement");
    }

    /// `MacDesktop`'s verify, as `PlatformBackend::verify` makes it: whether
    /// it passed.
    fn mac_verify(&mut self, recent: bool) -> bool {
        use crate::platform::Desktop;
        let (mac, expected) = (&mut self.mac, &self.expected);
        if recent {
            mac.prepare_recent(|| {}).is_ok() && mac.display_matches_recent(expected).unwrap()
        } else {
            mac.prepare(|| {}).is_ok() && mac.display_matches(expected).unwrap()
        }
    }
}

/// `ROUNDS` of two of `verify`, back to back; `counts`: whether `bench`
/// counts their reads.
fn back_to_back(bench: &mut Bench, label: &str, counts: bool, verify: impl Fn(&mut Bench)) {
    let (reads, window_server) = (bench.reads, window_server_cpu());
    let (wall, cpu) = (Instant::now(), thread_cpu());
    for _ in 0..ROUNDS {
        verify(bench);
        verify(bench);
    }
    let (wall, cpu) = (wall.elapsed(), thread_cpu() - cpu);
    let verifies = f64::from(ROUNDS * 2);
    report(format_args!(
        "back_to_back {label} verifies={verifies} wall_us_per_verify={:.3} thread_cpu_us_per_verify={:.3} window_server_cpu_us_per_verify={} reads_per_verify={}",
        wall.as_secs_f64() * 1e6 / verifies,
        cpu.as_secs_f64() * 1e6 / verifies,
        per(window_server, window_server_cpu(), verifies * 1e-6),
        counted(counts.then(|| bench.reads - reads), verifies),
    ));
}

/// One paced arm: `bench.verify(recent)` for `Some(recent)`, else nothing.
fn paced(bench: &mut Bench, waiter: &Waiter, arm: Option<bool>) {
    let capture = Duration::from_secs(1) / 60;
    let pointer = Duration::from_millis(16);
    let (reads, window_server) = (bench.reads, window_server_cpu());
    let (start, cpu) = (Instant::now(), thread_cpu());
    let (mut next_capture, mut next_pointer) = (start, start);
    let (mut verifies, mut slow, mut in_verify) = (0u32, 0u32, Duration::ZERO);
    loop {
        let now = Instant::now();
        if now >= start + PACED {
            break;
        }
        let mut rounds = 0;
        if now >= next_capture {
            next_capture = (next_capture + capture).max(now);
            rounds += 1;
        }
        if now >= next_pointer {
            next_pointer = (next_pointer + pointer).max(now);
            rounds += 1;
        }
        if let Some(recent) = arm {
            for _ in 0..rounds * 2 {
                let at = Instant::now();
                bench.verify(recent);
                let took = at.elapsed();
                in_verify += took;
                verifies += 1;
                slow += u32::from(took > READ);
            }
        }
        waiter.wait(next_capture.min(next_pointer)).unwrap();
    }
    let (wall, cpu) = (start.elapsed().as_secs_f64(), thread_cpu() - cpu);
    let label = arm.map_or_else(|| "idle".to_owned(), |recent| format!("recent={recent}"));
    report(format_args!(
        "paced {label} seconds={wall:.2} verifies_per_s={:.1} over_10us_per_s={:.1} reads_per_s={} verify_wall_ms_per_s={:.3} thread_cpu_ms_per_s={:.3} window_server_cpu_ms_per_s={}",
        f64::from(verifies) / wall,
        f64::from(slow) / wall,
        counted((!bench.passing).then(|| bench.reads - reads), wall),
        in_verify.as_secs_f64() * 1e3 / wall,
        cpu.as_secs_f64() * 1e3 / wall,
        per(window_server, window_server_cpu(), wall * 1e-3),
    ));
}

/// `reads` per `count`, where they were counted.
fn counted(reads: Option<u32>, count: f64) -> String {
    reads.map_or_else(
        || "uncounted".into(),
        |reads| format!("{:.3}", f64::from(reads) / count),
    )
}

/// This thread's CPU time.
fn thread_cpu() -> Duration {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a local the call fills.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut now) },
        0
    );
    Duration::new(now.tv_sec as u64, now.tv_nsec as u32)
}

/// WindowServer's CPU time, as `ps` reads it (to 10 ms), where it can.
fn window_server_cpu() -> Option<Duration> {
    let pid = std::process::Command::new("pgrep")
        .args(["-x", "WindowServer"])
        .output()
        .ok()?;
    let pid = String::from_utf8(pid.stdout).ok()?;
    let time = std::process::Command::new("ps")
        .args(["-o", "time=", "-p", pid.trim()])
        .output()
        .ok()?;
    let time = String::from_utf8(time.stdout).ok()?;
    let seconds = time.trim().split(':').try_fold(0.0, |total: f64, part| {
        Some(total * 60.0 + part.parse::<f64>().ok()?)
    })?;
    Some(Duration::from_secs_f64(seconds))
}

/// `after - before`, in seconds per `count`, or `unread`.
fn per(before: Option<Duration>, after: Option<Duration>, count: f64) -> String {
    match (before, after) {
        (Some(before), Some(after)) => {
            format!("{:.3}", after.saturating_sub(before).as_secs_f64() / count)
        }
        _ => "unread".into(),
    }
}
