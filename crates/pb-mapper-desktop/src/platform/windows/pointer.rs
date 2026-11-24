//! The pointer as `WinInput` moves it. A move is input, one absolute
//! `SendInput` event, as a mouse's own move is: low-level hooks and raw
//! input see it, and the idle timer counts it. `SetCursorPos` only places
//! the pointer. `SendInput` returns before the system applies the move, so
//! `WinCursor` reports the injected pixel until `GetCursorInfo` shows it
//! (`input::cursor::Pointer`).
//!
//! UIPI drops a process's input to a window of higher integrity (an
//! elevated program in front of a host that is not elevated), and
//! `SendInput` need not say so. A settle window that ends with the pointer
//! still where the read before its first move found it, more than a pixel
//! short of a target inside the cursor's clip, is taken for that (a target
//! outside the clip is a clamp, which placing the pointer makes too): moves
//! then place the pointer while the window then in the foreground stays
//! there, and the token's integrity level is logged. A `SendInput` that
//! takes nothing (another thread blocks input, or this thread may not send
//! input to the input desktop) turns placing on at once, its Win32 error
//! logged too.
use crate::input::cursor::{Pointer, Settled};
use std::{cell::Cell, rc::Rc, time::Instant};
use windows::Win32::{
    Foundation::{HWND, RECT},
    UI::WindowsAndMessaging::{GetClipCursor, GetForegroundWindow},
};

/// The injector's and the cursor's, on thread C.
pub(crate) type Shared = Rc<Cell<Moves>>;

/// How moves reach the pointer, and the last one until it shows.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Moves {
    /// The last move made, until `GetCursorInfo` shows it.
    pointer: Pointer,
    /// Moves place the pointer while this window is in the foreground.
    placing: Option<HWND>,
}

impl Moves {
    /// A move to absolute pixel `to` was made at `now`.
    pub(crate) fn moved(&mut self, to: (i32, i32), now: Instant) {
        self.pointer.moved(to, now);
    }

    /// Whether the next move places the pointer rather than injects: until
    /// the window in front when injected moves stopped taking effect leaves
    /// the foreground. One more call per move while it does, none otherwise.
    pub(crate) fn placing(&mut self) -> bool {
        let Some(window) = self.placing else {
            return false;
        };
        // SAFETY: no arguments; it reads the foreground window's handle.
        if unsafe { GetForegroundWindow() } == window {
            return true;
        }
        self.placing = None;
        false
    }

    /// `SendInput` took nothing, `GetLastError` saying `code`: moves place
    /// the pointer at once, this one included, while the window in front
    /// stays there.
    pub(crate) fn refused(&mut self, code: u32) {
        self.place(Some(code));
    }

    /// The pointer to report at `now`, `shown` being where `GetCursorInfo`
    /// shows it; moves that never took effect turn placing on.
    pub(crate) fn settle(&mut self, shown: (i32, i32), now: Instant) -> (i32, i32) {
        let Settled { at, unmoved } = self.pointer.settle(Some(shown), now);
        if let Some(to) = unmoved
            && self.placing.is_none()
            && within_clip(to)
        {
            self.place(None);
        }
        at.unwrap_or(shown)
    }

    /// Moves place the pointer while the window now in front stays there;
    /// logged once for each such window, with the Win32 error of a
    /// `SendInput` that refused the move (none when the moves were dropped).
    fn place(&mut self, refused: Option<u32>) {
        // SAFETY: no arguments; it reads the foreground window's handle.
        self.placing = Some(unsafe { GetForegroundWindow() });
        tracing::warn!(
            target: "pb_mapper_desktop::session",
            event = "desktop_moves_placed",
            win32 = ?refused,
            token = %super::input::token_context(),
        );
    }
}

/// Whether the pointer may go to `to`: inside the rectangle `ClipCursor`
/// confines it to, the whole virtual desktop when nothing does. A clip that
/// cannot be read confines nothing.
fn within_clip((x, y): (i32, i32)) -> bool {
    let mut clip = RECT::default();
    // SAFETY: an out rectangle; physical pixels in thread C's per-monitor DPI
    // scope.
    if unsafe { GetClipCursor(&mut clip) }.is_err() {
        return true;
    }
    (clip.left..clip.right).contains(&x) && (clip.top..clip.bottom).contains(&y)
}

/// The `SendInput` absolute coordinate, 0 to 65535, of `pixel` on an axis
/// of the virtual desktop that starts at `origin` and spans `size` pixels;
/// a pixel off it, the nearest edge. It is the least value whose scaled
/// coordinate, `value × size / 65536`, falls on that pixel when truncated;
/// rounded to the nearest, it falls there too on an axis under 32768
/// pixels, so either way Windows maps it lands on the pixel. `None` for an
/// empty axis.
pub(crate) fn absolute(pixel: i32, origin: i32, size: i32) -> Option<i32> {
    let size = i64::from(size);
    if size <= 0 {
        return None;
    }
    let offset = (i64::from(pixel) - i64::from(origin)).clamp(0, size - 1);
    // ceil(offset × 65536 / size)
    let value = ((offset << 16) + size - 1) / size;
    i32::try_from(value.min(0xffff)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pixel of 1920-, 3840- and 7680-pixel axes, each starting left
    /// of (or above) the primary display's origin, lands on itself whether
    /// Windows truncates or rounds the scaled coordinate; a pixel off the
    /// desktop lands on its nearest edge.
    #[test]
    fn every_pixel_of_the_virtual_desktop_has_its_own_absolute_coordinate() {
        for size in [1920, 3840, 7680] {
            for origin in [-size / 2, -size + 1, -1] {
                let mut last = -1;
                for pixel in origin..origin + size {
                    let value = absolute(pixel, origin, size).unwrap();
                    assert!(value > last && value <= 0xffff, "{pixel} of {size}");
                    last = value;
                    let scaled = i64::from(value) * i64::from(size);
                    let truncated = origin + i32::try_from(scaled >> 16).unwrap();
                    let rounded = origin + i32::try_from((scaled + 0x8000) >> 16).unwrap();
                    assert_eq!((truncated, rounded), (pixel, pixel), "{pixel} of {size}");
                }
                assert_eq!(absolute(origin - 5, origin, size), Some(0));
                assert_eq!(
                    absolute(origin + size, origin, size),
                    absolute(origin + size - 1, origin, size)
                );
            }
        }
        assert_eq!(absolute(0, 0, 0), None);
    }
}
