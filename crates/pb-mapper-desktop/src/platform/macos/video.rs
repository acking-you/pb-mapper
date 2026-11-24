//! Capture and encode fused, as thread C sees them: a ScreenCaptureKit
//! stream scaled to the encoder's size, and a VideoToolbox session that
//! reads its pictures in place. Thread C paces: each submit encodes the
//! newest picture, or the last one again every `LIVENESS_INTERVAL` while
//! the screen stands still, or at once for a keyframe. The encoder pushes
//! its output and wakes thread C (`DRAIN = None`). A new picture is dated by
//! when it reached the stream's queue, which thread C anchors its pacing on.

use super::{
    capture::{CaptureState, Counts, Stream},
    encoder::{self, Encoder},
    platform::ScreenAccess,
};
use crate::{
    BackendError, Display, EncodedFrame, FailureKind, Presented, Result, VideoCodec, VideoLimits,
    VideoRate, Waker,
    codec::pipeline::{FIRST_SUBMIT_WAIT, LIVENESS_INTERVAL},
    error,
    platform::VideoSource,
    qos::{ENCODER_BITRATE_MAX, ENCODER_BITRATE_MIN, scaled_size},
    worker::ENCODER_DEPTH,
};
use std::time::{Duration, Instant};

/// The capture stops before the encoder goes.
pub(crate) struct MacVideo {
    stream: Stream,
    encoder: Encoder,
    created: Instant,
    last_submit: Option<Instant>,
    /// Asked for and not yet submitted.
    keyframe: bool,
    /// The last submit's new picture: when it arrived, and the pictures it
    /// covers.
    presented: Option<Presented>,
    timing: Timing,
}

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
    /// never upscaled; the encoder first, whose failure is the codec's.
    fn open(
        _: &ScreenAccess,
        display: &Display,
        codec: VideoCodec,
        limits: VideoLimits,
        rate: VideoRate,
        waker: &Waker,
    ) -> Result<Self> {
        rate_bounds(rate.bitrate)?;
        let (width, height) = scaled_size(
            display.width,
            display.height,
            limits.max_width,
            limits.max_height,
        );
        let encoder = Encoder::new(codec, width, height, limits.max_fps, rate.bitrate, waker)?;
        let stream = Stream::start(display.id, width, height, limits.max_fps, waker)?;
        let now = Instant::now();
        Ok(Self {
            stream,
            encoder,
            created: now,
            last_submit: None,
            keyframe: true,
            presented: None,
            timing: Timing::new(now),
        })
    }
    fn codec(&self) -> VideoCodec {
        self.encoder.codec()
    }
    fn set_rate(&mut self, rate: VideoRate) -> Result<()> {
        rate_bounds(rate.bitrate)?;
        self.encoder.set_bitrate(rate.bitrate)
    }
    /// Never waits: nothing captured yet, a full encoder or a still screen
    /// inside the liveness interval submit nothing; a keyframe asked for
    /// stays asked for until a submit carries it.
    fn submit(&mut self, keyframe: bool) -> Result<bool> {
        self.presented = None;
        self.keyframe |= keyframe | self.encoder.keyframe_due();
        if self.encoder.pending() >= ENCODER_DEPTH {
            return Ok(true);
        }
        let now = Instant::now();
        let repeat = self.keyframe
            || self
                .last_submit
                .is_some_and(|last| now >= last + LIVENESS_INTERVAL);
        let Some(taken) = self.stream.take(repeat)? else {
            if self.last_submit.is_none() && now >= self.created + FIRST_SUBMIT_WAIT {
                return Err(BackendError::new(
                    FailureKind::CaptureFailed,
                    "ScreenCaptureKit delivered no picture",
                )
                .into());
            }
            return Ok(false);
        };
        self.encoder
            .encode(&taken.picture, self.keyframe, taken.fresh.is_none())?;
        self.keyframe = false;
        self.last_submit = Some(now);
        if let Some(arrival) = taken.fresh {
            self.presented = Some(Presented::new(arrival.at, arrival.pictures));
            self.timing.took(arrival.displayed);
        }
        self.timing.roll(now, &self.stream);
        Ok(true)
    }
    /// A new picture, dated by its arrival on the stream's queue.
    fn presented(&self) -> Option<Presented> {
        self.presented
    }
    fn idle_until(&self, keyframe_due: Option<Instant>) -> Option<Instant> {
        match self.stream.state() {
            CaptureState::Ready => None,
            CaptureState::Waiting => Some(self.created + FIRST_SUBMIT_WAIT),
            CaptureState::Repeatable if self.keyframe || self.encoder.wants_keyframe() => None,
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
}
