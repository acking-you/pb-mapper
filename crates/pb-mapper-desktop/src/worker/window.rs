//! Thread C's one-second statistics window.

use crate::shared::{EngineState, InputWaitStats};
use std::time::{Duration, Instant};

/// The window of the published capture rate and input wait.
const STATS_WINDOW: Duration = Duration::from_secs(1);
/// Input wait samples kept per window; the maximum is exact beyond it.
const WAIT_SAMPLES: usize = 4096;

/// The published one-second rates: unique captures, input wait and the
/// encoder's dwell.
pub(super) struct Window {
    started: Instant,
    pub(super) unique: u64,
    waits: Vec<u32>,
    max_wait: u32,
    /// The encoder's own submit to output time of each unit it returned.
    dwells: Vec<Duration>,
}
impl Window {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started: now,
            unique: 0,
            waits: Vec::new(),
            max_wait: 0,
            dwells: Vec::new(),
        }
    }

    pub(super) fn wait(&mut self, waited: Duration) {
        let waited = u32::try_from(waited.as_micros()).unwrap_or(u32::MAX);
        self.max_wait = self.max_wait.max(waited);
        if self.waits.len() < WAIT_SAMPLES {
            self.waits.push(waited);
        }
    }

    /// A unit's `VideoFormat::encode_us`, which the encoder measures itself:
    /// thread C cannot tell which unit a call returned, nor see one the
    /// encoder discards in the call that submits the next. Zero is no sample:
    /// JPEG, or a backend that measures none.
    pub(super) fn dwell(&mut self, encode_us: u32) {
        if encode_us > 0 && self.dwells.len() < WAIT_SAMPLES {
            self.dwells.push(Duration::from_micros(encode_us.into()));
        }
    }

    pub(super) fn roll(&mut self, now: Instant, state: &mut EngineState) {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed < STATS_WINDOW {
            return;
        }
        state.capture_fps = self.unique as f32 / elapsed.as_secs_f32();
        let middle = self.dwells.len() / 2;
        state.encoder_dwell =
            (!self.dwells.is_empty()).then(|| *self.dwells.select_nth_unstable(middle).1);
        self.waits.sort_unstable();
        let at = |quantile: usize| {
            self.waits
                .get((self.waits.len() * quantile / 100).min(self.waits.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        state.input_wait = InputWaitStats {
            p50_us: at(50),
            p99_us: at(99),
            max_us: self.max_wait,
        };
        // A new window, as `new` makes one, keeping the samples' buffers.
        let Self {
            started,
            unique,
            waits,
            max_wait,
            dwells,
        } = self;
        *started = now;
        *unique = 0;
        waits.clear();
        *max_wait = 0;
        dwells.clear();
    }
}
