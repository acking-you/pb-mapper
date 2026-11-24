//! Thread C's pointer reports.

use super::{Clock, Engine, link_lost};
use crate::{
    Backend, CursorShape,
    protocol::MAX_VIEWERS,
    shared::{CursorReport, CursorUpdate},
};
use std::time::{Duration, Instant};

/// The longest wait between two pointer samples while capturing.
pub(crate) const CURSOR_INTERVAL: Duration = Duration::from_millis(16);
/// The shortest: a faster pace samples once a period, down to this.
pub(crate) const CURSOR_MIN: Duration = Duration::from_millis(8);
/// How long after the controller's move thread C looks at the shape the
/// pointer took for it: the hand over a link, the I-beam over text, the
/// resize arrow at a border.
pub(crate) const SHAPE_LOOK: Duration = Duration::from_millis(2);
/// One more look this long after the last move, for a slower app; a later
/// shape waits for the next sample.
pub(crate) const SHAPE_LATE: Duration = Duration::from_millis(8);
const _: () = assert!(
    CURSOR_MIN.as_nanos() < CURSOR_INTERVAL.as_nanos(),
    "a faster pace samples more often, never less",
);
const _: () = assert!(
    SHAPE_LOOK.as_nanos() < SHAPE_LATE.as_nanos()
        && SHAPE_LATE.as_nanos() < CURSOR_INTERVAL.as_nanos(),
    "each look comes before the sample that would see the shape anyway",
);

/// The looks at the pointer's shape that thread C owes the controller's
/// moves, ahead of the next sample: `SHAPE_LOOK` after the newest move, and
/// `SHAPE_LATE` after it. A look still pending for an earlier move keeps
/// its time where that is sooner, so moves closer together than
/// `SHAPE_LOOK` never put a look off, and running it leaves the newest
/// move's own looks. At most one is pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ShapeLook {
    /// When the pending look runs.
    due: Instant,
    /// The newest move.
    moved: Instant,
}
impl ShapeLook {
    pub(super) fn due(self) -> Instant {
        self.due
    }

    /// The looks after a move at `now`. A pending one already due stays
    /// due: it runs in this iteration, for the move before.
    pub(super) fn after_move(look: Option<Self>, now: Instant) -> Self {
        let first = now + SHAPE_LOOK;
        Self {
            due: look.map_or(first, |look| look.due.min(first)),
            moved: now,
        }
    }

    /// What is left once the pending look ran at `now`: the newest move's
    /// looks still ahead.
    fn next(self, now: Instant) -> Option<Self> {
        [SHAPE_LOOK, SHAPE_LATE]
            .into_iter()
            .map(|after| self.moved + after)
            .find(|due| *due > now)
            .map(|due| Self {
                due,
                moved: self.moved,
            })
    }
}

impl<B: Backend, K: Clock> Engine<B, K> {
    /// The wait between two samples while capturing: `CURSOR_INTERVAL`, or
    /// the pace's period where that is shorter, but never under
    /// `CURSOR_MIN`. A pointer a viewer does not draw itself (the host
    /// user's, another controller's that the OS stopped short, one an app
    /// moved) then moves at the session's frame rate.
    pub(super) fn cursor_interval(&self) -> Duration {
        CURSOR_INTERVAL.min(self.pacer.period()).max(CURSOR_MIN)
    }

    /// Every `cursor_interval` while capturing, and at each look a
    /// controller's move left (`ShapeLook`); never while paused or
    /// unavailable. A sample is reported when it differs from the last
    /// report, or, equal to it, once more after C takes a move, so the
    /// report carries that move's sequence; never when it is the echo of the
    /// controller's own move, unless the shape changed. `CursorEcho` holds
    /// the rule. The backend is asked for the shape only in a session whose
    /// viewer reads shapes. A look reads the pointer and its shape, and only
    /// when a viewer that reads shapes gets a new one does it go on as a
    /// sample and start the clock again; otherwise it reports nothing, so a
    /// viewer gets a position sooner than the clock only with a new shape.
    /// A reading that finds a new shape keeps the looks still owed: that
    /// shape may answer an earlier move, and the newest move's come later.
    pub(super) fn sample_cursor(&mut self, now: Instant) {
        if self.paused || self.unavailable.is_some() {
            return;
        }
        let look = self.shape_due.filter(|look| now >= look.due());
        if let Some(look) = look {
            self.shape_due = look.next(now);
        }
        if now >= self.cursor_due {
            self.read_cursor(now, false);
        } else if look.is_some() && self.cursor_shapes {
            self.read_cursor(now, true);
        }
    }

    /// One reading of the pointer, and of its shape where a viewer reads
    /// shapes; for a `look`, reported only with a new shape.
    fn read_cursor(&mut self, now: Instant, look: bool) {
        let Some(context) = &self.context else {
            return;
        };
        if !look {
            self.cursor_due = now + self.cursor_interval();
        }
        let position = match self.backend.cursor(context) {
            Ok(position) => position,
            Err(failure) => return self.fail(failure, now),
        };
        // Only of a pointer showing on the shared display: one hidden, or on
        // a display the viewer does not see, tells it nothing, not even its
        // shape, and the viewer keeps the last.
        let showing = position.is_some_and(|position| position.visible);
        if look && !showing {
            return;
        }
        let shape = (self.cursor_shapes && showing).then(|| self.backend.cursor_shape(context));
        let mut reshaped = false;
        match shape {
            Some(Ok(Some(shape))) => {
                let (kind, width, height) = (shape.kind(), shape.width(), shape.height());
                self.cursor_shape = Some(shape.clone());
                for slot in self.slots.iter_mut().flatten() {
                    if slot.cursor_shapes && slot.cursor.reshape(shape.clone()) {
                        reshaped = true;
                    }
                }
                if reshaped {
                    tracing::debug!(
                        target: "pb_mapper_desktop::session",
                        event = "desktop_cursor_shape",
                        kind = ?kind,
                        width,
                        height,
                        look,
                    );
                }
            }
            Some(Err(failure)) if link_lost(&failure) => return self.fail(failure, now),
            Some(Err(failure)) => tracing::debug!(
                target: "pb_mapper_desktop::session",
                event = "desktop_cursor_shape_failed",
                detail = %failure,
            ),
            Some(Ok(None)) | None => {}
        }
        if look {
            if !reshaped {
                return;
            }
            // A sample in all but its time: the clock starts again from it.
            self.cursor_due = now + self.cursor_interval();
        }
        let mover_moved = std::mem::replace(&mut self.mover_moved_since_sample, false);
        let owner_slot = match (self.last_pointer_owner, position) {
            (Some((mover, mx, my)), Some(pos)) if pos.visible => {
                let at = (u32::try_from(pos.x).ok(), u32::try_from(pos.y).ok());
                if at == (Some(mx), Some(my)) {
                    Some(mover)
                } else if mover_moved {
                    // OS clamped the mover's injection; update both owner and peer cursor.
                    if let (Some(cx), Some(cy)) = at {
                        self.last_pointer_owner = Some((mover, cx, cy));
                        if let Some(slot) = &mut self.slots[mover.index()] {
                            slot.pointer = Some((cx, cy));
                        }
                    }
                    self.publish_peer_cursor(mover.index(), pos.x, pos.y);
                    Some(mover)
                } else {
                    // Physical host pointer moved independently.
                    self.last_pointer_owner = None;
                    None
                }
            }
            _ => {
                if !mover_moved {
                    self.last_pointer_owner = None;
                }
                None
            }
        };
        for idx in 0..MAX_VIEWERS {
            let Some(slot) = &mut self.slots[idx] else {
                continue;
            };
            if slot.paused {
                continue;
            }
            let position =
                if slot.peer_cursors && owner_slot.is_some_and(|mover| mover != slot.slot) {
                    // Controllers keep their local pointer. A watcher replaces the
                    // last visible OS pointer with the mover's coloured cursor.
                    if slot.state.record.controlling {
                        continue;
                    }
                    Some(crate::CursorPosition::default())
                } else {
                    position
                };
            let cursor = position.map(|position| CursorUpdate {
                generation: slot.state.record.generation,
                x: position.x,
                y: position.y,
                visible: position.visible,
                shape: slot.cursor.shape_serial,
            });
            if let Some(report) = slot.cursor.sample(cursor, slot.sequence) {
                slot.shared.cursor.send_replace(Some(report));
            }
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
/// helper's, which its worker reads after each input it applies and pushes
/// in order with its answers, is never older than the last injection.
///
/// The shape is no position: a new shape is reported even at the echo of the
/// controller's own move (the viewer takes the shape of every report, the
/// position only of one its moves did not overtake), and an equal one, read
/// again under a new OS handle, is no change.
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
    /// The pointer's shape, as the backend last reported a different one.
    shape: Option<CursorShape>,
    /// Counts the shapes: `CursorUpdate::shape` of the current one.
    pub(super) shape_serial: u64,
}
impl CursorEcho {
    /// Take `shape` as the pointer's; whether it differs from the last.
    pub(super) fn reshape(&mut self, shape: CursorShape) -> bool {
        if self.shape.as_ref() == Some(&shape) {
            return false;
        }
        self.shape = Some(shape);
        self.shape_serial += 1;
        true
    }

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
        let reshaped = self.reported.map(|reported| reported.shape) != Some(cursor.shape);
        if !reshaped
            && cursor.visible
            && self.injected.is_some_and(|(x, y)| at == (Some(x), Some(y)))
        {
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
            shape: self.shape.clone(),
        })
    }
}
