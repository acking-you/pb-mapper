//! Capture and encode fused, as thread C sees them: a ScreenCaptureKit
//! stream scaled to the encoder's size, and a VideoToolbox session that
//! reads its pictures in place. Thread C paces: each submit encodes the
//! newest picture, or the last one again every `LIVENESS_INTERVAL` while
//! the screen stands still, or at the next tick for a keyframe or for a
//! last picture VideoToolbox dropped (`codec::resubmit`). A submit only hands
//! the picture to the encoder's submit thread, which makes the VideoToolbox
//! call; at most `VT_IN_FLIGHT` units are in flight, and a submit that finds
//! the submit thread still busy leaves its tick armed until that thread
//! wakes thread C. The encoder pushes its output and wakes thread C
//! (`DRAIN = None`). A new picture is dated by when it reached the stream's
//! queue, which thread C anchors its pacing on.
//! A display that sleeps gives the stream nothing: an open, the first
//! submit after a resume and a stream with no picture light it
//! (`power.rs`) and fail, and thread C's retry opens a stream once it draws.
//! A stream whose display turned off under it repeats its last picture
//! until the display has been dark for `wake::SETTLE`, then does the same.

use super::{
    capture::{CaptureState, Counts, Stream},
    encoder::{self, Admit, Encoder},
    platform::ScreenAccess,
    power,
    wake::{SETTLE, SleepWatch},
};
use crate::{
    BackendError, Display, EncodedFrame, FailureKind, Presented, Result, VideoCodec, VideoLimits,
    VideoRate, Waker,
    codec::{
        pipeline::{FIRST_SUBMIT_WAIT, LATE_OUTPUT, LIVENESS_INTERVAL},
        resubmit::Unit,
    },
    error, failure_kind,
    platform::VideoSource,
    qos::{ENCODER_BITRATE_MAX, ENCODER_BITRATE_MIN, scaled_size},
    worker::ENCODER_DEPTH,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetExtendedPixels,
    CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidth,
};
use std::{
    ffi::c_void,
    time::{Duration, Instant},
};

/// Units VideoToolbox may hold at once, the mailbox's among them; thread C
/// skips a tick that finds this many (`full`). Where a session encodes
/// asynchronously (a size that is not a multiple of 16, such as a MacBook's
/// built-in display at its default resolution) and the pace outruns it,
/// each unit admitted waits behind the others: on an M6 at a pace of 120,
/// a 3024x1964 session's units took 41 ms (H.264) and 39 ms (H.265) from
/// submit to output, at a median, with four in flight, and 18 and 17 ms
/// with two, at the same frame rate (`video_toolbox_in_flight`). Where it
/// encodes inside the call, one unit is in flight at a time anyway.
pub(super) const VT_IN_FLIGHT: usize = 2;
const _: () = assert!(
    VT_IN_FLIGHT + 1 < ENCODER_DEPTH,
    "thread C's own gate, which also counts units out and not yet drained, leaves room beside this one"
);

// The surface behind a pixel buffer (CoreVideo's, not retained), and its
// size (IOSurface's), for `macos_capture_layout`.
#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferGetIOSurface(pixel_buffer: &CVPixelBuffer) -> *const c_void;
}
#[link(name = "IOSurface", kind = "framework")]
unsafe extern "C" {
    fn IOSurfaceGetAllocSize(buffer: *const c_void) -> usize;
}

/// The first picture's layout, as a debug line on
/// `pb_mapper_desktop::session` (`macos_capture_layout`): its size, the
/// pixels it says it extends by on each side, each plane's rows and bytes
/// per row, and its surface's size. They say whether ScreenCaptureKit pads
/// its pictures to a multiple of 16, which VideoToolbox would read.
fn trace_layout(picture: &CVPixelBuffer) {
    if !tracing::enabled!(target: "pb_mapper_desktop::session", tracing::Level::DEBUG) {
        return;
    }
    let (mut left, mut right, mut top, mut bottom) = (0usize, 0usize, 0usize, 0usize);
    // SAFETY: out pointers to locals; the picture is planar NV12.
    unsafe {
        CVPixelBufferGetExtendedPixels(picture, &mut left, &mut right, &mut top, &mut bottom)
    };
    // SAFETY: the picture's own surface, null when it has none, alive while
    // the picture is.
    let surface = unsafe { CVPixelBufferGetIOSurface(picture) };
    // SAFETY: a live surface.
    let surface_bytes = (!surface.is_null()).then(|| unsafe { IOSurfaceGetAllocSize(surface) });
    tracing::debug!(
        target: "pb_mapper_desktop::session",
        event = "macos_capture_layout",
        width = CVPixelBufferGetWidth(picture),
        height = CVPixelBufferGetHeight(picture),
        extended_left = left,
        extended_right = right,
        extended_top = top,
        extended_bottom = bottom,
        luma_rows = CVPixelBufferGetHeightOfPlane(picture, 0),
        luma_bytes_per_row = CVPixelBufferGetBytesPerRowOfPlane(picture, 0),
        chroma_rows = CVPixelBufferGetHeightOfPlane(picture, 1),
        chroma_bytes_per_row = CVPixelBufferGetBytesPerRowOfPlane(picture, 1),
        surface_bytes,
    );
}

/// The capture stops before the encoder goes.
pub(crate) struct MacVideo {
    stream: Stream,
    encoder: Encoder,
    /// The captured display's id.
    display: u32,
    /// Resumed since the last submit, which then checks that the display
    /// did not fall asleep during the pause.
    resumed: bool,
    /// What the stream found of the display's sleep, and when it last
    /// found anything: a new picture, or a look.
    watch: SleepWatch,
    looked: Instant,
    created: Instant,
    last_submit: Option<Instant>,
    /// Asked for and not yet submitted.
    keyframe: bool,
    /// The last picture again, as a new one: VideoToolbox dropped the
    /// newest unit. Taken from the encoder, and not yet submitted.
    resubmit: bool,
    /// The last submit's new picture: when it arrived, and the pictures it
    /// covers.
    presented: Option<Presented>,
    timing: Timing,
}

/// How often a stream with no new picture looks whether its display fell
/// asleep, at a repeat of its last picture, which a still screen submits
/// every `LIVENESS_INTERVAL`.
const ASLEEP_LOOK: Duration = Duration::from_millis(500);
const _: () = assert!(
    ASLEEP_LOOK.as_millis() + LIVENESS_INTERVAL.as_millis() < SETTLE.as_millis(),
    "a display that turns off under a still screen is found awake within SETTLE before"
);

/// Where `macos_capture_timing` goes: with thread C's pacing.
const TIMING_TARGET: &str = "pb_mapper_desktop::feedback";
/// Display-to-arrival delays kept per second.
const TIMING_SAMPLES: usize = 256;

/// The stream's once-a-second debug figures, `macos_capture_timing`:
/// ScreenCaptureKit's complete pictures, idle samples and pictures a newer
/// one replaced before thread C took them, per second, and how long after
/// the window server displayed a picture thread C took it reached the
/// stream's queue (p50 and p95). Nothing allocated.
struct Timing {
    started: Instant,
    counts: Counts,
    delays: [u32; TIMING_SAMPLES],
    len: usize,
}
impl Timing {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            counts: Counts::default(),
            delays: [0; TIMING_SAMPLES],
            len: 0,
        }
    }

    /// A new picture thread C took, which arrived `displayed` after the
    /// window server displayed it.
    fn took(&mut self, displayed: Option<Duration>) {
        let Some(displayed) = displayed else {
            return;
        };
        if let Some(slot) = self.delays.get_mut(self.len) {
            *slot = u32::try_from(displayed.as_micros()).unwrap_or(u32::MAX);
            self.len += 1;
        }
    }

    fn roll(&mut self, now: Instant, stream: &Stream) {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let counts = stream.counts();
        if tracing::enabled!(target: TIMING_TARGET, tracing::Level::DEBUG) {
            let delays = self.delays.get_mut(..self.len).unwrap_or_default();
            delays.sort_unstable();
            let at = |quantile: usize| {
                delays
                    .get((delays.len() * quantile / 100).min(delays.len().saturating_sub(1)))
                    .copied()
            };
            let per_second =
                |now: u64, then: u64| now.saturating_sub(then) as f64 / elapsed.as_secs_f64();
            tracing::debug!(
                target: TIMING_TARGET,
                event = "macos_capture_timing",
                complete_per_s = per_second(counts.complete, self.counts.complete),
                idle_per_s = per_second(counts.idle, self.counts.idle),
                replaced_per_s = per_second(counts.replaced, self.counts.replaced),
                display_to_arrival_p50_us = at(50),
                display_to_arrival_p95_us = at(95),
            );
        }
        self.counts = counts;
        self.started = now;
        self.len = 0;
    }
}

fn rate_bounds(bitrate: u32) -> Result<()> {
    if (ENCODER_BITRATE_MIN..=ENCODER_BITRATE_MAX).contains(&bitrate) {
        Ok(())
    } else {
        Err(error("video bitrate outside supported bounds"))
    }
}

impl VideoSource for MacVideo {
    type Grant = ScreenAccess;
    const DRAIN: Option<Duration> = None;
    fn encodes(_: &Display, codec: VideoCodec) -> bool {
        encoder::encodes(codec)
    }
    /// The display fitted inside the viewer's limits, its aspect kept,
    /// never upscaled; the encoder first, whose failure is the codec's. A
    /// sleeping display is lit instead, and the open fails until it draws.
    fn open(
        _: &ScreenAccess,
        display: &Display,
        codec: VideoCodec,
        limits: VideoLimits,
        rate: VideoRate,
        waker: &Waker,
    ) -> Result<Self> {
        rate_bounds(rate.bitrate)?;
        // Thread C checked the session just before: this login owns the
        // screen, unlocked.
        if power::wake([display.id]) {
            return Err(power::waking());
        }
        let (width, height) = scaled_size(
            display.width,
            display.height,
            limits.max_width,
            limits.max_height,
        );
        let encoder = Encoder::new(codec, width, height, limits.max_fps, rate.bitrate, waker)?;
        let stream =
            Stream::start(display.id, width, height, limits.max_fps, waker).map_err(|failure| {
                // ScreenCaptureKit may not list a display that fell asleep.
                if failure_kind(&failure) == Some(FailureKind::DisplayLost)
                    && power::wake([display.id])
                {
                    power::waking()
                } else {
                    failure
                }
            })?;
        let now = Instant::now();
        Ok(Self {
            stream,
            encoder,
            display: display.id,
            resumed: false,
            watch: SleepWatch::default(),
            looked: now,
            created: now,
            last_submit: None,
            keyframe: true,
            resubmit: false,
            presented: None,
            timing: Timing::new(now),
        })
    }
    fn codec(&self) -> VideoCodec {
        self.encoder.codec()
    }
    fn set_rate(&mut self, rate: VideoRate) -> Result<()> {
        rate_bounds(rate.bitrate)?;
        self.encoder.set_bitrate(rate.bitrate);
        Ok(())
    }
    /// Never waits: nothing captured yet, a full encoder or a still screen
    /// inside the liveness interval submit nothing; a keyframe asked for
    /// stays asked for until a submit carries it, and so does a resubmit of
    /// the last picture, which goes as a new picture (not a duplicate: the
    /// viewer never had it) unless a newer one came. Thread C skips a tick
    /// while `VT_IN_FLIGHT` units are in flight (`full`), and a submit that
    /// finds them anyway spends its tick; an encoder whose submit thread is
    /// still busy leaves it armed, and that thread wakes thread C once it is
    /// free, so the newest picture goes then. A display that slept through
    /// a pause, that never gave a picture because it sleeps, or that turned
    /// off under the stream and stayed dark for `SETTLE`, is lit, and the
    /// submit fails so that thread C opens a new stream once it draws.
    fn submit(&mut self, keyframe: bool) -> Result<bool> {
        self.presented = None;
        // Thread C checked the session just before, as for `open`.
        if std::mem::take(&mut self.resumed) && power::wake([self.display]) {
            return Err(power::waking());
        }
        self.keyframe |= keyframe | self.encoder.keyframe_due();
        self.resubmit |= self.encoder.take_resubmit();
        match self.encoder.admit(VT_IN_FLIGHT) {
            Admit::Full => return Ok(true),
            Admit::Busy => return Ok(false),
            Admit::Free => {}
        }
        let now = Instant::now();
        let repeat = self.keyframe
            || self.resubmit
            || self
                .last_submit
                .is_some_and(|last| now >= last + LIVENESS_INTERVAL);
        let Some(taken) = self.stream.take(repeat)? else {
            if self.last_submit.is_none() && now >= self.created + FIRST_SUBMIT_WAIT {
                if power::wake([self.display]) {
                    return Err(power::waking());
                }
                return Err(BackendError::new(
                    FailureKind::CaptureFailed,
                    "ScreenCaptureKit delivered no picture",
                )
                .into());
            }
            return Ok(false);
        };
        if taken.fresh.is_some() {
            self.watch.awake(now);
            self.looked = now;
        } else if now >= self.looked + ASLEEP_LOOK {
            // A display put to sleep sends the stream nothing more, and its
            // last picture would repeat for good.
            self.looked = now;
            if !power::sleeping(self.display) {
                self.watch.awake(now);
            } else if self.watch.asleep(now) {
                power::light();
                return Err(power::waking());
            }
        }
        if self.last_submit.is_none() {
            trace_layout(&taken.picture);
        }
        let unit = Unit::of(taken.fresh.is_some(), self.resubmit);
        self.encoder.encode(&taken.picture, self.keyframe, unit)?;
        self.keyframe = false;
        self.resubmit = false;
        self.last_submit = Some(now);
        if let Some(arrival) = taken.fresh {
            self.presented = Some(Presented::new(arrival.at, arrival.pictures));
            self.timing.took(arrival.displayed);
        }
        self.timing.roll(now, &self.stream);
        Ok(true)
    }
    /// `VT_IN_FLIGHT` units in flight: thread C skips the tick and counts it
    /// in `skipped_ticks`, and the encoder in `macos_encoder_load`'s
    /// `gate_ticks`.
    fn full(&self) -> bool {
        self.encoder.full(VT_IN_FLIGHT)
    }
    /// A new picture, dated by its arrival on the stream's queue.
    fn presented(&self) -> Option<Presented> {
        self.presented
    }
    fn idle_until(&self, keyframe_due: Option<Instant>) -> Option<Instant> {
        if self.resumed {
            return None;
        }
        // A submit found the encoder's submit thread busy: that thread
        // wakes thread C once it is free. Nothing is due before then, and
        // `None`, due now, would spin thread C, which never polls this push
        // source.
        if self.encoder.waiting() {
            return Some(Instant::now() + LATE_OUTPUT);
        }
        match self.stream.state() {
            CaptureState::Ready => None,
            CaptureState::Waiting => Some(self.created + FIRST_SUBMIT_WAIT),
            CaptureState::Repeatable
                if self.keyframe
                    || self.resubmit
                    || self.encoder.wants_keyframe()
                    || self.encoder.wants_resubmit() =>
            {
                None
            }
            CaptureState::Repeatable => self
                .last_submit
                .map(|at| at + LIVENESS_INTERVAL)
                .map(|until| keyframe_due.map_or(until, |due| due.min(until))),
        }
    }
    fn drain(&mut self) -> Result<Option<EncodedFrame>> {
        self.stream.check()?;
        self.encoder.drain()
    }
    fn pending(&self) -> usize {
        // A stopped stream needs to be drained even while paused, when no
        // other callback is necessarily waiting to be collected.
        self.encoder.pending() + usize::from(self.stream.stopped())
    }
    /// The display may sleep while every viewer is paused, which releases
    /// the session's activity: the next submit looks.
    fn paused(&mut self, paused: bool) {
        self.resumed = !paused;
    }
}
