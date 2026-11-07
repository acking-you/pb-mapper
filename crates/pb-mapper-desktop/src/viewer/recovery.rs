//! The viewer's keyframe limiter. Every keyframe request (the native
//! decoder's, the sink's own after a drop, the user's) goes through it, and it
//! is the only path to `ViewerMessage::Keyframe`, so the host never sees a
//! request storm. Recovery never depends on presentation receipts.
use crate::KeyframeReason;
use std::time::{Duration, Instant};

/// Requests leave at most this often.
pub(crate) const KEYFRAME_MIN_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(crate) struct Recovery {
    last_request: Option<Instant>,
    requested: u64,
}
impl Recovery {
    /// Whether a request goes to the host now. None goes while the viewer is
    /// paused (the host sends an IDR on resume), nor sooner than
    /// `KEYFRAME_MIN_INTERVAL` after the last one.
    pub fn request(&mut self, _reason: KeyframeReason, now: Instant, paused: bool) -> bool {
        if paused
            || self
                .last_request
                .is_some_and(|last| now.saturating_duration_since(last) < KEYFRAME_MIN_INTERVAL)
        {
            return false;
        }
        self.last_request = Some(now);
        self.requested += 1;
        true
    }

    pub fn requested(&self) -> u64 {
        self.requested
    }
}
