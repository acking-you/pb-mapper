//! What runs on a Mac without a person: the encoder, and the displays and
//! modes as Core Graphics lists them. Capture and injection need the
//! owner's Screen Recording and Accessibility grants, which no CI runner
//! has; the manual checklist covers them.
#![cfg(target_os = "macos")]

use super::{desktop, encoder::Encoder, modes::MacModes};
use crate::{
    VideoCodec, Waker,
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
/// between them (an unbounded GOP); the output wakes thread C.
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
    for (frame, keyframe) in asked.into_iter().enumerate() {
        encoder.encode(&picture, keyframe, frame > 0).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let unit = loop {
            if let Some(unit) = encoder.drain().unwrap() {
                break unit;
            }
            assert!(Instant::now() < deadline, "no output for frame {frame}");
            waiter.wait(deadline).unwrap();
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
