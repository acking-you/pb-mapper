//! The host's only input authorisation ledger. Input is admitted only for
//! a frame the viewer presented, validated by its `VideoSink` and then here.
//! Transport receipts never reach this type, admitting input never renews it,
//! and cursor observation renews nothing.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// How long a presented frame keeps input admitted after it was produced.
pub(crate) const PRESENTED_FRESH: Duration = Duration::from_secs(2);
/// Produced frames remembered for validation; 2 s at 120 fps.
pub(crate) const RECENT_FRAMES: usize = 256;
const _: () = assert!(
    RECENT_FRAMES as u64 >= PRESENTED_FRESH.as_secs() * crate::video::MAX_FPS as u64,
    "the ledger remembers every frame that can still be fresh"
);

pub(crate) struct AuthLedger {
    /// Mirrors the engine's generation; C writes it on invalidate.
    generation: u64,
    /// `(frame_id, produced)`, newest last; C writes on every produced frame.
    recent: VecDeque<(u64, Instant)>,
    /// `(generation, frame_id)`, set only by a validated `Presented`.
    acked: Option<(u64, u64)>,
    stale_presentation: bool,
    lease_due: bool,
}
impl AuthLedger {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            recent: VecDeque::with_capacity(RECENT_FRAMES),
            acked: None,
            stale_presentation: false,
            lease_due: false,
        }
    }

    pub fn on_produced(&mut self, frame_id: u64, at: Instant) {
        if self.recent.len() == RECENT_FRAMES {
            self.recent.pop_front();
        }
        self.recent.push_back((frame_id, at));
    }

    /// A new generation: nothing produced or presented before it admits input.
    pub fn on_invalidate(&mut self, generation: u64) {
        self.generation = generation;
        self.recent.clear();
        self.acked = None;
        self.lease_due = false;
    }

    /// B-recv. Accepts a presentation of the current generation whose frame was
    /// produced less than `PRESENTED_FRESH` ago and that advances `acked`.
    /// Anything else marks the presentation stale, so C releases held input.
    pub fn on_presented(&mut self, generation: u64, frame_id: u64, now: Instant) -> bool {
        let advances = self.acked.is_none_or(|(acked_generation, acked)| {
            acked_generation == generation && frame_id > acked
        });
        if generation != self.generation || !advances || !self.produced_fresh(frame_id, now) {
            self.stale_presentation = true;
            return false;
        }
        self.acked = Some((generation, frame_id));
        self.lease_due = true;
        true
    }

    /// C (`Engine::input`). Admits input that references a frame no newer than
    /// the presented one, of the current generation, produced less than
    /// `PRESENTED_FRESH` ago. Never mutates.
    pub fn admits(&self, generation: u64, frame_id: u64, now: Instant) -> bool {
        generation == self.generation
            && self.acked.is_some_and(|(acked_generation, acked)| {
                acked_generation == generation && frame_id <= acked
            })
            && self.produced_fresh(frame_id, now)
    }

    /// The presented frame of the current generation is still fresh.
    pub fn fresh(&self, now: Instant) -> bool {
        self.acked.is_some_and(|(generation, acked)| {
            generation == self.generation && self.produced_fresh(acked, now)
        })
    }

    pub fn take_stale(&mut self) -> bool {
        std::mem::take(&mut self.stale_presentation)
    }

    pub fn take_lease_due(&mut self) -> bool {
        std::mem::take(&mut self.lease_due)
    }

    fn produced_fresh(&self, frame_id: u64, now: Instant) -> bool {
        self.recent.iter().rev().any(|(id, produced)| {
            *id == frame_id && now.saturating_duration_since(*produced) < PRESENTED_FRESH
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_and_input_never_grant_input_rights() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut ledger = AuthLedger::new(1);
        for frame in 1..=5 {
            ledger.on_produced(frame, ms(frame * 10));
        }
        // Produced and received frames alone grant nothing.
        assert!(!ledger.admits(1, 3, ms(60)));
        assert!(!ledger.fresh(ms(60)));
        assert!(!ledger.take_stale());

        assert!(ledger.on_presented(1, 3, ms(60)));
        assert!(ledger.take_lease_due());
        assert!(ledger.fresh(ms(60)));
        assert!(ledger.admits(1, 3, ms(60)));
        assert!(ledger.admits(1, 2, ms(60)));
        // Input never moves the presented frame forward.
        for _ in 0..3 {
            assert!(!ledger.admits(1, 4, ms(60)));
        }
        assert!(ledger.on_presented(1, 4, ms(70)));
        assert!(ledger.admits(1, 4, ms(70)));

        // A presented frame produced more than 2 s ago admits nothing.
        assert!(!ledger.admits(1, 4, ms(40 + 2000)));
        assert!(!ledger.fresh(ms(40 + 2000)));

        // A stale or repeated presentation is refused and flagged once.
        assert!(!ledger.on_presented(1, 4, ms(80)));
        assert!(!ledger.on_presented(0, 5, ms(80)));
        assert!(!ledger.on_presented(1, 9, ms(80)));
        assert!(ledger.take_stale());
        assert!(!ledger.take_stale());

        // A generation change clears what was presented.
        ledger.on_invalidate(2);
        assert!(!ledger.fresh(ms(90)));
        assert!(!ledger.admits(2, 4, ms(90)));
        assert!(!ledger.admits(1, 4, ms(90)));
        assert!(!ledger.on_presented(1, 5, ms(90)));
        ledger.on_produced(6, ms(100));
        assert!(ledger.on_presented(2, 6, ms(110)));
        assert!(ledger.admits(2, 6, ms(110)));
    }
}
