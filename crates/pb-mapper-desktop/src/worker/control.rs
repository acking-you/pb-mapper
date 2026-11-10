//! Thread C's input lane and the control grant.

use super::{Clock, Engine, link_lost};
use crate::{
    Backend, Button, Input, InputEvent,
    idr::IdrReason,
    lane::LaneCommand,
    protocol::{NoticeCode, StateReason},
};
use std::time::{Duration, Instant};

/// The least time between two helper lease renewals.
const LEASE_RENEW_MIN: Duration = Duration::from_millis(250);

impl<B: Backend, K: Clock> Engine<B, K> {
    /// A full lane means input is late beyond repair: end control rather than
    /// inject a burst the user no longer expects.
    pub(super) fn congested(&mut self) {
        self.release_control();
        self.control_note = Some((
            StateReason::InputCongestion,
            "input arrived faster than the host could apply it; request control again".into(),
        ));
    }

    /// The lane overflowed: its input is late beyond repair, only counted as
    /// taken, and control ends. A display mode command is not input and runs
    /// as a drain would have run it, the owner's restore above all, which the
    /// host already said was queued. Nothing runs once cancelled.
    pub(super) fn overflowed(&mut self) {
        let mut drained = std::mem::take(&mut self.drained);
        self.shared.lane.drain_into(&mut drained);
        for (command, _) in drained.drain(..) {
            match command {
                LaneCommand::Input(event) => {
                    self.taken(&event);
                }
                LaneCommand::DisplayMode(command) if !self.shared.cancel.is_cancelled() => {
                    let now = self.clock.now();
                    self.display_mode(command, now);
                }
                LaneCommand::DisplayMode(_)
                | LaneCommand::Control { .. }
                | LaneCommand::Release
                | LaneCommand::EndControl => {}
            }
        }
        self.drained = drained;
        self.congested();
    }

    /// Execute every queued command in order. False when cancelled: the rest
    /// of the lane is dropped, never executed.
    pub(super) fn drain_lane(&mut self) -> bool {
        let mut drained = std::mem::take(&mut self.drained);
        self.shared.lane.drain_into(&mut drained);
        let mut finished = true;
        for (command, received) in drained.drain(..) {
            if self.shared.cancel.is_cancelled() {
                // The rest drop with the drain, never run.
                finished = false;
                break;
            }
            let now = self.clock.now();
            self.window.wait(now.saturating_duration_since(received));
            self.execute(command, now);
        }
        self.drained = drained;
        finished
    }

    fn execute(&mut self, command: LaneCommand, now: Instant) {
        // A display mode command is never held behind a drag, nor ends one.
        if self.hold.until.is_some() && !matches!(command, LaneCommand::DisplayMode(_)) {
            if !matches!(
                command,
                LaneCommand::Release
                    | LaneCommand::EndControl
                    | LaneCommand::Control { enabled: false }
            ) {
                if !self.hold.push(command) {
                    self.congested();
                }
                return;
            }
            // Ending control drops the whole hold: nothing in it was injected.
            self.drop_held();
        }
        match command {
            LaneCommand::Control { enabled } => self.control(enabled),
            LaneCommand::Release => self.backend.release(),
            LaneCommand::EndControl => self.release_control(),
            LaneCommand::Input(event) => match self.drag_guard(&event, now) {
                Some(until) => {
                    self.hold.until = Some(until);
                    self.hold.push(LaneCommand::Input(event));
                }
                None => self.input(event, now),
            },
            LaneCommand::DisplayMode(command) => self.display_mode(command, now),
        }
    }

    /// When an admitted left press that starts a drag follows a click within
    /// the OS double-click time and box, it must wait until the returned time
    /// so the two never merge into a double click.
    fn drag_guard(&mut self, event: &InputEvent, now: Instant) -> Option<Instant> {
        let Input::Button {
            button: Button::Left,
            down: true,
            drag_start: true,
        } = event.input
        else {
            return None;
        };
        if !self.state.record.controlling
            || !self.allowed()
            || !self
                .shared
                .auth
                .lock()
                .admits(event.generation, event.frame_id, now)
        {
            return None;
        }
        let guard = self.backend.double_click_guard()?;
        self.hold.until(guard, now)
    }

    /// The hold ended: each held command runs the full input path now, as if
    /// it had just left the lane. False when cancelled.
    pub(super) fn release_hold(&mut self) -> bool {
        let held = self.hold.release();
        for (index, command) in held.into_iter().enumerate() {
            if self.shared.cancel.is_cancelled() {
                return false;
            }
            let now = self.clock.now();
            match command {
                // The press that started the hold has waited it out.
                LaneCommand::Input(event) if index == 0 => self.input(event, now),
                command => self.execute(command, now),
            }
        }
        true
    }

    pub(super) fn allowed(&self) -> bool {
        self.shared.policy.allowed() && self.shared.policy.get() == self.policy
    }

    fn control(&mut self, enabled: bool) {
        let allowed = self.allowed();
        // A request repeated on a delayed link must not reset the desktop
        // generation, the decoder or an active lease.
        if enabled && allowed && self.state.record.controlling {
            return;
        }
        if !enabled && !self.state.record.controlling {
            self.backend.release();
            return;
        }
        self.release_control();
        self.control_note = None;
        if !enabled {
            return;
        }
        if !allowed {
            self.control_note = Some((
                StateReason::ControlDenied,
                "control is not authorized at the host".into(),
            ));
            return;
        }
        if let Some(admission) = &self.admission {
            match admission.acquire() {
                Ok(lease) => self.lease = Some(lease),
                Err(failure) => {
                    self.control_note = Some((StateReason::ControlDenied, failure.to_string()));
                    return;
                }
            }
        }
        self.state.record.controlling = true;
    }

    /// Count `event` as taken off the lane, applied or not. A cursor report
    /// carries the last sequence taken, which tells the viewer what the report
    /// already reflects. False for a sequence already taken.
    pub(super) fn taken(&mut self, event: &InputEvent) -> bool {
        if event.sequence <= self.sequence {
            return false;
        }
        self.sequence = event.sequence;
        if matches!(event.input, Input::Move { .. }) {
            self.cursor.took_move();
        }
        true
    }

    /// Drop every held command, none of which was injected; held input
    /// counts as taken.
    pub(super) fn drop_held(&mut self) {
        for command in self.hold.release() {
            if let LaneCommand::Input(event) = command {
                self.taken(&event);
            }
        }
    }

    fn input(&mut self, event: InputEvent, now: Instant) {
        if !self.taken(&event) {
            return;
        }
        // The injection boundary checks the desktop for every input; this
        // checks it only when a check is due.
        if (self.check_due(now) && !self.geometry(now))
            || !self.allowed()
            || !self.state.record.controlling
        {
            return;
        }
        if !self.live()
            || !self
                .shared
                .auth
                .lock()
                .admits(event.generation, event.frame_id, now)
        {
            // The screen changed or the presented frame is stale: wait for a
            // fresh frame, and hold nothing meanwhile.
            self.backend.release();
            return;
        }
        let Some(context) = self.context.clone() else {
            return;
        };
        let lock = matches!(event.input, Input::Lock {});
        match self.backend.input_in_context(&context, &event.input) {
            Ok(()) if lock => self.notice(
                NoticeCode::Locked,
                "The remote computer is locked".into(),
                now,
            ),
            Ok(()) => {
                self.hold.injected(&event.input, now);
                if let Input::Move { x, y } = event.input {
                    self.cursor.injected(x, y);
                }
            }
            Err(failure) => {
                if link_lost(&failure) {
                    self.fatal = Some(failure.to_string());
                } else {
                    // The boundary may have seen a desktop change this engine
                    // has not checked yet: the new generation admits nothing
                    // stale, so drop the event and keep control.
                    let generation = self.state.record.generation;
                    if !self.geometry(now) || self.state.record.generation != generation {
                        return;
                    }
                }
                if lock {
                    // Nothing was held down, so control stays.
                    self.notice(NoticeCode::LockFailed, failure.to_string(), now);
                    return;
                }
                // A live picture is not proof of working control. End the
                // grant and keep the failure until the viewer asks for control
                // again; queued presses are never replayed.
                self.release_control();
                self.control_note =
                    Some((StateReason::InputFailed, format!("input_failed: {failure}")));
            }
        }
    }

    /// Release held input once when the presented frame goes stale. Nothing
    /// is admitted again until a fresh presentation, so one release suffices.
    pub(super) fn release_if_stale(&mut self, now: Instant) {
        if self.state.record.controlling && !self.shared.auth.lock().fresh(now) {
            self.drop_held();
            if !self.stale_released {
                self.backend.release();
                self.stale_released = true;
            }
        } else {
            self.stale_released = false;
        }
    }

    pub(super) fn renew_lease(&mut self, now: Instant) {
        if !self.state.record.controlling
            || self
                .last_lease
                .is_some_and(|at| now.saturating_duration_since(at) < LEASE_RENEW_MIN)
            || !self.shared.auth.lock().take_lease_due()
        {
            return;
        }
        self.last_lease = Some(now);
        if let Some(context) = self.context.clone()
            && let Err(failure) = self.backend.presented(&context)
            && link_lost(&failure)
        {
            self.fatal = Some(failure.to_string());
        }
    }

    pub(super) fn release_control(&mut self) {
        self.invalidate();
        self.state.record.controlling = false;
        self.lease = None;
    }

    /// A new desktop generation: nothing produced or presented before it
    /// admits input, older units are not sent, and the next unit is an IDR.
    pub(super) fn invalidate(&mut self) {
        self.backend.release();
        self.drop_held();
        self.cursor.invalidate();
        self.state.record.generation += 1;
        let generation = self.state.record.generation;
        self.shared.auth.lock().on_invalidate(generation);
        self.state.dropped_frames += self.shared.outgoing.retain_generation(generation) as u64;
        self.shared.idr.lock().force(IdrReason::Generation);
    }
}
