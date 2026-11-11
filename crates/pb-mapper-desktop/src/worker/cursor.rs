//! Thread C's pointer reports.

use super::{Clock, Engine};
use crate::{
    Backend,
    shared::{CursorReport, CursorUpdate},
};
use std::time::{Duration, Instant};

const CURSOR_INTERVAL: Duration = Duration::from_millis(16);

impl<B: Backend, K: Clock> Engine<B, K> {
    /// Every `CURSOR_INTERVAL` while capturing; never while paused or
    /// unavailable. Reported when it differs from the last report, or, equal
    /// to it, once more after C takes a move, so the report carries that
    /// move's sequence; never when it is the echo of the controller's own
    /// move. `CursorEcho` holds the rule.
    pub(super) fn sample_cursor(&mut self, now: Instant) {
        if self.paused || self.unavailable.is_some() || now < self.cursor_due {
            return;
        }
        let Some(context) = &self.context else {
            return;
        };
        self.cursor_due = now + CURSOR_INTERVAL;
        match self.backend.cursor(context) {
            Ok(position) => {
                let cursor = position.map(|position| CursorUpdate {
                    generation: self.state.record.generation,
                    x: position.x,
                    y: position.y,
                    visible: position.visible,
                });
                if let Some(report) = self.cursor.sample(cursor, self.sequence) {
                    self.shared.cursor.send_replace(Some(report));
                }
            }
            Err(failure) => self.fail(failure, now),
        }
    }
}

/// What C reports of the pointer: every sample unlike the last report, at once,
/// except the controller's own move coming back. The viewer draws its own
/// pointer where the controller put it; an echo of that move, late behind the
/// video, would drag it back. A shown pointer at the position C last injected,
/// in the same display pixels, is that echo, whatever it showed before, until
/// a report finds it elsewhere: then a return there is someone else's move. A
/// hidden pointer carries no position, so it is never an echo. Comparing with
/// the last report rather than the last sample is what reports a move the OS
/// stopped short (a `ClipCursor`, an edge): the pointer stays where it was, but
/// not where the controller put it. A sample equal to the last report has
/// nothing to tell a viewer, unless C took a move since. Every report carries
/// the last input sequence C took, and the controller sets aside a report older
/// than its newest move, which crossed it on the way; so after a move, a
/// sample that is not its echo is reported again at the newer sequence, and a
/// clamp or a pointer that stays hidden is still told. A backend observes the
/// pointer after every input it applied (see `Backend::cursor`), so the
/// helper's, observed during a capture call, is never older than the last
/// injection.
#[derive(Default)]
pub(super) struct CursorEcho {
    /// The last report.
    reported: Option<CursorUpdate>,
    /// Where the last move in this generation put the pointer, until a report
    /// finds it elsewhere.
    injected: Option<(u32, u32)>,
    /// C took a move off the lane, applied or not, since the last report.
    moved: bool,
    serial: u64,
}
impl CursorEcho {
    pub(super) fn injected(&mut self, x: u32, y: u32) {
        self.injected = Some((x, y));
    }

    pub(super) fn took_move(&mut self) {
        self.moved = true;
    }

    /// A new generation: nothing in it was injected yet.
    pub(super) fn invalidate(&mut self) {
        self.injected = None;
    }

    /// The report to send for `cursor`, sampled after the input of
    /// `input_sequence`, if any.
    fn sample(
        &mut self,
        cursor: Option<CursorUpdate>,
        input_sequence: u64,
    ) -> Option<CursorReport> {
        let cursor = cursor.filter(|cursor| self.moved || self.reported != Some(*cursor))?;
        let at = (u32::try_from(cursor.x).ok(), u32::try_from(cursor.y).ok());
        if cursor.visible && self.injected.is_some_and(|(x, y)| at == (Some(x), Some(y))) {
            return None;
        }
        self.reported = Some(cursor);
        self.injected = None;
        self.moved = false;
        self.serial += 1;
        Some(CursorReport {
            serial: self.serial,
            cursor,
            input_sequence,
        })
    }
}
