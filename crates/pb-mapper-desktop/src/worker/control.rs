//! Thread C's input lane and the control grant.

use super::{Clock, Engine, LeftPress, link_lost};
use crate::{
    Backend, Button, Input, InputEvent,
    idr::IdrReason,
    input::held::Pressed,
    lane::LaneCommand,
    protocol::{MAX_VIEWERS, NoticeCode, StateReason},
};
use std::time::{Duration, Instant};

/// The least time between two helper lease renewals.
const LEASE_RENEW_MIN: Duration = Duration::from_millis(250);

impl<B: Backend, K: Clock> Engine<B, K> {
    fn other_slots_hold_any(&self, idx: usize) -> bool {
        self.slots
            .iter()
            .enumerate()
            .any(|(j, slot)| j != idx && slot.as_ref().is_some_and(|s| !s.held.is_empty()))
    }

    fn other_slots_hold(&self, idx: usize, pressed: Pressed) -> bool {
        self.slots
            .iter()
            .enumerate()
            .any(|(j, slot)| j != idx && slot.as_ref().is_some_and(|s| s.held.contains(pressed)))
    }

    /// Release held OS inputs attributable exclusively to slot `idx`.
    pub(super) fn release_slot_inputs(&mut self, idx: usize) {
        if !self.other_slots_hold_any(idx) {
            self.backend.release();
            if let Some(slot) = &mut self.slots[idx] {
                slot.held.clear();
            }
            return;
        }
        if self.context.is_none() {
            self.backend.release();
            if let Some(slot) = &mut self.slots[idx] {
                slot.held.clear();
            }
            return;
        }
        let mut to_release = Vec::new();
        if let Some(slot) = &mut self.slots[idx] {
            slot.held.release(|pressed| {
                to_release.push(pressed);
                Ok(())
            });
        }
        for pressed in to_release {
            if !self.other_slots_hold(idx, pressed)
                && let Some(context) = &self.context
            {
                let release_input = pressed.into_release_input();
                let _ = self.backend.input_in_context(context, &release_input);
            }
        }
    }

    pub(super) fn sync_lease(&mut self) {
        let any_controlling = self
            .slots
            .iter()
            .flatten()
            .any(|slot| slot.state.record.controlling);
        if !any_controlling {
            self.lease = None;
        }
    }

    /// A full lane means input is late beyond repair: end control rather than
    /// inject a burst the user no longer expects.
    pub(super) fn slot_congested(&mut self, idx: usize) {
        self.release_slot_control(idx);
        if let Some(slot) = &mut self.slots[idx] {
            slot.control_note = Some((
                StateReason::InputCongestion,
                "input arrived faster than the host could apply it; request control again".into(),
            ));
        }
    }

    /// The lane overflowed: its input is late beyond repair, only counted as
    /// taken, and control ends. A display mode command is not input and
    /// runs, the owner's restore above all, which the host already said was
    /// queued. A viewer's is checked against control as the lane leaves it:
    /// after the viewer's own release or pause it is refused, as a drain
    /// refuses it, and a grant queued in the lane is not taken, as control
    /// ends anyway. Nothing runs once cancelled.
    pub(super) fn slot_overflowed(&mut self, idx: usize) {
        let Some(slot) = &mut self.slots[idx] else {
            return;
        };
        let mut drained = std::mem::take(&mut slot.drained);
        slot.shared.lane.drain_into(&mut drained);
        for (command, _) in drained.drain(..) {
            let cancelled = self.room.cancel.is_cancelled()
                || self.slots[idx]
                    .as_ref()
                    .is_none_or(|slot| slot.shared.cancel.is_cancelled());
            match command {
                LaneCommand::Input(event) => {
                    self.slot_taken(idx, &event);
                }
                LaneCommand::DisplayMode(command) if !cancelled => {
                    let now = self.clock.now();
                    self.display_mode(Some(idx), command, now);
                }
                // `congested` below releases control in full.
                LaneCommand::Control { enabled: false } | LaneCommand::EndControl => {
                    if let Some(slot) = &mut self.slots[idx] {
                        slot.state.record.controlling = false;
                    }
                }
                LaneCommand::DisplayMode(_)
                | LaneCommand::Control { enabled: true }
                | LaneCommand::Release => {}
            }
        }
        if let Some(slot) = &mut self.slots[idx] {
            slot.drained = drained;
        }
        self.slot_congested(idx);
    }

    /// Execute every queued command in order. False when cancelled: the rest
    /// of the lane is dropped, never executed.
    pub(super) fn drain_slot_lane(&mut self, idx: usize) -> bool {
        let Some(slot) = &mut self.slots[idx] else {
            return true;
        };
        let mut drained = std::mem::take(&mut slot.drained);
        slot.shared.lane.drain_into(&mut drained);
        let mut finished = true;
        for (command, received) in drained.drain(..) {
            let cancelled = self.room.cancel.is_cancelled()
                || self.slots[idx]
                    .as_ref()
                    .is_none_or(|slot| slot.shared.cancel.is_cancelled());
            if cancelled {
                // The rest drop with the drain, never run.
                finished = false;
                break;
            }
            let now = self.clock.now();
            self.window.wait(now.saturating_duration_since(received));
            self.execute_slot(idx, command, now);
        }
        if let Some(slot) = &mut self.slots[idx] {
            slot.drained = drained;
        }
        finished
    }

    fn execute_slot(&mut self, idx: usize, command: LaneCommand, now: Instant) {
        // A display mode command is never held behind a drag, nor ends one.
        let holding = self.slots[idx]
            .as_ref()
            .is_some_and(|slot| slot.hold.until.is_some());
        if holding && !matches!(command, LaneCommand::DisplayMode(_)) {
            if !matches!(
                command,
                LaneCommand::Release
                    | LaneCommand::EndControl
                    | LaneCommand::Control { enabled: false }
            ) {
                let pushed = self.slots[idx]
                    .as_mut()
                    .is_some_and(|slot| slot.hold.push(command));
                if !pushed {
                    self.slot_congested(idx);
                }
                return;
            }
            // Ending control drops the whole hold: nothing in it was injected.
            self.drop_slot_held(idx);
        }
        match command {
            LaneCommand::Control { enabled } => self.slot_control(idx, enabled),
            LaneCommand::Release => self.release_slot_inputs(idx),
            LaneCommand::EndControl => self.release_slot_control(idx),
            LaneCommand::Input(event) => match self.drag_guard(idx, &event, now) {
                Some(until) => {
                    if let Some(slot) = &mut self.slots[idx] {
                        slot.hold.until = Some(until);
                        slot.hold.push(LaneCommand::Input(event));
                    }
                }
                None => self.slot_input(idx, event, now),
            },
            LaneCommand::DisplayMode(command) => self.display_mode(Some(idx), command, now),
        }
    }

    /// When an admitted left press that starts a drag follows a click within
    /// the OS double-click time and box, it must wait until the returned time
    /// so the two never merge into a double click.
    fn drag_guard(&mut self, idx: usize, event: &InputEvent, now: Instant) -> Option<Instant> {
        let Input::Button {
            button: Button::Left,
            down: true,
            drag_start: true,
        } = event.input
        else {
            return None;
        };
        if !self.allowed() {
            return None;
        }
        let guard = self.backend.double_click_guard()?;
        let slot = self.slots[idx].as_ref()?;
        if !slot.state.record.controlling
            || !slot
                .shared
                .auth
                .lock()
                .admits(event.generation, event.frame_id, now)
        {
            return None;
        }
        let pointer = slot
            .pointer
            .or_else(|| self.last_pointer_owner.map(|(_, x, y)| (x, y)))?;
        self.last_left_press?.drag_deadline(pointer, guard, now)
    }

    /// The hold ended: each held command runs the full input path now, as if
    /// it had just left the lane. False when cancelled.
    pub(super) fn release_slot_hold(&mut self, idx: usize) -> bool {
        let Some(slot) = &mut self.slots[idx] else {
            return true;
        };
        let held = slot.hold.release();
        for command in held {
            let cancelled = self.room.cancel.is_cancelled()
                || self.slots[idx]
                    .as_ref()
                    .is_none_or(|slot| slot.shared.cancel.is_cancelled());
            if cancelled {
                return false;
            }
            let now = self.clock.now();
            // Another controller may have clicked while this lane waited.
            self.execute_slot(idx, command, now);
        }
        true
    }

    pub(super) fn allowed(&self) -> bool {
        self.room.policy.allowed() && self.room.policy.get() == self.policy
    }

    fn slot_control(&mut self, idx: usize, enabled: bool) {
        let allowed = self.allowed();
        let Some(currently_controlling) = self.slots[idx]
            .as_ref()
            .map(|slot| slot.state.record.controlling)
        else {
            return;
        };
        // A request repeated on a delayed link must not reset the desktop
        // generation, the decoder or an active lease.
        if enabled && allowed && currently_controlling {
            return;
        }
        if !enabled && !currently_controlling {
            self.release_slot_inputs(idx);
            return;
        }
        self.release_slot_control(idx);
        if let Some(slot) = &mut self.slots[idx] {
            slot.control_note = None;
        }
        if !enabled {
            return;
        }
        if !allowed {
            if let Some(slot) = &mut self.slots[idx] {
                slot.control_note = Some((
                    StateReason::ControlDenied,
                    "control is not authorized at the host".into(),
                ));
            }
            return;
        }
        if self.lease.is_none()
            && let Some(admission) = &self.admission
        {
            match admission.acquire() {
                Ok(lease) => self.lease = Some(lease),
                Err(failure) => {
                    if let Some(slot) = &mut self.slots[idx] {
                        slot.control_note = Some((StateReason::ControlDenied, failure.to_string()));
                    }
                    return;
                }
            }
        }
        if let Some(slot) = &mut self.slots[idx] {
            slot.state.record.controlling = true;
        }
    }

    /// Count `event` as taken off the lane, applied or not. A cursor report
    /// carries the last sequence taken, which tells the viewer what the report
    /// already reflects. False for a sequence already taken.
    pub(super) fn slot_taken(&mut self, idx: usize, event: &InputEvent) -> bool {
        let Some(slot) = &mut self.slots[idx] else {
            return false;
        };
        if event.sequence <= slot.sequence {
            return false;
        }
        slot.sequence = event.sequence;
        if matches!(event.input, Input::Move { .. }) {
            slot.cursor.took_move();
        }
        true
    }

    /// Drop every held command, none of which was injected; held input
    /// counts as taken.
    pub(super) fn drop_slot_held(&mut self, idx: usize) {
        let Some(slot) = &mut self.slots[idx] else {
            return;
        };
        let released = slot.hold.release();
        for command in released {
            if let LaneCommand::Input(event) = command {
                self.slot_taken(idx, &event);
            }
        }
    }

    fn slot_input(&mut self, idx: usize, event: InputEvent, now: Instant) {
        if !self.slot_taken(idx, &event) {
            return;
        }
        // The injection boundary checks the desktop for every input; this
        // checks it only when a check is due.
        if (self.check_due(now) && !self.geometry(now))
            || !self.allowed()
            || !self.slots[idx]
                .as_ref()
                .is_some_and(|slot| slot.state.record.controlling)
        {
            return;
        }
        let admitted = self.slot_live(idx)
            && self.slots[idx].as_ref().is_some_and(|slot| {
                slot.shared
                    .auth
                    .lock()
                    .admits(event.generation, event.frame_id, now)
            });
        if !admitted {
            // The screen changed or the presented frame is stale: wait for a
            // fresh frame, and hold nothing for this viewer meanwhile.
            self.release_slot_inputs(idx);
            return;
        }
        let lock = matches!(event.input, Input::Lock {});
        match self.inject_slot_input(idx, &event.input) {
            Ok(()) if lock => self.notice_all(
                NoticeCode::Locked,
                "The remote computer is locked".into(),
                now,
            ),
            Ok(()) => {
                if let Some(slot) = &mut self.slots[idx] {
                    if matches!(
                        event.input,
                        Input::Button {
                            button: Button::Left,
                            down: true,
                            ..
                        }
                    ) {
                        self.last_left_press = slot
                            .pointer
                            .or_else(|| self.last_pointer_owner.map(|(_, x, y)| (x, y)))
                            .map(|position| LeftPress { at: now, position });
                    }
                    if let Input::Move { x, y } = event.input {
                        slot.pointer = Some((x, y));
                        slot.cursor.injected(x, y);
                        self.last_pointer_owner = Some((slot.slot, x, y));
                        self.mover_moved_since_sample = true;
                    }
                }
                if let Input::Move { x, y } = event.input {
                    self.publish_peer_cursor(
                        idx,
                        i32::try_from(x).unwrap_or(i32::MAX),
                        i32::try_from(y).unwrap_or(i32::MAX),
                    );
                }
            }
            Err(failure) => {
                if link_lost(&failure) {
                    self.fatal = Some(failure);
                    return;
                }
                // A desktop change invalidates the event while control stays granted.
                let Some(generation) = self.slots[idx]
                    .as_ref()
                    .map(|slot| slot.state.record.generation)
                else {
                    return;
                };
                if !self.geometry(now)
                    || self.slots[idx]
                        .as_ref()
                        .is_none_or(|slot| slot.state.record.generation != generation)
                {
                    return;
                }
                if lock {
                    // Nothing was held down, so control stays.
                    self.notice_slot(idx, NoticeCode::LockFailed, failure.to_string(), now);
                    return;
                }
                // A live picture is not proof of working control. End the
                // grant and keep the failure until the viewer asks for control
                // again; queued presses are never replayed.
                self.release_slot_control(idx);
                if let Some(slot) = &mut self.slots[idx] {
                    slot.control_note =
                        Some((StateReason::InputFailed, format!("input_failed: {failure}")));
                }
            }
        }
    }

    fn inject_slot_input(&mut self, idx: usize, input: &Input) -> crate::Result<()> {
        let shared_hold = match *input {
            Input::Button {
                button,
                down: false,
                ..
            } => self.other_slots_hold(idx, Pressed::Button(button)),
            Input::Key { key, down: false } => self.other_slots_hold(idx, Pressed::Key(key)),
            Input::PhysicalKey { usage, down: false } => {
                self.other_slots_hold(idx, Pressed::Physical(usage))
            }
            _ => false,
        };
        let (Some(context), Some(slot)) = (&self.context, &mut self.slots[idx]) else {
            return Ok(());
        };
        // When another controller moved the OS cursor in the meantime, return
        // the pointer to this viewer's last position before clicking/scrolling.
        if matches!(input, Input::Button { .. } | Input::Wheel { .. })
            && let Some((x, y)) = slot.pointer
            && self.last_pointer_owner != Some((slot.slot, x, y))
        {
            let reposition = Input::Move { x, y };
            self.backend.input_in_context(context, &reposition)?;
            self.last_pointer_owner = Some((slot.slot, x, y));
            self.mover_moved_since_sample = true;
            slot.cursor.injected(x, y);
        }
        let inject = |_| {
            if shared_hold {
                Ok(())
            } else {
                self.backend.input_in_context(context, input)
            }
        };
        match *input {
            Input::Button { button, down, .. } => slot.held.button(button, down, inject),
            Input::Key { key, down } => slot.held.key(key, down, inject),
            Input::PhysicalKey { usage, down } => slot.held.physical(usage, down, inject),
            Input::Move { .. } | Input::Wheel { .. } | Input::Text { .. } | Input::Lock {} => {
                self.backend.input_in_context(context, input)
            }
        }
    }

    /// Release held input once when the presented frame goes stale. Nothing
    /// is admitted again until a fresh presentation, so one release suffices.
    pub(super) fn release_slot_if_stale(&mut self, idx: usize, now: Instant) {
        let Some((controlling, fresh, already_released)) = self.slots[idx].as_ref().map(|slot| {
            (
                slot.state.record.controlling,
                slot.shared.auth.lock().fresh(now),
                slot.stale_released,
            )
        }) else {
            return;
        };
        if controlling && !fresh {
            self.drop_slot_held(idx);
            if !already_released {
                self.release_slot_inputs(idx);
                if let Some(slot) = &mut self.slots[idx] {
                    slot.stale_released = true;
                }
            }
        } else if let Some(slot) = &mut self.slots[idx] {
            slot.stale_released = false;
        }
    }

    pub(super) fn renew_leases(&mut self, now: Instant) {
        let mut should_present = false;
        for slot in self.slots.iter_mut().flatten() {
            if !slot.state.record.controlling
                || slot
                    .last_lease
                    .is_some_and(|at| now.saturating_duration_since(at) < LEASE_RENEW_MIN)
                || !slot.shared.auth.lock().take_lease_due()
            {
                continue;
            }
            slot.last_lease = Some(now);
            should_present = true;
        }
        if should_present
            && let Some(context) = &self.context
            && let Err(failure) = self.backend.presented(context)
            && link_lost(&failure)
        {
            self.fatal = Some(failure);
        }
    }

    pub(super) fn release_slot_control(&mut self, idx: usize) {
        self.invalidate_slot(idx);
        if let Some(slot) = &mut self.slots[idx] {
            slot.state.record.controlling = false;
        }
        self.sync_lease();
    }

    pub(super) fn release_all_control(&mut self) {
        self.invalidate_all();
        for slot in self.slots.iter_mut().flatten() {
            slot.state.record.controlling = false;
        }
        self.lease = None;
    }

    /// Invalidate one viewer's generation without disturbing other viewers.
    pub(super) fn invalidate_slot(&mut self, idx: usize) {
        self.release_slot_inputs(idx);
        self.drop_slot_held(idx);
        if let Some(slot) = &self.slots[idx]
            && self
                .last_pointer_owner
                .is_some_and(|(owner, _, _)| owner == slot.slot)
        {
            self.last_pointer_owner = None;
        }
        self.hide_peer_cursor(idx);
        let Some(slot) = &mut self.slots[idx] else {
            return;
        };
        slot.pointer = None;
        slot.cursor.invalidate();
        slot.state.record.generation += 1;
        let generation = slot.state.record.generation;
        slot.shared.auth.lock().on_invalidate(generation);
        slot.state.dropped_frames += slot.shared.outgoing.retain_generation(generation) as u64;
        slot.shared.idr.lock().force(IdrReason::Generation);
        self.keyframe = true;
    }

    /// Desktop-wide new generation: nothing produced or presented before it
    /// admits input on any viewer, older units are dropped, and the next unit is an IDR.
    pub(super) fn invalidate_all(&mut self) {
        self.backend.release();
        self.last_pointer_owner = None;
        self.mover_moved_since_sample = false;
        self.last_left_press = None;
        self.hide_all_peer_cursors();
        for idx in 0..MAX_VIEWERS {
            self.drop_slot_held(idx);
            let Some(slot) = &mut self.slots[idx] else {
                continue;
            };
            slot.held.clear();
            slot.pointer = None;
            slot.cursor.invalidate();
            slot.state.record.generation += 1;
            let generation = slot.state.record.generation;
            slot.shared.auth.lock().on_invalidate(generation);
            slot.state.dropped_frames += slot.shared.outgoing.retain_generation(generation) as u64;
            slot.shared.idr.lock().force(IdrReason::Generation);
        }
        self.keyframe = true;
    }
}
