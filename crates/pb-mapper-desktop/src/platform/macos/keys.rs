//! Keys as macOS posts them: the virtual key codes of `Events.h` (`kVK_*`),
//! which name a key's position on an ANSI keyboard whatever the layout, so
//! a shortcut works on any host layout; the modifier flags each modifier
//! sets; and text cut into the runs one keyboard event carries.

use crate::Key;

/// `kVK_Return`, `kVK_Tab`: what a newline and a tab in text press.
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

/// The modifier flags (`kCGEventFlagMask*`) of Shift, Control, Option and
/// Command.
pub(crate) const SHIFT: u64 = 0x2_0000;
pub(crate) const CONTROL: u64 = 0x4_0000;
pub(crate) const OPTION: u64 = 0x8_0000;
pub(crate) const COMMAND: u64 = 0x10_0000;

/// The flag a modifier key sets while it is down; `None` for any other key.
pub(crate) fn modifier(code: u16) -> Option<u64> {
    match code {
        0x38 | 0x3c => Some(SHIFT),
        0x3b | 0x3e => Some(CONTROL),
        0x3a | 0x3d => Some(OPTION),
        0x37 | 0x36 => Some(COMMAND),
        _ => None,
    }
}

/// The virtual key of the physical key at USB HID keyboard-page `usage`
/// (`0x07`), for a viewer's physical keys; `None` for a key a Mac keyboard
/// lacks (Application, Power).
#[cfg_attr(
    all(target_os = "macos", not(test)),
    expect(
        dead_code,
        reason = "physical keys reach the injector in a later change"
    )
)]
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

/// One keyboard event's worth of text: up to [`TEXT_RUN`] UTF-16 units, or
/// a key that text presses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Run {
    Text(Vec<u16>),
    Key(u16),
}

/// `text` as runs: a newline is one Return and a tab one Tab, a carriage
/// return is Return unless a newline follows it (`\r\n` is one Return), and
/// the rest is cut into runs that never split a surrogate pair.
pub(crate) fn runs(text: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let key = match c {
            '\n' => Some(RETURN),
            '\t' => Some(TAB),
            '\r' if chars.peek() == Some(&'\n') => continue,
            '\r' => Some(RETURN),
            _ => None,
        };
        if let Some(key) = key {
            if !current.is_empty() {
                runs.push(Run::Text(std::mem::take(&mut current)));
            }
            runs.push(Run::Key(key));
            continue;
        }
        let mut units = [0; 2];
        let units = c.encode_utf16(&mut units);
        if current.len() + units.len() > TEXT_RUN {
            runs.push(Run::Text(std::mem::take(&mut current)));
        }
        current.extend_from_slice(units);
    }
    if !current.is_empty() {
        runs.push(Run::Text(current));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            [Key::Shift, Key::Control, Key::Alt, Key::Meta].map(|key| modifier(key_code(key))),
            [Some(SHIFT), Some(CONTROL), Some(OPTION), Some(COMMAND)]
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
        assert_eq!(usage_code(0xe7).and_then(modifier), Some(COMMAND));
        assert_eq!((usage_code(0x65), usage_code(0x00)), (None, None));
    }

    /// Text presses one Return per line break, `\r\n` included, and one Tab
    /// per tab; runs never exceed an event's length or split a surrogate
    /// pair.
    #[test]
    fn text_runs_press_line_breaks_and_keep_surrogate_pairs() {
        assert_eq!(
            runs("a\r\nb\tc\rd\n"),
            [
                Run::Text(vec![u16::from(b'a')]),
                Run::Key(RETURN),
                Run::Text(vec![u16::from(b'b')]),
                Run::Key(TAB),
                Run::Text(vec![u16::from(b'c')]),
                Run::Key(RETURN),
                Run::Text(vec![u16::from(b'd')]),
                Run::Key(RETURN),
            ]
        );
        // 19 letters then an emoji (two units): the pair starts the next run.
        let text = format!("{}\u{1f600}", "x".repeat(19));
        let cut = runs(&text);
        assert!(matches!(&cut[..], [Run::Text(a), Run::Text(b)] if a.len() == 19 && b.len() == 2));
        let joined: Vec<u16> = cut
            .iter()
            .flat_map(|run| match run {
                Run::Text(units) => units.clone(),
                Run::Key(_) => Vec::new(),
            })
            .collect();
        assert_eq!(String::from_utf16(&joined).unwrap(), text);
        assert!(runs("").is_empty());
    }
}
