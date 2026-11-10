//! Thread C's display mode changes and restores.

use super::{Clock, Engine, link_lost};
use crate::{
    Backend, DesktopContext, DesktopKind,
    modes::{ModeCommand, ModeList, SETTLE},
    protocol::NoticeCode,
};
use std::time::Instant;

impl<B: Backend, K: Clock> Engine<B, K> {
    /// A viewer's display mode change or restore, refused with a notice
    /// unless it controls, the desktop is the ordinary one, a change names a
    /// mode the viewer was offered, and the last change has settled. The
    /// owner's restore needs no control. A change is a new context at the
    /// next check, which follows at once: a new generation, an IDR, held
    /// input released, the encoder built at the new size.
    pub(super) fn display_mode(&mut self, command: ModeCommand, now: Instant) {
        let controls = self.state.record.controlling && self.allowed();
        let refused = if command != ModeCommand::HostRestore && !controls {
            Some("control of the remote computer is needed to change its resolution")
        } else if self
            .context
            .as_ref()
            .is_none_or(|context| context.desktop != DesktopKind::Ordinary)
        {
            Some("the remote resolution cannot change while the secure desktop shows")
        } else if let ModeCommand::Set(mode) = command
            && !self
                .state
                .record
                .modes
                .as_ref()
                .is_some_and(|modes| modes.supported.contains(&mode))
        {
            Some("the remote display does not offer that resolution")
        } else if self.settle_until.is_some_and(|until| now < until) {
            Some("the remote resolution is still changing")
        } else {
            None
        };
        if let Some(refused) = refused {
            return self.notice(NoticeCode::DisplayModeFailed, refused.into(), now);
        }
        let Some(context) = self.context.clone() else {
            return;
        };
        let changed = match command {
            ModeCommand::Set(mode) => self.backend.set_display_mode(&context, mode).map(|()| true),
            ModeCommand::Restore | ModeCommand::HostRestore => self.backend.restore_display_mode(),
        };
        match changed {
            Ok(changed) => self.mode_changed(changed, now),
            Err(failure) if link_lost(&failure) => {
                self.fatal = Some(failure.to_string());
            }
            Err(failure) => self.notice(NoticeCode::DisplayModeFailed, failure.to_string(), now),
        }
    }

    /// Put the display back without being asked: control was revoked. A
    /// failure stays journalled, and the session's end tries again.
    pub(super) fn restore_mode(&mut self, now: Instant) {
        match self.backend.restore_display_mode() {
            Ok(changed) => self.mode_changed(changed, now),
            Err(failure) if link_lost(&failure) => {
                self.fatal = Some(failure.to_string());
            }
            Err(failure) => tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "display_mode_not_restored",
                detail = %failure,
            ),
        }
    }

    /// After a mode command reached the backend: the modes are read again
    /// at once, which a paused session's capture would not do, so the
    /// owner's card never keeps a change already put back. A change also
    /// settles, and the context is checked at once.
    fn mode_changed(&mut self, changed: bool, now: Instant) {
        if let Some(context) = self.context.clone() {
            self.read_modes(&context);
        }
        if changed {
            self.settle_until = Some(now + SETTLE);
            self.context_due = now;
        }
    }

    /// The display's modes as the backend reads them now.
    pub(super) fn read_modes(&mut self, context: &DesktopContext) {
        self.state.record.modes = match self.backend.display_modes(context) {
            Ok(modes) => modes.filter(ModeList::valid),
            Err(failure) => {
                tracing::debug!(
                    target: "pb_mapper_desktop::session",
                    event = "display_modes_unread",
                    detail = %failure,
                );
                None
            }
        };
    }
}
