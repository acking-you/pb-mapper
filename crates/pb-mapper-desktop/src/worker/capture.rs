//! Thread C's capture: the desktop context, the encoder and its output.

use super::{CONTEXT_CHECK_INTERVAL, Clock, ENCODER_DEPTH, Engine, H265_FAILED, TRANSPORT_GATE};
use crate::{
    Backend, EncodedFrame, FailureKind, Result, VideoCodec, VideoRate, error, failure_kind,
    idr::IdrReason,
    outgoing::{HostUnit, Push},
    protocol::{StateReason, VideoSize},
};
use std::{
    io,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const UNAVAILABLE_RETRY: Duration = Duration::from_millis(200);
/// The longest the helper may block one drain call for encoder output.
const HELPER_DRAIN_WAIT_MAX: Duration = Duration::from_millis(4);

impl<B: Backend, K: Clock> Engine<B, K> {
    /// A context check is due: there is no context yet, capture is
    /// unavailable, or `context_due` has passed.
    pub(super) fn check_due(&self, now: Instant) -> bool {
        self.context.is_none() || self.unavailable.is_some() || now >= self.context_due
    }

    /// Check the desktop identity. A new context or a recovery rebuilds the
    /// encoder once; a new context also starts a new generation, while the
    /// failure a recovery follows already started one if anything had been
    /// produced. False while unavailable.
    pub(super) fn geometry(&mut self, now: Instant) -> bool {
        let context = match self.backend.context() {
            Ok(context) => context,
            Err(failure) => {
                self.fail(failure, now);
                return false;
            }
        };
        self.context_due = now + CONTEXT_CHECK_INTERVAL;
        let recovered = self.unavailable.is_some();
        let changed = self.context.as_ref() != Some(&context);
        if !recovered && !changed {
            return true;
        }
        if self.context.is_some() || recovered {
            if changed && self.context.is_some() {
                self.invalidate();
            }
            self.keyframe = true;
            if let Err(failure) = self.backend.configure_video(self.limits, self.nominal_fps) {
                self.fail(failure, now);
                return false;
            }
        }
        if changed {
            // On a context change only, never per frame: the owner's own
            // change reaches the viewer too.
            self.read_modes(&context);
        }
        self.state.record.display = Some(context.display.clone());
        self.state.record.desktop = context.desktop;
        self.context = Some(context);
        self.unavailable = None;
        self.streaming = false;
        true
    }

    /// Capture is unavailable until a retry succeeds. A failure that ends
    /// streaming may be a desktop switch: it starts a new generation and is
    /// retried at once. Any other, such as a capture failing again right
    /// after a recovery, is retried every `UNAVAILABLE_RETRY`, never at the
    /// capture pace. A sticky link failure ends the session instead, and so
    /// does an H.265 encoder that would not build or encode: H.265 is then
    /// off for the rest of the process, and the viewer reconnects to H.264.
    pub(super) fn fail(&mut self, failure: io::Error, now: Instant) {
        let message = failure.to_string();
        if failure.kind() == io::ErrorKind::NotConnected {
            self.fatal = Some(message);
        } else if self.codec == VideoCodec::H265
            && failure_kind(&failure)
                == Some(FailureKind::EncoderFailed {
                    codec: VideoCodec::H265,
                })
        {
            H265_FAILED.store(true, Ordering::Release);
            tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "desktop_h265_failed",
                detail = ?message,
            );
            self.fatal = Some(message);
        }
        self.context_due = if self.unavailable.is_none() && self.streaming {
            self.invalidate();
            now
        } else {
            now + UNAVAILABLE_RETRY
        };
        // A mode change costs the capture its device for a moment.
        let reason = if self.settle_until.is_some_and(|until| now < until) {
            StateReason::DisplayChanging
        } else {
            unavailable_reason(&failure)
        };
        self.unavailable = Some((reason, failure.to_string()));
    }

    /// Capturing, not paused, with a frame produced in this generation.
    pub(super) fn live(&self) -> bool {
        !self.paused && self.unavailable.is_none() && self.streaming
    }

    /// When to poll the encoder again after `at`. A push source wakes thread
    /// C itself when it has output, so it is never polled sooner than the
    /// next context check.
    fn drain_after(&self, at: Instant) -> Instant {
        at + self.backend.drain_poll().unwrap_or(CONTEXT_CHECK_INTERVAL)
    }

    /// At most one backend capture or drain call.
    pub(super) fn capture_step(&mut self, now: Instant) {
        let shared = self.shared.clone();
        let targets = &shared.targets;
        if targets.paused.load(Ordering::Acquire) {
            // Never send while paused, and resume with an IDR.
            self.paused = true;
            if self.backend.pending_video() > 0
                && let Some(context) = &self.context
            {
                let result = self.backend.drain_video(context, Duration::ZERO);
                self.drain_due = self.drain_after(self.clock.now());
                match result {
                    Ok(Some(frame)) => {
                        self.window.dwell(frame.format.encode_us);
                        self.state.dropped_frames += 1;
                    }
                    Ok(None) => {}
                    Err(failure) => self.fail(failure, now),
                }
            }
            self.state.dropped_frames += self.shared.outgoing.clear() as u64;
            self.keyframe = true;
            return;
        }
        self.paused = false;
        // A retry checks the context and then captures at once, so a failure
        // that persists never shows as recovered in between.
        let retry = self.unavailable.is_some();
        if retry && now < self.context_due {
            return;
        }
        if self.check_due(now) && !self.geometry(now) {
            return;
        }
        let Some(context) = self.context.clone() else {
            return;
        };
        let pace_fps = targets.pace_fps.load(Ordering::Acquire).max(1);
        if pace_fps != self.pace_fps {
            self.pace_fps = pace_fps;
            self.pacer.set_pace(pace_fps, now);
        }
        if retry || self.pacer.due(now) {
            if self.shared.outgoing.len() < TRANSPORT_GATE
                && self.backend.pending_video() < ENCODER_DEPTH
            {
                let keyframe = self.keyframe || self.shared.idr.lock().take_due(now);
                let rate = VideoRate {
                    bitrate: targets.encoder_bps.load(Ordering::Acquire),
                    pace_fps,
                };
                let result = self
                    .backend
                    .capture_video(&context, self.codec, keyframe, rate);
                if keyframe {
                    // Handed over: an IDR still in the encoder at the next
                    // tick must not be asked for again.
                    self.keyframe = false;
                    self.shared.idr.lock().answered(now);
                }
                let done = self.clock.now();
                self.drain_due = self.drain_after(done);
                self.produced(result, done);
            } else {
                // Transport backpressure or a full encoder, never
                // presentation: skip the tick.
                self.state.skipped_ticks += 1;
            }
            self.pacer.consume(now);
        } else if self.backend.pending_video() > 0 {
            let wait = HELPER_DRAIN_WAIT_MAX.min(self.pacer.next().saturating_duration_since(now));
            let result = self.backend.drain_video(&context, wait);
            let done = self.clock.now();
            let drained = matches!(result, Ok(Some(_)));
            // The ordinary backend returns at once: poll it again shortly. A
            // helper that waited the whole time is polled again at once, and so
            // is a backend that just returned a unit.
            self.drain_due =
                if drained || (!wait.is_zero() && done.saturating_duration_since(now) >= wait) {
                    done
                } else {
                    self.drain_after(done)
                };
            self.produced(result, done);
        }
    }

    fn produced(&mut self, result: Result<Option<EncodedFrame>>, at: Instant) {
        let frame = match result {
            Ok(None) => return,
            Ok(Some(frame))
                if frame.bytes.is_empty() || frame.bytes.len() > crate::wire::MAX_PAYLOAD =>
            {
                return self.fail(error("encoded frame exceeds the transport limit"), at);
            }
            Ok(Some(frame)) => frame,
            Err(failure) => return self.fail(failure, at),
        };
        let format = frame.format;
        self.window.dwell(format.encode_us);
        if format.keyframe {
            self.shared.idr.lock().answered(at);
        }
        self.frame_id += 1;
        self.shared.auth.lock().on_produced(self.frame_id, at);
        if !format.duplicate {
            self.window.unique += 1;
        }
        self.state.record.video = Some(VideoSize {
            width: format.width,
            height: format.height,
        });
        self.state.gop = format.gop;
        self.streaming = true;
        // B-send writes the `State` of a generation before its first unit:
        // publish it before the unit can be taken.
        self.publish();
        let pushed = self.shared.outgoing.push(HostUnit {
            generation: self.state.record.generation,
            frame_id: self.frame_id,
            codec: format.codec,
            width: format.width,
            height: format.height,
            keyframe: format.keyframe,
            duplicate: format.duplicate,
            pts_us: format.pts_us,
            encode_us: format.encode_us,
            ready_at: at,
            bytes: bytes::Bytes::from(frame.bytes),
        });
        if let Push::Overflow { dropped } = pushed {
            self.state.dropped_frames += dropped as u64;
            self.shared.idr.lock().request(IdrReason::HostResync);
        }
    }
}

/// A capture failure's reason for the viewer.
pub(super) fn unavailable_reason(failure: &io::Error) -> StateReason {
    match failure_kind(failure) {
        Some(FailureKind::SecureDesktop) => StateReason::SecureDesktop,
        Some(FailureKind::DisplayLost) => StateReason::DisplayLost,
        _ => StateReason::CaptureFailed,
    }
}
