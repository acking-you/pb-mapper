//! The pointer on Windows: where it is, and whether it shows.
use crate::{CursorMode, Result, error, platform::CursorSource};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSOR_SUPPRESSED, CURSORINFO, GetCursorInfo,
};

pub(crate) struct WinCursor;
impl CursorSource for WinCursor {
    const MODE: CursorMode = CursorMode::Separate;
    fn read(&mut self) -> Result<Option<(i32, i32, bool)>> {
        let mut cursor = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            GetCursorInfo(&mut cursor).map_err(error)?;
        }
        let showing =
            cursor.flags.0 & CURSOR_SHOWING.0 != 0 && cursor.flags.0 & CURSOR_SUPPRESSED.0 == 0;
        let point = cursor.ptScreenPos;
        Ok(Some((point.x, point.y, showing)))
    }
}
