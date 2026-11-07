//! When the host encodes an IDR. The GOP is infinite: IDRs come only on
//! demand, never periodically and never for an fps, bitrate, preset or
//! congestion change. Requests are deferred and coalesced, never dropped, and
//! recovery never waits for a presentation acknowledgement.
use crate::KeyframeReason;
use std::time::{Duration, Instant};

/// The least time between a requested IDR and the previous one.
pub(crate) const HOST_IDR_MIN_INTERVAL: Duration = Duration::from_millis(300);

/// Why an IDR is asked for. A new encoder and late encoder output need no
/// request: the encoder starts with an IDR and replaces late output with one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdrReason {
    /// `Engine::invalidate`: a control grant or revoke, an input failure, or
    /// capture recovered.
    Generation,
    Resume,
    Viewer(KeyframeReason),
    /// The outgoing head grew stale, or the hand-off overflowed.
    HostResync,
}

pub(crate) struct IdrPolicy {
    /// The first reason since the last IDR (coalesced).
    pending: Option<IdrReason>,
    forced: bool,
    last_emitted: Option<Instant>,
}
impl IdrPolicy {
    pub fn new() -> Self {
        Self {
            pending: None,
            forced: false,
            last_emitted: None,
        }
    }

    /// Immediate, on the next submit.
    pub fn force(&mut self, reason: IdrReason) {
        self.forced = true;
        self.note(reason);
    }

    /// Deferred until `HOST_IDR_MIN_INTERVAL` after the last IDR.
    pub fn request(&mut self, reason: IdrReason) {
        self.note(reason);
    }

    /// Whether the next submit must be an IDR. It stays true until the encoder
    /// emits one, so a submit the encoder refused does not lose the request.
    pub fn take_due(&mut self, now: Instant) -> bool {
        self.forced
            || (self.pending.is_some()
                && self.last_emitted.is_none_or(|last| {
                    now.saturating_duration_since(last) >= HOST_IDR_MIN_INTERVAL
                }))
    }

    /// An IDR left the encoder: it answers every request made so far.
    pub fn on_emitted(&mut self, now: Instant) {
        self.pending = None;
        self.forced = false;
        self.last_emitted = Some(now);
    }

    #[cfg(test)]
    fn pending(&self) -> Option<IdrReason> {
        self.pending
    }

    fn note(&mut self, reason: IdrReason) {
        self.pending.get_or_insert(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_coalesce_defer_and_are_never_dropped() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut idr = IdrPolicy::new();
        // Nothing is periodic.
        assert!(!idr.take_due(ms(0)));
        assert!(!idr.take_due(ms(60_000)));

        // Ten requests in 100 ms: one IDR now, and one 300 ms after it.
        let mut emitted = vec![];
        for n in 0..10 {
            idr.request(IdrReason::Viewer(KeyframeReason::Gap));
            if idr.take_due(ms(n * 10)) {
                idr.on_emitted(ms(n * 10));
                emitted.push(n * 10);
            }
        }
        assert_eq!(emitted, [0]);
        assert_eq!(idr.pending(), Some(IdrReason::Viewer(KeyframeReason::Gap)));
        // A deferred request stays due until the encoder emits it.
        assert!(!idr.take_due(ms(299)));
        assert!(idr.take_due(ms(300)));
        assert!(idr.take_due(ms(316)));
        idr.on_emitted(ms(316));
        assert!(!idr.take_due(ms(400)));

        // A force bypasses the interval.
        idr.force(IdrReason::Generation);
        assert!(idr.take_due(ms(320)));
        idr.on_emitted(ms(320));
        assert_eq!(idr.pending(), None);
        assert!(!idr.take_due(ms(10_000)));
    }
}
