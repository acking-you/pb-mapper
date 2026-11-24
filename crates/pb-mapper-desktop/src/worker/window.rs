//! Thread C's one-second statistics window.

use crate::{Presented, shared::InputWaitStats};
use std::time::{Duration, Instant};

/// The window of the published capture rate and input wait.
const STATS_WINDOW: Duration = Duration::from_secs(1);
/// Input wait samples kept per window; the maximum is exact beyond it.
const WAIT_SAMPLES: usize = 4096;
/// Present-to-submit waits kept per window, in a buffer of their own: more
/// than a second of submits at the highest pace.
const PACING_SAMPLES: usize = 256;
/// Where `desktop_capture_pacing` goes: with the feedback interval.
const PACING_TARGET: &str = "pb_mapper_desktop::feedback";

/// Rolled one-second figures shared across active slots in the room.
#[derive(Clone, Copy, Default)]
pub(super) struct WindowStats {
    pub capture_fps: f32,
    pub encoder_dwell: Option<Duration>,
    pub input_wait: InputWaitStats,
}

/// The published one-second rates: unique captures, input wait and the
/// encoder's dwell.
pub(super) struct Window {
    started: Instant,
    pub(super) unique: u64,
    waits: Vec<u32>,
    max_wait: u32,
    /// The encoder's own submit to output time of each unit it returned.
    dwells: Vec<Duration>,
    pacing: Pacing,
}

/// How capture paced against the display this window, for the debug event
/// `desktop_capture_pacing`: never published, and nothing allocated.
struct Pacing {
    /// Captures that spent their tick, and those whose picture was dated.
    captures: u32,
    dated: u32,
    /// The presents the dated pictures covered.
    presents: u64,
    /// Captures that left the tick armed: they found nothing new, or an
    /// encoder that had not asked for input yet, or they sent a refinement
    /// repeat of a still picture.
    empty_polls: u32,
    /// From each dated picture's present to its capture, in microseconds.
    waits: [u32; PACING_SAMPLES],
    len: usize,
}
impl Pacing {
    const NEW: Self = Self {
        captures: 0,
        dated: 0,
        presents: 0,
        empty_polls: 0,
        waits: [0; PACING_SAMPLES],
        len: 0,
    };
}

impl Window {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started: now,
            unique: 0,
            waits: Vec::new(),
            max_wait: 0,
            dwells: Vec::new(),
            pacing: Pacing::NEW,
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

    /// A capture call that began at `at` spent its tick on `picture`, which
    /// its source dated, or not.
    pub(super) fn captured(&mut self, at: Instant, picture: Option<Presented>) {
        let pacing = &mut self.pacing;
        pacing.captures += 1;
        let Some(picture) = picture else {
            return;
        };
        pacing.dated += 1;
        pacing.presents += u64::from(picture.presents);
        if let Some(slot) = pacing.waits.get_mut(pacing.len) {
            let waited = at.saturating_duration_since(picture.at).as_micros();
            *slot = u32::try_from(waited).unwrap_or(u32::MAX);
            pacing.len += 1;
        }
    }

    /// A capture call left its tick armed: it found nothing new, or an
    /// encoder that had not asked for input yet, or it sent a refinement
    /// repeat.
    pub(super) fn empty_poll(&mut self) {
        self.pacing.empty_polls += 1;
    }

    pub(super) fn roll(&mut self, now: Instant) -> Option<WindowStats> {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed < STATS_WINDOW {
            return None;
        }
        let capture_fps = self.unique as f32 / elapsed.as_secs_f32();
        let middle = self.dwells.len() / 2;
        let encoder_dwell =
            (!self.dwells.is_empty()).then(|| *self.dwells.select_nth_unstable(middle).1);
        self.waits.sort_unstable();
        let at = |quantile: usize| {
            self.waits
                .get((self.waits.len() * quantile / 100).min(self.waits.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        let input_wait = InputWaitStats {
            p50_us: at(50),
            p99_us: at(99),
            max_us: self.max_wait,
        };
        self.pacing.trace(elapsed);
        // A new window, as `new` makes one, keeping the samples' buffers.
        let Self {
            started,
            unique,
            waits,
            max_wait,
            dwells,
            pacing,
        } = self;
        *started = now;
        *unique = 0;
        waits.clear();
        *max_wait = 0;
        dwells.clear();
        *pacing = Pacing::NEW;
        Some(WindowStats {
            capture_fps,
            encoder_dwell,
            input_wait,
        })
    }
}

impl Pacing {
    /// `desktop_capture_pacing`, for a window `elapsed` long that captured:
    /// the wait from a present to the capture that took it (p50 and p95),
    /// the share of captures whose picture was dated, and per second the
    /// captures, the presents they covered, the presents among them that
    /// newer ones replaced before a capture, and the polls that found
    /// nothing new.
    fn trace(&mut self, elapsed: Duration) {
        if self.captures == 0 && self.empty_polls == 0
            || !tracing::enabled!(target: PACING_TARGET, tracing::Level::DEBUG)
        {
            return;
        }
        let waits = self.waits.get_mut(..self.len).unwrap_or_default();
        waits.sort_unstable();
        let at = |quantile: usize| {
            waits
                .get((waits.len() * quantile / 100).min(waits.len().saturating_sub(1)))
                .copied()
        };
        let seconds = elapsed.as_secs_f64();
        let per_second = |count: u64| count as f64 / seconds;
        tracing::debug!(
            target: PACING_TARGET,
            event = "desktop_capture_pacing",
            wait_p50_us = at(50),
            wait_p95_us = at(95),
            dated_share = f64::from(self.dated) / f64::from(self.captures.max(1)),
            captures_per_s = per_second(self.captures.into()),
            presents_per_s = per_second(self.presents),
            coalesced_per_s = per_second(self.presents.saturating_sub(self.dated.into())),
            empty_polls_per_s = per_second(self.empty_polls.into()),
        );
    }
}
