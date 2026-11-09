//! Pointer metadata, confined to the selected display.

use crate::{CursorPosition, Display};

/// The pointer at `(x, y)`, absolute physical virtual-desktop pixels, relative
/// to `display`. A hidden pointer, or one on another display, reports nothing:
/// its position elsewhere is not disclosed.
pub(crate) fn confine(display: &Display, (x, y): (i32, i32), showing: bool) -> CursorPosition {
    let x = i64::from(x) - i64::from(display.x);
    let y = i64::from(y) - i64::from(display.y);
    if !showing || x < 0 || y < 0 || x >= i64::from(display.width) || y >= i64::from(display.height)
    {
        // Do not disclose the cursor location on a different monitor.
        return CursorPosition::default();
    }
    let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else {
        return CursorPosition::default();
    };
    CursorPosition {
        x,
        y,
        visible: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointer_coordinates_are_physical_and_confined_to_the_selected_display() {
        let display = Display {
            id: 1,
            name: "left monitor".into(),
            x: -1920,
            y: -200,
            width: 1920,
            height: 1080,
        };
        let point = (-100, 300);
        assert_eq!(
            confine(&display, point, true),
            CursorPosition {
                x: 1820,
                y: 500,
                visible: true
            }
        );
        // Both an off-display cursor and an OS-hidden cursor withhold position.
        assert_eq!(confine(&display, (0, 300), true), CursorPosition::default());
        assert_eq!(confine(&display, point, false), CursorPosition::default());
    }
}
