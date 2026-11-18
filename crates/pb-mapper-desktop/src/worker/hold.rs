//! The hold that keeps a drag start from merging into a double click.

use crate::{
    Input, InputEvent,
    lane::{LANE_CAPACITY, LaneCommand},
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// A held drag-start press waits this much past the double-click time...
const DRAG_GUARD_MARGIN: Duration = Duration::from_millis(20);
/// ...but never longer than this.
const DRAG_GUARD_MAX: Duration = Duration::from_millis(600);
const _: () = assert!(
    DRAG_GUARD_MARGIN.as_nanos() < DRAG_GUARD_MAX.as_nanos(),
    "the margin past the double-click time stays below the cap"
);

/// A left press that starts a drag right after a click would merge with it
/// into a double click on the host. It waits out the OS double-click time
/// instead, with every later command behind it. The hold only delays: held
/// commands carry no authorisation of their own, each runs the full input path
/// when the hold ends, and anything that ends control or changes the
/// generation drops them all, since none was injected yet.
#[derive(Default)]
pub(super) struct DragHold {
    /// In order, with consecutive moves coalesced to the newest.
    held: VecDeque<LaneCommand>,
    /// Held commands other than moves.
    presses: usize,
    pub(super) until: Option<Instant>,
}

/// The shared injector counts clicks from every controller on this display.
#[derive(Clone, Copy)]
pub(super) struct LeftPress {
    pub at: Instant,
    pub position: (u32, u32),
}

impl LeftPress {
    pub(super) fn drag_deadline(
        self,
        pointer: (u32, u32),
        guard: crate::DoubleClickGuard,
        now: Instant,
    ) -> Option<Instant> {
        let time = Duration::from_millis(guard.time_ms.into());
        let until = self.at + (time + DRAG_GUARD_MARGIN).min(DRAG_GUARD_MAX);
        (now.saturating_duration_since(self.at) < time
            && pointer.0.abs_diff(self.position.0) <= guard.half_box.0
            && pointer.1.abs_diff(self.position.1) <= guard.half_box.1
            && until > now)
            .then_some(until)
    }
}

impl DragHold {
    /// Hold `command` behind the press. False when that would hold more than
    /// a lane of commands.
    pub(super) fn push(&mut self, command: LaneCommand) -> bool {
        let is_move = |command: &LaneCommand| {
            matches!(
                command,
                LaneCommand::Input(InputEvent {
                    input: Input::Move { .. },
                    ..
                })
            )
        };
        if is_move(&command) {
            if let Some(last) = self.held.back_mut().filter(|last| is_move(last)) {
                *last = command;
                return true;
            }
        } else if self.presses == LANE_CAPACITY {
            return false;
        } else {
            self.presses += 1;
        }
        self.held.push_back(command);
        true
    }

    pub(super) fn release(&mut self) -> VecDeque<LaneCommand> {
        self.until = None;
        self.presses = 0;
        std::mem::take(&mut self.held)
    }
}
