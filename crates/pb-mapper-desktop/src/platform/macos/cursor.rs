//! The pointer on macOS: where the system has it, in pixels of the bound
//! display. Drawn apart from the picture (ScreenCaptureKit captures without
//! it). macOS says nothing reliable about another app hiding it, so on the
//! display it shows.

use super::desktop::Shared;
use crate::{CursorMode, Result, platform::CursorSource};
use objc2_core_graphics::CGEvent;

pub(crate) struct MacCursor {
    geometry: Shared,
}

impl MacCursor {
    pub(crate) fn new(geometry: Shared) -> Self {
        Self { geometry }
    }
}

impl CursorSource for MacCursor {
    const MODE: CursorMode = CursorMode::Separate;
    /// On another display, it is hidden from this one's viewer.
    fn read(&mut self) -> Result<Option<(i32, i32, bool)>> {
        let here = CGEvent::new(None);
        let point = CGEvent::location(here.as_deref());
        Ok(Some(match self.geometry.get().pixel((point.x, point.y)) {
            Some((x, y)) => (x, y, true),
            None => (0, 0, false),
        }))
    }
}
