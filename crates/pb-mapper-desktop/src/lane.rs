//! The ordered input lane from B-recv to thread C. B-recv pushes and
//! never waits; C drains the whole lane ahead of and after every backend
//! capture call.
use crate::{InputEvent, modes::ModeCommand, scheduler::Wake};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

/// At least the viewer's `INPUT_QUEUE`: a full viewer burst can arrive during
/// one backend call.
pub(crate) const LANE_CAPACITY: usize = 512;

pub(crate) enum LaneCommand {
    Control {
        enabled: bool,
    },
    Release,
    Input(InputEvent),
    /// The host ends control itself (viewer pause).
    EndControl,
    /// Change or restore the display's mode, in order with input.
    DisplayMode(ModeCommand),
}

pub(crate) struct InputLane {
    /// Each command with the time B-recv received it (`input_wait`).
    q: parking_lot::Mutex<VecDeque<(LaneCommand, Instant)>>,
    overflow: AtomicBool,
    wake: Wake,
}
impl InputLane {
    pub fn new(wake: Wake) -> Self {
        Self {
            q: parking_lot::Mutex::new(VecDeque::new()),
            overflow: AtomicBool::new(false),
            wake,
        }
    }

    /// Queue `command` and wake C. A full lane drops it and flags the overflow,
    /// which ends control: no input is silently delayed past the lane.
    pub fn push(&self, command: LaneCommand) -> bool {
        let queued = {
            let mut q = self.q.lock();
            if q.len() < LANE_CAPACITY {
                q.push_back((command, Instant::now()));
                true
            } else {
                false
            }
        };
        if !queued {
            self.overflow.store(true, Ordering::Release);
        }
        self.wake.notify();
        queued
    }

    pub fn drain(&self) -> VecDeque<(LaneCommand, Instant)> {
        std::mem::take(&mut *self.q.lock())
    }

    pub fn take_overflow(&self) -> bool {
        self.overflow.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_lane_drops_and_flags_overflow_in_order() {
        let lane = InputLane::new(Wake::new());
        for _ in 0..LANE_CAPACITY {
            assert!(lane.push(LaneCommand::Release));
        }
        assert!(!lane.take_overflow());
        assert!(!lane.push(LaneCommand::Control { enabled: true }));
        assert!(lane.take_overflow());
        assert!(!lane.take_overflow());
        let drained = lane.drain();
        assert_eq!(drained.len(), LANE_CAPACITY);
        assert!(
            drained
                .iter()
                .all(|(command, _)| matches!(command, LaneCommand::Release))
        );
        assert!(lane.push(LaneCommand::EndControl));
        assert!(matches!(
            lane.drain().pop_front(),
            Some((LaneCommand::EndControl, _))
        ));
    }
}
