//! The input vocabulary a controlling viewer sends, and the neutral rules
//! every platform's injector follows.
pub(crate) mod cursor;
pub(crate) mod held;

use crate::{Display, Result, error};
use serde::{Deserialize, Serialize};

/// The most UTF-8 bytes one `Input::Text` carries.
pub(crate) const TEXT_INPUT_MAX: usize = 1024;

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
/// insertion, not an IME session; key events are for navigation and shortcuts;
/// physical keys let the host's own layout and input method compose.
/// Every variant is a struct variant: serde ignores extra fields on internally
/// tagged unit variants, which would undo `deny_unknown_fields`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
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
    /// Typed as it is: a `\n` or `\t` is one Enter or Tab key, and the `\r`
    /// of a `\r\n` pair adds nothing.
    Text {
        text: String,
    },
    /// Lock the workstation in the interactive session. Control-gated
    /// exactly like every other input.
    Lock {},
    /// A key by its place on the keyboard: `usage` is its USB HID usage on
    /// the keyboard page (0x07), 0x04..=0xE7. The host's layout and input
    /// method decide what it types. Only to a host that listed
    /// `physical_keys`.
    PhysicalKey {
        usage: u16,
        down: bool,
    },
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
#[non_exhaustive]
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

/// The keyboard page's key usages: below are error codes, above reserved.
pub(crate) const PHYSICAL_KEY_USAGES: std::ops::RangeInclusive<u16> = 0x04..=0xE7;

/// One stretch of `Input::Text` as the host types it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Typed<'a> {
    /// Characters with no line break or tab among them.
    Chars(&'a str),
    /// Enter for a line break, Tab for a tab: one tap.
    Key(Key),
}

impl Input {
    pub(crate) fn validate(&self, display: &Display) -> Result<()> {
        let refused = match self {
            Self::Move { x, y } => {
                (*x >= display.width || *y >= display.height).then_some("pointer outside display")
            }
            Self::Text { text } => (text.len() > TEXT_INPUT_MAX || text.contains('\0'))
                .then_some("text input too large or invalid"),
            Self::Wheel {
                horizontal,
                vertical,
            } => (horizontal.unsigned_abs() > 100 || vertical.unsigned_abs() > 100)
                .then_some("scroll input too large"),
            Self::PhysicalKey { usage, .. } => (!PHYSICAL_KEY_USAGES.contains(usage))
                .then_some("physical key outside the keyboard page"),
            Self::Button { .. } | Self::Key { .. } | Self::Lock {} => None,
        };
        refused.map_or(Ok(()), |reason| Err(error(reason)))
    }

    /// `text` as the host types it, in order: runs of characters, and one
    /// Enter for each `\n`, `\r` or `\r\n`, one Tab for each `\t`. Every
    /// platform types text this way, so none sees a control character twice,
    /// as a key and as a character.
    pub(crate) fn typed(text: &str, mut each: impl FnMut(Typed<'_>) -> Result<()>) -> Result<()> {
        let (mut run, mut after_cr) = (0, false);
        for (at, c) in text.char_indices() {
            let key = match c {
                '\t' => Key::Tab,
                '\r' | '\n' => Key::Enter,
                _ => {
                    after_cr = false;
                    continue;
                }
            };
            if let Some(chars) = text.get(run..at).filter(|chars| !chars.is_empty()) {
                each(Typed::Chars(chars))?;
            }
            run = at + c.len_utf8();
            // The `\n` of a `\r\n` is the line break already typed.
            if !(c == '\n' && after_cr) {
                each(Typed::Key(key))?;
            }
            after_cr = c == '\r';
        }
        match text.get(run..).filter(|chars| !chars.is_empty()) {
            Some(chars) => each(Typed::Chars(chars)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Typed;
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
        let physical = |usage| Input::PhysicalKey { usage, down: true };
        assert!(valid(physical(0x04)) && valid(physical(0xE7)));
        assert!(!valid(physical(0x03)) && !valid(physical(0xE8)));
    }

    /// A line break or tab is one key and never also a character; the `\r`
    /// of a `\r\n` adds nothing.
    #[test]
    fn text_types_each_line_break_and_tab_once() {
        let typed = |text: &str| {
            let mut out = Vec::new();
            Input::typed(text, |piece| {
                out.push(match piece {
                    Typed::Chars(chars) => chars.to_owned(),
                    Typed::Key(key) => format!("<{key:?}>"),
                });
                Ok(())
            })
            .unwrap();
            out.concat()
        };
        assert_eq!(typed("a\nb\tc"), "a<Enter>b<Tab>c");
        assert_eq!(typed("x\r\ny\r\rz\n"), "x<Enter>y<Enter><Enter>z<Enter>");
        assert_eq!(typed("远控 ✓"), "远控 ✓");
    }
}
