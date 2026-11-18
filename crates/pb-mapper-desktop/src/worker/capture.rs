//! Thread C's capture: the desktop context, the encoder and its output.

use super::{
    CONTEXT_CHECK_INTERVAL, Clock, ENCODER_DEPTH, Engine, H265_FAILED, TRANSPORT_GATE,
    capture_consent_ended, link_lost,
};
use crate::{
    Backend, EncodedFrame, FailureKind, Result, VideoCodec, VideoLimits, VideoRate, error,
    failure_kind,
    idr::IdrReason,
    outgoing::{FrameBytes, HostUnit, Push},
    protocol::{MAX_VIEWERS, StateReason, VideoSize},
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
                self.invalidate_all();
            }
            self.keyframe = true;
            if let Err(failure) = self
                .backend
                .configure_video(self.limits, self.limits.max_fps)
            {
                self.fail(failure, now);
                return false;
            }
        }
        if changed {
            // On a context change only, never per frame: the owner's own
            // change reaches the viewer too.
            self.read_modes(&context);
        }
        for slot in self.slots.iter_mut().flatten() {
            slot.state.record.display = Some(context.display.clone());
            slot.state.record.desktop = context.desktop;
            slot.streaming = false;
        }
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
    /// Withdrawing capture consent ends the session without an automatic reconnect.
    pub(super) fn fail(&mut self, failure: io::Error, now: Instant) {
        if link_lost(&failure) || capture_consent_ended(&failure) {
            self.fatal = Some(failure);
            return;
        }
        if self.codec == VideoCodec::H265
            && failure_kind(&failure)
                == Some(FailureKind::EncoderFailed {
                    codec: VideoCodec::H265,
                })
        {
            H265_FAILED.store(true, Ordering::Release);
            tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "desktop_h265_failed",
                detail = %failure,
            );
            self.fatal = Some(failure);
            return;
        }
        self.context_due = if self.unavailable.is_none() && self.streaming {
            self.invalidate_all();
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

    /// Capturing, not paused, with a frame produced for `idx` in this generation.
    pub(super) fn slot_live(&self, idx: usize) -> bool {
        self.unavailable.is_none()
            && self.slots[idx]
                .as_ref()
                .is_some_and(|slot| !slot.paused && slot.streaming)
    }

    /// When to poll the encoder again after `at`. A push source wakes thread
    /// C itself when it has output, so it is never polled sooner than the
    /// next context check.
    fn drain_after(&self, at: Instant) -> Instant {
        at + self.backend.drain_poll().unwrap_or(CONTEXT_CHECK_INTERVAL)
    }

    /// At most one backend capture or drain call.
    pub(super) fn capture_step(&mut self, now: Instant) {
        let mut any_unpaused = false;
        let mut desired_limits: Option<VideoLimits> = None;
        let mut pace_fps = 1u32;
        let mut encoder_bps = u32::MAX;
        for idx in 0..MAX_VIEWERS {
            let Some(paused) = self.slots[idx]
                .as_ref()
                .map(|slot| slot.shared.targets.paused.load(Ordering::Acquire))
            else {
                continue;
            };
            if paused {
                if let Some(slot) = &mut self.slots[idx] {
                    slot.paused = true;
                    slot.state.dropped_frames += slot.shared.outgoing.clear() as u64;
                }
                self.hide_peer_cursor(idx);
            } else if let Some(slot) = &mut self.slots[idx] {
                if slot.paused {
                    slot.paused = false;
                    self.keyframe = true;
                }
                any_unpaused = true;
                desired_limits = Some(match desired_limits {
                    None => slot.limits,
                    Some(cur) => VideoLimits {
                        max_width: cur.max_width.min(slot.limits.max_width),
                        max_height: cur.max_height.min(slot.limits.max_height),
                        max_fps: cur.max_fps.max(slot.limits.max_fps),
                    },
                });
                pace_fps =
                    pace_fps.max(slot.shared.targets.pace_fps.load(Ordering::Acquire).max(1));
                encoder_bps =
                    encoder_bps.min(slot.shared.targets.encoder_bps.load(Ordering::Acquire));
            }
        }
        self.cursor_shapes = self
            .slots
            .iter()
            .flatten()
            .any(|slot| !slot.paused && slot.cursor_shapes);
        if !any_unpaused {
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
                        for slot in self.slots.iter_mut().flatten() {
                            slot.state.dropped_frames += 1;
                        }
                    }
                    Ok(None) => {}
                    Err(failure) => self.fail(failure, now),
                }
            }
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
        let limits_changed = desired_limits.filter(|limits| *limits != self.limits);
        if let Some(limits) = limits_changed {
            self.limits = limits;
            self.keyframe = true;
            if self.context.is_some()
                && !retry
                && let Err(failure) = self
                    .backend
                    .configure_video(self.limits, self.limits.max_fps)
            {
                self.fail(failure, now);
                return;
            }
        }
        if self.check_due(now) && !self.geometry(now) {
            return;
        }
        let Some(context) = &self.context else {
            return;
        };
        if pace_fps != self.pace_fps {
            self.pace_fps = pace_fps;
            self.pacer.set_pace(pace_fps, now);
        }
        if retry || self.pacer.due(now) {
            let can_deliver = self
                .slots
                .iter()
                .flatten()
                .any(|slot| !slot.paused && slot.shared.outgoing.len() < TRANSPORT_GATE);
            if can_deliver && self.backend.pending_video() < ENCODER_DEPTH {
                let keyframe = self
                    .slots
                    .iter()
                    .flatten()
                    .filter(|slot| !slot.paused)
                    .fold(self.keyframe, |due, slot| {
                        due | slot.shared.idr.lock().take_due(now)
                    });
                let rate = VideoRate {
                    bitrate: encoder_bps,
                    pace_fps,
                };
                let result = self
                    .backend
                    .capture_video(context, self.codec, keyframe, rate);
                if keyframe {
                    // Handed over: an IDR still in the encoder at the next
                    // tick must not be asked for again.
                    self.keyframe = false;
                    for slot in self.slots.iter().flatten().filter(|slot| !slot.paused) {
                        slot.shared.idr.lock().answered(now);
                    }
                }
                let done = self.clock.now();
                self.drain_due = self.drain_after(done);
                self.produced(result, done);
            } else {
                // Transport backpressure or a full encoder, never
                // presentation: skip the tick.
                for slot in self.slots.iter_mut().flatten().filter(|slot| !slot.paused) {
                    slot.state.skipped_ticks += 1;
                }
            }
            self.pacer.consume(now);
        } else if self.backend.pending_video() > 0 {
            let wait = HELPER_DRAIN_WAIT_MAX.min(self.pacer.next().saturating_duration_since(now));
            let result = self.backend.drain_video(context, wait);
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
        self.frame_id += 1;
        if !format.duplicate {
            self.window.unique += 1;
        }
        let video = VideoSize {
            width: format.width,
            height: format.height,
        };
        self.video = Some(video);
        self.gop = format.gop;
        self.streaming = true;
        let active_count = self
            .slots
            .iter()
            .flatten()
            .filter(|slot| !slot.paused)
            .count();
        if active_count == 0 {
            return;
        }
        enum Fanout {
            Single(Option<Vec<u8>>),
            Shared(bytes::Bytes),
        }
        let mut fanout = if active_count == 1 {
            Fanout::Single(Some(frame.bytes))
        } else {
            Fanout::Shared(bytes::Bytes::from(frame.bytes))
        };
        for idx in 0..MAX_VIEWERS {
            let Some(slot) = &mut self.slots[idx] else {
                continue;
            };
            if slot.paused {
                continue;
            }
            if format.keyframe {
                slot.shared.idr.lock().answered(at);
            }
            slot.shared.auth.lock().on_produced(self.frame_id, at);
            slot.state.record.video = Some(video);
            slot.state.gop = format.gop;
            slot.streaming = true;
            // B-send writes the `State` of a generation before its first unit:
            // publish it before the unit can be taken.
            self.publish_slot(idx);
            let Some(slot) = &mut self.slots[idx] else {
                continue;
            };
            let unit_bytes = match &mut fanout {
                Fanout::Single(owned) => match owned.take() {
                    Some(vec) => FrameBytes::Owned(vec),
                    None => continue,
                },
                Fanout::Shared(bytes) => FrameBytes::Shared(bytes.clone()),
            };
            let pushed = slot.shared.outgoing.push(HostUnit {
                generation: slot.state.record.generation,
                frame_id: self.frame_id,
                codec: format.codec,
                width: format.width,
                height: format.height,
                keyframe: format.keyframe,
                duplicate: format.duplicate,
                pts_us: format.pts_us,
                encode_us: format.encode_us,
                ready_at: at,
                bytes: unit_bytes,
            });
            if let Push::Overflow { dropped } = pushed {
                slot.state.dropped_frames += dropped as u64;
                slot.shared.idr.lock().request(IdrReason::HostResync);
            }
        }
    }
}

/// A capture failure's reason for the viewer.
pub(super) fn unavailable_reason(failure: &io::Error) -> StateReason {
    match failure_kind(failure) {
        Some(FailureKind::SecureDesktop) => StateReason::SecureDesktop,
        Some(FailureKind::DisplayLost) => StateReason::DisplayLost,
        Some(
            FailureKind::CaptureFailed
            | FailureKind::EncoderFailed { .. }
            | FailureKind::Unsupported
            | FailureKind::PermissionMissing
            | FailureKind::ConsentRevoked,
        )
        | None => StateReason::CaptureFailed,
    }
}
