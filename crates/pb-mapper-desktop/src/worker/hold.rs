//! The hold that keeps a drag start from merging into a double click.

use crate::{
    Button, Input, InputEvent,
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
    /// The last injected pointer position, in input space.
    pointer: Option<(u32, u32)>,
    /// The last injected left press, and where the pointer was.
    left_down: Option<(Instant, (u32, u32))>,
}
impl DragHold {
    /// When a drag-start press at `now` must wait: within the double-click
    /// time and box of the last left press.
    pub(super) fn until(&self, guard: crate::DoubleClickGuard, now: Instant) -> Option<Instant> {
        let (pressed, (x, y)) = self.left_down?;
        let (px, py) = self.pointer?;
        let time = Duration::from_millis(guard.time_ms.into());
        let until = (pressed + time + DRAG_GUARD_MARGIN).min(now + DRAG_GUARD_MAX);
        (now.saturating_duration_since(pressed) < time
            && px.abs_diff(x) <= guard.half_box.0
            && py.abs_diff(y) <= guard.half_box.1
            && until > now)
            .then_some(until)
    }

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

    pub(super) fn injected(&mut self, input: &Input, now: Instant) {
        match *input {
            Input::Move { x, y } => self.pointer = Some((x, y)),
            Input::Button {
                button: Button::Left,
                down: true,
                ..
            } => self.left_down = self.pointer.map(|pointer| (now, pointer)),
            Input::Button { .. }
            | Input::Wheel { .. }
            | Input::Key { .. }
            | Input::Text { .. }
            | Input::Lock {}
            | Input::PhysicalKey { .. } => {}
        }
    }
}
