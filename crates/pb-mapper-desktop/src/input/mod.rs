//! The input vocabulary a controlling viewer sends, and the neutral rules
//! every platform's injector follows.
pub(crate) mod cursor;
pub(crate) mod held;

use crate::{Display, Result, error};
use serde::{Deserialize, Serialize};

/// The OS double-click time and half the double-click box, read in the
/// interactive session. A press that starts a drag waits this long after a
/// click so the two never merge into a double click.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoubleClickGuard {
    pub time_ms: u32,
    pub half_box: (u32, u32),
}

/// Deliberately small, platform-independent input vocabulary. Text is Unicode
/// insertion, not an IME session; key events are for navigation and shortcuts.
/// Every variant is a struct variant: serde ignores extra fields on internally
/// tagged unit variants, which would undo `deny_unknown_fields`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Move {
        x: u32,
        y: u32,
    },
    Button {
        button: Button,
        down: bool,
        /// A press that starts a drag. The host only delays it past the OS
        /// double-click time after a click; it authorises nothing.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        drag_start: bool,
    },
    Wheel {
        horizontal: i32,
        vertical: i32,
    },
    Key {
        key: Key,
        down: bool,
    },
    Text {
        text: String,
    },
    /// Lock the workstation in the interactive session. Control-gated
    /// exactly like every other input.
    Lock {},
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
}
/// Keys for navigation, shortcuts and the modifier bar. Letters and
/// digits are virtual keys, never characters, so a shortcut works on any host
/// keyboard layout; text goes through `Input::Text`. `Meta` is the Windows key.
/// Ctrl+Alt+Del is deliberately unrepresentable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Shift,
    Control,
    Alt,
    Meta,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Space,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputEvent {
    pub generation: u64,
    pub frame_id: u64,
    pub sequence: u64,
    pub input: Input,
}

impl Input {
    pub(crate) fn validate(&self, display: &Display) -> Result<()> {
        let refused = match self {
            Self::Move { x, y } => {
                (*x >= display.width || *y >= display.height).then_some("pointer outside display")
            }
            Self::Text { text } => (text.len() > 1024 || text.contains('\0'))
                .then_some("text input too large or invalid"),
            Self::Wheel {
                horizontal,
                vertical,
            } => (horizontal.unsigned_abs() > 100 || vertical.unsigned_abs() > 100)
                .then_some("scroll input too large"),
            Self::Button { .. } | Self::Key { .. } | Self::Lock {} => None,
        };
        refused.map_or(Ok(()), |reason| Err(error(reason)))
    }
}

#[cfg(test)]
mod tests {
    use crate::{Button, Display, Input, Key};

    /// The injection boundary refuses a pointer off the display, text too
    /// long or holding NUL, and a scroll too large; nothing else.
    #[test]
    fn input_is_refused_only_outside_its_bounds() {
        let display = Display {
            id: 1,
            name: "bounds".into(),
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let valid = |input: Input| input.validate(&display).is_ok();
        assert!(valid(Input::Move { x: 1919, y: 1079 }));
        assert!(!valid(Input::Move { x: 1920, y: 0 }) && !valid(Input::Move { x: 0, y: 1080 }));
        assert!(valid(Input::Text {
            text: "a".repeat(1024)
        }));
        assert!(
            !valid(Input::Text {
                text: "a".repeat(1025)
            }) && !valid(Input::Text { text: "a\0".into() })
        );
        assert!(valid(Input::Wheel {
            horizontal: -100,
            vertical: 100
        }));
        assert!(
            !valid(Input::Wheel {
                horizontal: 101,
                vertical: 0
            }) && !valid(Input::Wheel {
                horizontal: 0,
                vertical: i32::MIN
            })
        );
        assert!(valid(Input::Button {
            button: Button::Left,
            down: true,
            drag_start: true
        }));
        assert!(valid(Input::Key {
            key: Key::A,
            down: true
        }));
        assert!(valid(Input::Lock {}));
    }
}
