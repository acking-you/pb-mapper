//! Keys as macOS posts them: the virtual key codes of `Events.h` (`kVK_*`),
//! which name a key's place on an ANSI keyboard whatever the layout. A named
//! letter or digit of the vocabulary goes to the key that types it in the
//! Mac's layout ([`Layout`]), as a Windows virtual key does: macOS matches a
//! shortcut by the character a place types, and Command at the ANSI A place
//! is Command-Q on a French AZERTY layout. Every other key, and the physical
//! keys of a viewer's keyboard by their USB HID usage, go by place. Also the
//! flags the modifiers held set on every posted event, and text cut into the
//! runs one keyboard event carries.

use crate::Key;
use std::ops::RangeInclusive;

/// `kVK_Return`, `kVK_Tab`.
pub(crate) const RETURN: u16 = 0x24;
pub(crate) const TAB: u16 = 0x30;

/// The most UTF-16 code units one event's Unicode string carries; apps read
/// no more than this from a single event.
pub(crate) const TEXT_RUN: usize = 20;

/// The virtual key `key` presses. `Meta` is Command; `Insert`, which Mac
/// keyboards lack, is Help, the key in its place on an extended keyboard.
pub(crate) fn key_code(key: Key) -> u16 {
    match key {
        Key::Shift => 0x38,
        Key::Control => 0x3b,
        Key::Alt => 0x3a,
        Key::Meta => 0x37,
        Key::Enter => RETURN,
        Key::Escape => 0x35,
        Key::Tab => TAB,
        Key::Backspace => 0x33,
        Key::Delete => 0x75,
        Key::Left => 0x7b,
        Key::Right => 0x7c,
        Key::Down => 0x7d,
        Key::Up => 0x7e,
        Key::Home => 0x73,
        Key::End => 0x77,
        Key::PageUp => 0x74,
        Key::PageDown => 0x79,
        Key::Insert => 0x72,
        Key::Space => 0x31,
        Key::A => 0x00,
        Key::S => 0x01,
        Key::D => 0x02,
        Key::F => 0x03,
        Key::H => 0x04,
        Key::G => 0x05,
        Key::Z => 0x06,
        Key::X => 0x07,
        Key::C => 0x08,
        Key::V => 0x09,
        Key::B => 0x0b,
        Key::Q => 0x0c,
        Key::W => 0x0d,
        Key::E => 0x0e,
        Key::R => 0x0f,
        Key::Y => 0x10,
        Key::T => 0x11,
        Key::O => 0x1f,
        Key::U => 0x20,
        Key::I => 0x22,
        Key::P => 0x23,
        Key::L => 0x25,
        Key::J => 0x26,
        Key::K => 0x28,
        Key::N => 0x2d,
        Key::M => 0x2e,
        Key::Digit1 => 0x12,
        Key::Digit2 => 0x13,
        Key::Digit3 => 0x14,
        Key::Digit4 => 0x15,
        Key::Digit6 => 0x16,
        Key::Digit5 => 0x17,
        Key::Digit9 => 0x19,
        Key::Digit7 => 0x1a,
        Key::Digit8 => 0x1c,
        Key::Digit0 => 0x1d,
        Key::F1 => 0x7a,
        Key::F2 => 0x78,
        Key::F3 => 0x63,
        Key::F4 => 0x76,
        Key::F5 => 0x60,
        Key::F6 => 0x61,
        Key::F7 => 0x62,
        Key::F8 => 0x64,
        Key::F9 => 0x65,
        Key::F10 => 0x6d,
        Key::F11 => 0x67,
        Key::F12 => 0x6f,
    }
}

/// The named letters and digits, each with the character it names.
const NAMED: [(Key, char); 36] = [
    (Key::A, 'a'),
    (Key::B, 'b'),
    (Key::C, 'c'),
    (Key::D, 'd'),
    (Key::E, 'e'),
    (Key::F, 'f'),
    (Key::G, 'g'),
    (Key::H, 'h'),
    (Key::I, 'i'),
    (Key::J, 'j'),
    (Key::K, 'k'),
    (Key::L, 'l'),
    (Key::M, 'm'),
    (Key::N, 'n'),
    (Key::O, 'o'),
    (Key::P, 'p'),
    (Key::Q, 'q'),
    (Key::R, 'r'),
    (Key::S, 's'),
    (Key::T, 't'),
    (Key::U, 'u'),
    (Key::V, 'v'),
    (Key::W, 'w'),
    (Key::X, 'x'),
    (Key::Y, 'y'),
    (Key::Z, 'z'),
    (Key::Digit0, '0'),
    (Key::Digit1, '1'),
    (Key::Digit2, '2'),
    (Key::Digit3, '3'),
    (Key::Digit4, '4'),
    (Key::Digit5, '5'),
    (Key::Digit6, '6'),
    (Key::Digit7, '7'),
    (Key::Digit8, '8'),
    (Key::Digit9, '9'),
];

/// Whether `key` is a named letter or digit, which goes through a layout.
pub(crate) fn named(key: Key) -> bool {
    NAMED.iter().any(|(named, _)| *named == key)
}

/// The places that type characters: a Mac keyboard's main block, without
/// the keypad and the function keys.
const TYPING: RangeInclusive<u16> = 0x00..=0x32;

/// The key each named letter and digit presses in one keyboard layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout([u16; NAMED.len()]);

impl Layout {
    /// The ANSI places: what a layout that cannot be read gets.
    pub(crate) fn ansi() -> Self {
        Self(NAMED.map(|(key, _)| key_code(key)))
    }

    /// A layout whose place `code` types `typed(code, shifted)`. Each named
    /// key goes to its ANSI place where that types its character, else to
    /// the first place that does, unshifted before shifted (a French
    /// layout's digits are shifted); where none does, as on a layout of
    /// another script, to its ANSI place.
    pub(crate) fn of(typed: impl Fn(u16, bool) -> Option<char>) -> Self {
        let mut layout = Self::ansi();
        for (code, (_, named)) in layout.0.iter_mut().zip(NAMED) {
            let types = |place: u16, shifted: bool| {
                typed(place, shifted).map(|typed| typed.to_ascii_lowercase()) == Some(named)
            };
            let ansi = *code;
            let found = [false, true].into_iter().find_map(|shifted| {
                if types(ansi, shifted) {
                    return Some(ansi);
                }
                TYPING.clone().find(|place| types(*place, shifted))
            });
            if let Some(found) = found {
                *code = found;
            }
        }
        layout
    }

    /// The virtual key `key` presses: a named letter or digit through this
    /// layout, any other key at its place.
    pub(crate) fn code(&self, key: Key) -> u16 {
        NAMED
            .iter()
            .position(|(named, _)| *named == key)
            .map_or_else(|| key_code(key), |at| self.0[at])
    }
}

/// The modifier flags (`kCGEventFlagMask*`) of Shift, Control, Option,
/// Command and Caps Lock.
pub(crate) const SHIFT: u64 = 0x2_0000;
pub(crate) const CONTROL: u64 = 0x4_0000;
pub(crate) const OPTION: u64 = 0x8_0000;
pub(crate) const COMMAND: u64 = 0x10_0000;
pub(crate) const CAPS_LOCK: u64 = 0x1_0000;
/// `kVK_CapsLock`.
const CAPS_LOCK_KEY: u16 = 0x39;

/// The modifier keys, left then right, with the flags each sets while it is
/// down: its modifier's, and its side's device bit (`NX_DEVICE*KEYMASK`),
/// which apps that tell left from right read.
const MODIFIERS: [(u16, u64); 8] = [
    (0x3b, CONTROL | 0x01),
    (0x38, SHIFT | 0x02),
    (0x3a, OPTION | 0x20),
    (0x37, COMMAND | 0x08),
    (0x3e, CONTROL | 0x2000),
    (0x3c, SHIFT | 0x04),
    (0x3d, OPTION | 0x40),
    (0x36, COMMAND | 0x10),
];

/// The modifier keys held down and Caps Lock's state, as every posted event
/// carries them: posted events do not take them from the keys before them.
#[derive(Clone, Copy, Default)]
pub(crate) struct Modifiers {
    /// One bit per entry of `MODIFIERS`.
    held: u8,
    caps_lock: bool,
}

impl Modifiers {
    /// Account for `code` going `down`. Whether it is a modifier, which
    /// posts as a flags change: Caps Lock turns on and off at each press.
    pub(crate) fn update(&mut self, code: u16, down: bool) -> bool {
        if code == CAPS_LOCK_KEY {
            self.caps_lock ^= down;
            return true;
        }
        let Some(at) = MODIFIERS.iter().position(|(modifier, _)| *modifier == code) else {
            return false;
        };
        if down {
            self.held |= 1 << at;
        } else {
            self.held &= !(1 << at);
        }
        true
    }

    /// The flags of everything held: a modifier stays while either of its
    /// keys is down.
    pub(crate) fn flags(&self) -> u64 {
        let held = MODIFIERS
            .iter()
            .enumerate()
            .filter(|(at, _)| self.held & (1 << at) != 0)
            .fold(0, |flags, (_, (_, flag))| flags | flag);
        if self.caps_lock {
            held | CAPS_LOCK
        } else {
            held
        }
    }
}

/// The virtual key of the physical key at USB HID keyboard-page `usage`
/// (`0x07`), for a viewer's physical keys; `None` for a key a Mac keyboard
/// lacks (Application, Power).
pub(crate) fn usage_code(usage: u16) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f,
        0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06,
    ];
    // 1 to 9, then 0.
    const DIGITS: [u16; 10] = [0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19, 0x1d];
    // F1 to F12.
    const F1_F12: [u16; 12] = [
        0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f,
    ];
    // F13 to F20.
    const F13_F20: [u16; 8] = [0x69, 0x6b, 0x71, 0x6a, 0x40, 0x4f, 0x50, 0x5a];
    // Keypad 1 to 9, then 0.
    const KEYPAD: [u16; 10] = [0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5b, 0x5c, 0x52];
    let index = |first: u16| usize::from(usage - first);
    Some(match usage {
        0x04..=0x1d => LETTERS[index(0x04)],
        0x1e..=0x27 => DIGITS[index(0x1e)],
        0x28 => RETURN,
        0x29 => 0x35,
        0x2a => 0x33,
        0x2b => TAB,
        0x2c => 0x31,
        0x2d => 0x1b,
        0x2e => 0x18,
        0x2f => 0x21,
        0x30 => 0x1e,
        // Backslash, and the ISO key beside Return, which Apple maps to it.
        0x31 | 0x32 => 0x2a,
        0x33 => 0x29,
        0x34 => 0x27,
        0x35 => 0x32,
        0x36 => 0x2b,
        0x37 => 0x2f,
        0x38 => 0x2c,
        0x39 => 0x39,
        0x3a..=0x45 => F1_F12[index(0x3a)],
        // Print Screen, Scroll Lock and Pause sit where F13 to F15 are.
        0x46 => 0x69,
        0x47 => 0x6b,
        0x48 => 0x71,
        0x49 => 0x72,
        0x4a => 0x73,
        0x4b => 0x74,
        0x4c => 0x75,
        0x4d => 0x77,
        0x4e => 0x79,
        0x4f => 0x7c,
        0x50 => 0x7b,
        0x51 => 0x7d,
        0x52 => 0x7e,
        // Num Lock is the keypad's Clear.
        0x53 => 0x47,
        0x54 => 0x4b,
        0x55 => 0x43,
        0x56 => 0x4e,
        0x57 => 0x45,
        0x58 => 0x4c,
        0x59..=0x62 => KEYPAD[index(0x59)],
        0x63 => 0x41,
        0x64 => 0x0a,
        0x67 => 0x51,
        0x68..=0x6f => F13_F20[index(0x68)],
        0x75 => 0x72,
        0x7f => 0x4a,
        0x80 => 0x48,
        0x81 => 0x49,
        0x85 => 0x5f,
        0x87 => 0x5e,
        0x89 => 0x5d,
        0x90 => 0x68,
        0x91 => 0x66,
        0xe0 => 0x3b,
        0xe1 => 0x38,
        0xe2 => 0x3a,
        0xe3 => 0x37,
        0xe4 => 0x3e,
        0xe5 => 0x3c,
        0xe6 => 0x3d,
        0xe7 => 0x36,
        _ => return None,
    })
}

/// `text` cut into the UTF-16 runs one keyboard event each carries: at
/// most [`TEXT_RUN`] units, never splitting a surrogate pair. Line breaks
/// and tabs never reach here; they are typed as keys.
pub(crate) fn runs(text: &str) -> Vec<Vec<u16>> {
    let mut runs = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    for c in text.chars() {
        let mut units = [0; 2];
        let units = c.encode_utf16(&mut units);
        if current.len() + units.len() > TEXT_RUN {
            runs.push(std::mem::take(&mut current));
        }
        current.extend_from_slice(units);
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flags `code` sets alone; `None` for a key that is no modifier.
    fn modifier(code: u16) -> Option<u64> {
        let mut held = Modifiers::default();
        held.update(code, true).then(|| held.flags())
    }

    /// In a French AZERTY layout, Select All's A is the key at the ANSI Q
    /// place, and Q, Z, W and M follow their letters; the digits stay at
    /// their places, shifted there. A layout of another script types no
    /// Latin letter, and every named key keeps its ANSI place.
    #[test]
    fn named_letters_follow_the_layout_and_fall_back_to_their_places() {
        let azerty = |code: u16, shifted: bool| {
            let (plain, shift) = match code {
                0x00 => ('q', 'Q'),
                0x0c => ('a', 'A'),
                0x06 => ('w', 'W'),
                0x0d => ('z', 'Z'),
                0x29 => ('m', 'M'),
                0x2e => (',', '?'),
                0x12 => ('&', '1'),
                0x13 => ('é', '2'),
                _ => return None,
            };
            Some(if shifted { shift } else { plain })
        };
        let layout = Layout::of(azerty);
        let codes = [
            Key::A,
            Key::Q,
            Key::Z,
            Key::W,
            Key::M,
            Key::Digit1,
            Key::Digit2,
        ]
        .map(|key| layout.code(key));
        assert_eq!(codes, [0x0c, 0x00, 0x0d, 0x06, 0x29, 0x12, 0x13]);
        assert_eq!(layout.code(Key::Enter), RETURN);

        let cyrillic = |code: u16, _| char::from_u32(0x0430 + u32::from(code));
        assert_eq!(Layout::of(cyrillic), Layout::ansi());
        assert_eq!(Layout::ansi().code(Key::C), 0x08);
    }

    /// Every key of the vocabulary presses its own virtual key, and the four
    /// modifiers set their flags.
    #[test]
    fn every_key_has_its_own_virtual_key() {
        let keys: Vec<Key> = serde_json::from_value(serde_json::json!([
            "shift",
            "control",
            "alt",
            "meta",
            "enter",
            "escape",
            "tab",
            "backspace",
            "delete",
            "left",
            "right",
            "up",
            "down",
            "home",
            "end",
            "page_up",
            "page_down",
            "insert",
            "space",
            "a",
            "b",
            "c",
            "d",
            "e",
            "f",
            "g",
            "h",
            "i",
            "j",
            "k",
            "l",
            "m",
            "n",
            "o",
            "p",
            "q",
            "r",
            "s",
            "t",
            "u",
            "v",
            "w",
            "x",
            "y",
            "z",
            "digit0",
            "digit1",
            "digit2",
            "digit3",
            "digit4",
            "digit5",
            "digit6",
            "digit7",
            "digit8",
            "digit9",
            "f1",
            "f2",
            "f3",
            "f4",
            "f5",
            "f6",
            "f7",
            "f8",
            "f9",
            "f10",
            "f11",
            "f12"
        ]))
        .unwrap();
        let codes: Vec<u16> = keys.iter().map(|key| key_code(*key)).collect();
        assert_eq!(codes.len(), 67);
        assert!(
            codes
                .iter()
                .enumerate()
                .all(|(i, code)| !codes[..i].contains(code))
        );
        // Meta is Command and Control is Control: the Apple swap relies on it.
        assert_eq!(
            [Key::Shift, Key::Control, Key::Alt, Key::Meta].map(|key| modifier(key_code(key))),
            [
                Some(SHIFT | 0x02),
                Some(CONTROL | 0x01),
                Some(OPTION | 0x20),
                Some(COMMAND | 0x08)
            ]
        );
        assert_eq!(modifier(key_code(Key::A)), None);
    }

    /// The physical keys of a keyboard map as the vocabulary does where both
    /// name them, and a key a Mac lacks maps to nothing.
    #[test]
    fn physical_keys_match_the_vocabulary() {
        let pairs = [
            (0x04, Key::A),
            (0x1d, Key::Z),
            (0x1e, Key::Digit1),
            (0x27, Key::Digit0),
            (0x28, Key::Enter),
            (0x2a, Key::Backspace),
            (0x3a, Key::F1),
            (0x45, Key::F12),
            (0x49, Key::Insert),
            (0x4c, Key::Delete),
            (0x52, Key::Up),
            (0xe0, Key::Control),
            (0xe3, Key::Meta),
        ];
        for (usage, key) in pairs {
            assert_eq!(usage_code(usage), Some(key_code(key)), "usage {usage:#x}");
        }
        assert_eq!(usage_code(0xe7).and_then(modifier), Some(COMMAND | 0x10));
        assert_eq!((usage_code(0x65), usage_code(0x00)), (None, None));
    }

    /// Left and right modifiers are held apart: a chord keeps its flag while
    /// either key is down, each side sets its own device bit, and Caps Lock
    /// turns on and off at each press.
    #[test]
    fn modifiers_carry_their_flags_while_either_side_is_down() {
        let (left, right) = (usage_code(0xe1).unwrap(), usage_code(0xe5).unwrap());
        let mut held = Modifiers::default();
        assert!(held.update(left, true) && held.update(right, true));
        assert!(held.update(left, false));
        assert_eq!(held.flags(), SHIFT | 0x04);
        assert!(held.update(right, false));
        assert_eq!(held.flags(), 0);
        // Cmd+C: the C goes with Command's flag, and is no modifier itself.
        assert!(held.update(key_code(Key::Meta), true));
        assert!(!held.update(key_code(Key::C), true));
        assert_eq!(held.flags() & COMMAND, COMMAND);
        let caps = usage_code(0x39).unwrap();
        assert!(held.update(caps, true) && held.update(caps, false));
        assert_eq!(held.flags() & CAPS_LOCK, CAPS_LOCK);
        held.update(caps, true);
        assert_eq!(held.flags() & CAPS_LOCK, 0);
    }

    /// Runs never exceed an event's length or split a surrogate pair.
    #[test]
    fn text_runs_keep_surrogate_pairs() {
        assert_eq!(runs("ab"), [vec![u16::from(b'a'), u16::from(b'b')]]);
        // 19 letters then an emoji (two units): the pair starts the next run.
        let text = format!("{}😀", "x".repeat(19));
        let cut = runs(&text);
        assert!(matches!(&cut[..], [a, b] if a.len() == 19 && b.len() == 2));
        assert_eq!(String::from_utf16(&cut.concat()).unwrap(), text);
        assert!(runs("").is_empty());
    }
}
