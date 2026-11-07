//! The viewer's decoder recovery (D7) and its keyframe limiter. Every keyframe
//! request (the native decoder's, the sink's own after a drop, the stall
//! watchdog's, the user's) goes through `Recovery::request`, the only path to
//! `ViewerMessage::Keyframe`, so the host never sees a request storm. Recovery
//! never depends on presentation receipts reaching the host: the picture is
//! back once the sink accepts a presentation of the IDR it handed out.
//!
//! ```text
//! Live --decoder error | backlog/overflow drop | gap | stall--> AwaitingIdr
//! AwaitingIdr --an IDR handed out, then it or a later frame presented--> Live
//! any --pause--> Paused --resume--> AwaitingIdr, held (the host resumes with an IDR)
//! any --State connecting or capture unavailable--> HostUnavailable
//! HostUnavailable --State streaming--> AwaitingIdr, held
//! Live --new generation--> AwaitingIdr, held (the host's invalidate sends the IDR)
//! AwaitingIdr --5 decoder errors in 30 s, nothing presented--> Failed (terminal)
//! ```
//!
//! The host's `State` says `paused` only because this viewer paused, which
//! `Paused` already covers; after a resume the host's IDR may arrive before
//! its `State` says `streaming` again, so that `paused` is no unavailability.
//!
//! While an IDR is awaited, requests go at most at 0.5, 1, 2, 4, 4 … s
//! intervals, and a received IDR resets that backoff. The timer re-requests
//! only while no IDR answered the last request, and an IDR answers once its
//! header is read, so neither an IDR slower to cross than the next request
//! nor a renderer that takes frames but shows nothing brings another IDR
//! every half second.
use crate::{KeyframeReason, viewer::DecoderState};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// Requests leave at most this often, `User` ones included.
pub(crate) const KEYFRAME_MIN_INTERVAL: Duration = Duration::from_millis(500);
/// The backoff between requests for one awaited IDR doubles up to this.
pub(crate) const KEYFRAME_BACKOFF_MAX: Duration = Duration::from_secs(4);
/// The host sends an IDR by itself after a generation change or a resume; no
/// request goes for this long.
pub(crate) const HOLD_AFTER_DISCONTINUITY: Duration = Duration::from_secs(1);
/// Frames handed out but not presented for this long, while the consumer
/// keeps taking frames, are a stall.
pub(crate) const STALL_WATCHDOG: Duration = Duration::from_millis(700);
/// This many decoder errors within `DECODER_FAILURE_WINDOW`, with no frame
/// presented in between, give up on the session's video.
pub(crate) const DECODER_FAILURE_LIMIT: usize = 5;
pub(crate) const DECODER_FAILURE_WINDOW: Duration = Duration::from_secs(30);

/// One episode of waiting for an IDR.
#[derive(Clone, Copy, Debug)]
struct Awaiting {
    /// Why the episode started; the timer's requests carry it.
    reason: KeyframeReason,
    /// Requests sent since the last IDR arrived.
    attempt: u32,
    /// No request but a `User` one goes before this.
    not_before: Instant,
    /// An IDR began to arrive since the last request: the timer waits for it
    /// to be shown instead of asking again.
    answered: bool,
    /// The first IDR handed out in this episode, `(generation, frame_id)`.
    /// Presenting it, or a later frame of its generation, ends the episode.
    idr: Option<(u64, u64)>,
}
impl Awaiting {
    fn new(reason: KeyframeReason, not_before: Instant) -> Self {
        Self {
            reason,
            attempt: 0,
            not_before,
            answered: false,
            idr: None,
        }
    }
    /// The host sends an IDR by itself: ask only if none came after the hold.
    fn held(now: Instant) -> Self {
        Self::new(KeyframeReason::Gap, now + HOLD_AFTER_DISCONTINUITY)
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Live,
    AwaitingIdr(Awaiting),
    Paused,
    HostUnavailable,
    Failed,
}

/// Owned by the sink and changed under its lock.
pub(crate) struct Recovery {
    phase: Phase,
    /// The host's last `State` was available; remembered across a pause.
    host_available: bool,
    last_request: Option<Instant>,
    requested: u64,
    /// Decoder errors since the last presented frame, within the window.
    failures: VecDeque<Instant>,
}
impl Default for Recovery {
    fn default() -> Self {
        Self {
            // Nothing is requested before the host's first streaming `State`,
            // whose IDR starts the picture.
            phase: Phase::HostUnavailable,
            host_available: false,
            last_request: None,
            requested: 0,
            failures: VecDeque::with_capacity(DECODER_FAILURE_LIMIT),
        }
    }
}
impl Recovery {
    /// A request for an IDR, for `reason`. True when it goes to the host now.
    /// Nothing goes while paused, while the host does not stream, or after
    /// the decoder failed. A decoder error counts toward the failure limit;
    /// any failure but `User` starts waiting for an IDR, whose requests
    /// follow the backoff. `User` bypasses the backoff but not
    /// `KEYFRAME_MIN_INTERVAL`.
    pub fn request(&mut self, reason: KeyframeReason, now: Instant) -> bool {
        if !matches!(self.phase, Phase::Live | Phase::AwaitingIdr(_)) {
            return false;
        }
        if reason == KeyframeReason::DecoderError && self.decoder_failed(now) {
            return false;
        }
        if reason == KeyframeReason::User {
            return self.interval_elapsed(now) && self.sent(now);
        }
        if matches!(self.phase, Phase::Live) {
            self.phase = Phase::AwaitingIdr(Awaiting::new(reason, now));
        }
        self.retry(now)
    }

    /// The viewer tick: the backoff's next request while an unanswered IDR is
    /// awaited, or the stall watchdog's while live. Returns what was sent.
    pub fn tick(&mut self, now: Instant, stalled: bool) -> Option<KeyframeReason> {
        match self.phase {
            Phase::Live if stalled => self
                .request(KeyframeReason::Stall, now)
                .then_some(KeyframeReason::Stall),
            Phase::AwaitingIdr(awaiting) if !awaiting.answered => {
                self.retry(now).then_some(awaiting.reason)
            }
            _ => None,
        }
    }

    /// An IDR's header was read: the host answered, so the timer waits for it
    /// instead of asking again. The hold and the backoff still apply to other
    /// requests until it reaches the sink.
    pub fn on_idr_arriving(&mut self) {
        if let Phase::AwaitingIdr(awaiting) = &mut self.phase {
            awaiting.answered = true;
        }
    }

    /// An IDR (or a JPEG picture) reached the sink: the host answered, so the
    /// backoff starts over.
    pub fn on_idr_received(&mut self, now: Instant) {
        if let Phase::AwaitingIdr(awaiting) = &mut self.phase {
            awaiting.attempt = 0;
            awaiting.answered = true;
            awaiting.not_before = now;
        }
    }

    /// The consumer took an IDR: presenting it brings the picture back.
    pub fn on_idr_handed(&mut self, generation: u64, frame_id: u64) {
        if let Phase::AwaitingIdr(awaiting) = &mut self.phase
            && awaiting.idr.is_none()
        {
            awaiting.idr = Some((generation, frame_id));
        }
    }

    /// The sink accepted a presentation.
    pub fn on_presented(&mut self, generation: u64, frame_id: u64) {
        self.failures.clear();
        if let Phase::AwaitingIdr(Awaiting {
            idr: Some((idr_generation, idr_frame)),
            ..
        }) = self.phase
            && generation == idr_generation
            && frame_id >= idr_frame
        {
            self.phase = Phase::Live;
        }
    }

    /// The host's `State`: `available` unless it is connecting or its capture
    /// is unavailable. A generation change while live, or available again,
    /// waits for the IDR the host sends by itself.
    pub fn on_host_state(&mut self, available: bool, new_generation: bool, now: Instant) {
        self.host_available = available;
        self.phase = match self.phase {
            phase @ (Phase::Failed | Phase::Paused) => phase,
            _ if !available => Phase::HostUnavailable,
            Phase::HostUnavailable => Phase::AwaitingIdr(Awaiting::held(now)),
            Phase::Live | Phase::AwaitingIdr(_) if new_generation => {
                Phase::AwaitingIdr(Awaiting::held(now))
            }
            phase => phase,
        };
    }

    pub fn pause(&mut self) {
        if !matches!(self.phase, Phase::Failed) {
            self.phase = Phase::Paused;
        }
    }

    /// The host answers `Resume` with an IDR.
    pub fn resume(&mut self, now: Instant) {
        if matches!(self.phase, Phase::Paused) {
            self.phase = if self.host_available {
                Phase::AwaitingIdr(Awaiting::held(now))
            } else {
                Phase::HostUnavailable
            };
        }
    }

    #[cfg(test)]
    pub fn live(&self) -> bool {
        matches!(self.phase, Phase::Live)
    }

    pub fn failed(&self) -> bool {
        matches!(self.phase, Phase::Failed)
    }

    /// How the overlay sees the decoder. Waiting for the first picture is not
    /// recovering.
    pub fn decoder_state(&self, presented_before: bool) -> DecoderState {
        match self.phase {
            Phase::Failed => DecoderState::Failed,
            Phase::AwaitingIdr(_) if presented_before => DecoderState::Recovering,
            _ => DecoderState::Ok,
        }
    }

    pub fn requested(&self) -> u64 {
        self.requested
    }

    /// Records a decoder error; true once the limit is reached.
    fn decoder_failed(&mut self, now: Instant) -> bool {
        while self
            .failures
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= DECODER_FAILURE_WINDOW)
        {
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        if self.failures.len() < DECODER_FAILURE_LIMIT {
            return false;
        }
        self.phase = Phase::Failed;
        tracing::warn!(
            target: "pb_mapper_desktop::frames",
            failures = self.failures.len(),
            "desktop_decoder_failed"
        );
        true
    }

    /// The next request of the awaited IDR, if its backoff allows one now.
    fn retry(&mut self, now: Instant) -> bool {
        let elapsed = self.interval_elapsed(now);
        let Phase::AwaitingIdr(awaiting) = &mut self.phase else {
            return false;
        };
        if !elapsed || now < awaiting.not_before {
            return false;
        }
        awaiting.attempt += 1;
        awaiting.answered = false;
        awaiting.not_before = now + backoff(awaiting.attempt);
        self.sent(now)
    }

    fn interval_elapsed(&self, now: Instant) -> bool {
        self.last_request
            .is_none_or(|last| now.saturating_duration_since(last) >= KEYFRAME_MIN_INTERVAL)
    }

    fn sent(&mut self, now: Instant) -> bool {
        self.last_request = Some(now);
        self.requested += 1;
        true
    }
}

/// The wait after the `attempt`-th request: 0.5, 1, 2, then 4 s.
fn backoff(attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(3);
    (KEYFRAME_MIN_INTERVAL * (1 << doublings)).min(KEYFRAME_BACKOFF_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KeyframeReason::{DecoderError, Gap, Stall, User};

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// Streaming, with generation 1's IDR shown.
    fn live(start: Instant) -> Recovery {
        let mut recovery = Recovery::default();
        recovery.on_host_state(true, true, start);
        recovery.on_idr_received(start);
        recovery.on_idr_handed(1, 1);
        recovery.on_presented(1, 1);
        assert!(recovery.live());
        recovery
    }

    #[test]
    fn recovery_stops_after_five_decoder_failures_and_pause_suppresses_requests() {
        let start = Instant::now();
        let at = |millis| start + ms(millis);

        // Nothing is asked before the host streams, nor during the hold its
        // first IDR is expected in.
        let mut recovery = Recovery::default();
        assert!(!recovery.request(Gap, at(0)));
        recovery.on_host_state(true, true, at(0));
        assert!(!recovery.request(Gap, at(500)));
        assert_eq!(recovery.tick(at(999), false), None);
        assert_eq!(recovery.tick(at(1000), false), Some(Gap));
        // Presenting a frame handed before the IDR does not end the wait.
        recovery.on_idr_received(at(1100));
        recovery.on_presented(1, 1);
        assert!(!recovery.live());
        recovery.on_idr_handed(1, 2);
        recovery.on_presented(1, 3);
        assert!(recovery.live());
        assert_eq!(recovery.decoder_state(true), DecoderState::Ok);

        // A stall while live asks once; the IDR that answers stops the timer
        // until it is shown.
        assert_eq!(recovery.tick(at(2000), true), Some(Stall));
        assert_eq!(recovery.decoder_state(true), DecoderState::Recovering);
        recovery.on_idr_received(at(2050));
        assert_eq!(recovery.tick(at(9000), false), None);
        recovery.on_idr_handed(1, 10);
        recovery.on_presented(1, 10);
        assert!(recovery.live());

        // Four decoder errors and a presented frame start the count over.
        let mut recovery = live(start);
        for millis in [0, 100, 200, 300] {
            recovery.request(DecoderError, at(millis));
        }
        recovery.on_idr_handed(1, 2);
        recovery.on_presented(1, 2);
        // Five within 30 s with nothing presented give up; the first went to
        // the host, the limiter held back the rest.
        assert!(recovery.request(DecoderError, at(1000)));
        for millis in [1100, 1200, 1300] {
            assert!(!recovery.request(DecoderError, at(millis)));
            assert!(!recovery.failed());
        }
        assert!(!recovery.request(DecoderError, at(30_999)));
        assert!(recovery.failed());
        assert_eq!(recovery.decoder_state(true), DecoderState::Failed);
        let requested = recovery.requested();
        assert!(!recovery.request(User, at(40_000)));
        assert_eq!(recovery.tick(at(40_000), true), None);
        recovery.pause();
        recovery.resume(at(40_000));
        recovery.on_host_state(true, true, at(40_000));
        assert!(recovery.failed());
        assert_eq!(recovery.requested(), requested);

        // Errors spread over more than 30 s never add up to five.
        let mut recovery = live(start);
        for seconds in 0..10 {
            recovery.request(DecoderError, at(seconds * 8_000));
        }
        assert!(!recovery.failed());

        // Paused: nothing at all, not even a user's request or a stall.
        let mut recovery = live(start);
        recovery.pause();
        for reason in [DecoderError, Gap, Stall, User] {
            assert!(!recovery.request(reason, at(1000)));
        }
        assert_eq!(recovery.tick(at(5000), true), None);
        assert_eq!(recovery.requested(), 0);
        // The host resumes with an IDR: none is asked for during the hold.
        recovery.resume(at(10_000));
        assert!(!recovery.request(Gap, at(10_500)));
        assert!(recovery.request(Gap, at(11_000)));
        // Nor while the host's capture is unavailable.
        recovery.on_host_state(false, false, at(12_000));
        assert!(!recovery.request(User, at(12_000)));
        assert_eq!(recovery.tick(at(20_000), true), None);
        assert_eq!(recovery.requested(), 1);

        // After a resume the host's IDR may come before its `State` streams
        // again; showing it is enough, and nothing is asked for.
        let mut recovery = live(start);
        recovery.pause();
        recovery.on_host_state(true, false, at(100));
        recovery.resume(at(200));
        recovery.on_idr_received(at(250));
        recovery.on_idr_handed(1, 5);
        recovery.on_presented(1, 5);
        recovery.on_host_state(true, false, at(300));
        assert!(recovery.live());
        assert_eq!(recovery.tick(at(5000), false), None);
        assert_eq!(recovery.requested(), 0);

        // An IDR's header stops the timer while its payload crosses, but the
        // hold still holds back other requests until the IDR reaches the sink.
        let mut recovery = live(start);
        recovery.on_host_state(true, true, at(0));
        recovery.on_idr_arriving();
        assert!(!recovery.request(DecoderError, at(700)));
        assert_eq!(recovery.tick(at(1500), false), None);
        recovery.on_idr_received(at(2000));
        recovery.on_idr_handed(2, 1);
        recovery.on_presented(2, 1);
        assert!(recovery.live());
        assert_eq!(recovery.requested(), 0);
    }
}
