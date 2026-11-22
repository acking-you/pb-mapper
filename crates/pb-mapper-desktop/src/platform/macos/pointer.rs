//! The pointer as the injector leaves it. `CGEventPost` returns before the
//! window server applies a move, so the pointer read right after a move can
//! still show where it was. Thread C reports every position but the one it
//! injected, and a viewer in control follows a report more than a pixel from
//! its own pointer, so that stale position dragged the viewer's pointer back
//! after each move: it flickered between the two for as long as it moved.
//! The `Backend::cursor` contract is the pointer as it stands after every
//! input applied. Until the window server shows the injected pixel, or
//! `SETTLE` passes (the system stopped the move short, or someone at the Mac
//! moved the mouse), the injected pixel is the pointer.

use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

/// Well past the time the window server takes to apply a posted move, even
/// under load; a stop short of the injected pixel is reported this late.
pub(crate) const SETTLE: Duration = Duration::from_millis(250);

/// The injector's and the cursor's, on thread C.
pub(crate) type Shared = Rc<Cell<Pointer>>;

/// The last move posted, until the window server shows it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Pointer {
    /// Absolute pixel, and when it was posted.
    moved: Option<((i32, i32), Instant)>,
}

impl Pointer {
    /// A move to absolute pixel `to` was posted at `now`.
    pub(crate) fn moved(&mut self, to: (i32, i32), now: Instant) {
        self.moved = Some((to, now));
    }

    /// The pointer to report at `now`, given the absolute pixel the window
    /// server shows (`None`: not on the bound display).
    pub(crate) fn settle(&mut self, shown: Option<(i32, i32)>, now: Instant) -> Option<(i32, i32)> {
        let Some((to, at)) = self.moved else {
            return shown;
        };
        if shown == Some(to) || now.saturating_duration_since(at) >= SETTLE {
            self.moved = None;
            return shown;
        }
        Some(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Before the window server applies a move, the read shows the old
    /// pixel, or none when that was off the display: the injected one is
    /// reported. Once it shows the move, reads are the window server's again,
    /// so a later move of the Mac's own mouse is reported; and a move the
    /// system stopped short is reported once `SETTLE` has passed.
    #[test]
    fn a_posted_move_is_the_pointer_until_the_window_server_shows_it() {
        let start = Instant::now();
        let mut pointer = Pointer::default();
        assert_eq!(pointer.settle(Some((5, 5)), start), Some((5, 5)));

        pointer.moved((100, 40), start);
        assert_eq!(pointer.settle(Some((5, 5)), start), Some((100, 40)));
        assert_eq!(pointer.settle(None, start), Some((100, 40)));
        assert_eq!(pointer.settle(Some((100, 40)), start), Some((100, 40)));
        assert_eq!(pointer.settle(Some((7, 9)), start), Some((7, 9)));

        pointer.moved((200, 80), start);
        let late = start + SETTLE;
        assert_eq!(
            pointer.settle(Some((199, 80)), late - Duration::from_millis(1)),
            Some((200, 80))
        );
        assert_eq!(pointer.settle(Some((199, 80)), late), Some((199, 80)));
    }
}
