//! What runs on a Mac without a person: the encoder, and the displays and
//! modes as Core Graphics lists them. Capture and injection need the
//! owner's Screen Recording and Accessibility grants, which no CI runner
//! has; the manual checklist covers them.
#![cfg(target_os = "macos")]

use super::{desktop, encoder::Encoder, layout, modes::MacModes};
use crate::{
    Key, VideoCodec, Waker,
    codec::annexb::{AccessUnit, ParameterSets},
    platform::DisplayModes,
    scheduler::{Waiter, Wake},
};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
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
