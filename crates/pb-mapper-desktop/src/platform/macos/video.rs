//! Capture and encode fused, as thread C sees them: a ScreenCaptureKit
//! stream scaled to the encoder's size, and a VideoToolbox session that
//! reads its pictures in place. Thread C paces: each submit encodes the
//! newest picture, or the last one again every `LIVENESS_INTERVAL` while
//! the screen stands still, or at once for a keyframe. The encoder pushes
//! its output and wakes thread C (`DRAIN = None`).

use super::{
    capture::{CaptureState, Stream},
    encoder::{self, Encoder},
    platform::ScreenAccess,
};
use crate::{
    BackendError, Display, EncodedFrame, FailureKind, Result, VideoCodec, VideoLimits, VideoRate,
    Waker,
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
    /// VideoToolbox encodes on its own threads, in hardware or software.
    const ENCODES_OFF_THREAD: bool = true;
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
        Ok(Self {
            stream,
            encoder,
            created: Instant::now(),
            last_submit: None,
            keyframe: true,
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
        self.keyframe |= keyframe | self.encoder.keyframe_due();
        if self.encoder.pending() >= ENCODER_DEPTH {
            return Ok(true);
        }
        let now = Instant::now();
        let repeat = self.keyframe
            || self
                .last_submit
                .is_some_and(|last| now >= last + LIVENESS_INTERVAL);
        let Some((picture, fresh)) = self.stream.take(repeat)? else {
            if self.last_submit.is_none() && now >= self.created + FIRST_SUBMIT_WAIT {
                return Err(BackendError::new(
                    FailureKind::CaptureFailed,
                    "ScreenCaptureKit delivered no picture",
                )
                .into());
            }
            return Ok(false);
        };
        self.encoder.encode(&picture, self.keyframe, !fresh)?;
        self.keyframe = false;
        self.last_submit = Some(now);
        Ok(true)
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
