//! Injection on Windows: enigo for buttons, the wheel, keys and text,
//! `SendInput` scancodes for physical keys, `SetCursorPos` for absolute
//! moves and `LockWorkStation`, with the token diagnostics a rejected
//! injection records.
use crate::{
    Button, DoubleClickGuard, Key, Result, error, input::held::Pressed, platform::Injector,
};
use enigo::{Axis, Direction, Enigo, Keyboard, Mouse};
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use windows::Win32::{
    Foundation::{E_UNEXPECTED, GetLastError, HANDLE},
    Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY, TokenIntegrityLevel, TokenSessionId, TokenUIAccess,
    },
    System::{
        Shutdown::LockWorkStation,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
    UI::{
        Input::KeyboardAndMouse::{
            GetDoubleClickTime, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, SendInput, VIRTUAL_KEY,
            VK_NUMLOCK, VK_PAUSE,
        },
        WindowsAndMessaging::{GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK, SetCursorPos},
    },
};

pub(crate) struct WinInput {
    enigo: Enigo,
    keyboard: KeyboardState,
}
impl WinInput {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            enigo: Enigo::new(&enigo::Settings::default()).map_err(error)?,
            keyboard: KeyboardState::default(),
        })
    }
}
impl Injector for WinInput {
    const LOCK: bool = true;
    const PHYSICAL_KEYS: bool = true;
    fn pointer(&mut self, x: i32, y: i32) -> Result<()> {
        // Enigo 0.6 absolute movement normalizes to the primary screen.
        // The Windows API accepts physical virtual-desktop coordinates,
        // including a selected display left/above the primary display.
        unsafe { SetCursorPos(x, y) }.map_err(native_input_error)
    }
    fn button(&mut self, button: Button, down: bool) -> Result<()> {
        self.enigo
            .button(button_code(button), direction(down))
            .map_err(native_input_error)
    }
    fn wheel(&mut self, horizontal: i32, vertical: i32) -> Result<()> {
        if horizontal != 0 {
            self.enigo
                .scroll(horizontal, Axis::Horizontal)
                .map_err(native_input_error)?;
        }
        if vertical != 0 {
            self.enigo
                .scroll(vertical, Axis::Vertical)
                .map_err(native_input_error)?;
        }
        Ok(())
    }
    fn key(&mut self, key: Key, down: bool) -> Result<()> {
        self.keyboard.key(key, down);
        self.enigo
            .key(key_code(key), direction(down))
            .map_err(native_input_error)
    }
    /// enigo 0.6 types a `\n` or `\t` as a key and then again as a Unicode
    /// character; `text` holds none (`Input::typed`).
    fn text(&mut self, text: &str) -> Result<()> {
        self.enigo.text(text).map_err(native_input_error)
    }
    /// As the keyboard would: a scancode, which the layout of the window in
    /// the foreground turns into a virtual key and its input method may
    /// compose.
    fn physical_key(&mut self, usage: u16, down: bool) -> Result<()> {
        self.send_physical(usage, down).map_err(native_input_error)
    }
    fn lock(&mut self) -> Result<()> {
        unsafe { LockWorkStation() }
            .map_err(|e| error(format!("Windows could not lock the workstation: {e}")))
    }
    /// This process runs in the interactive session (the helper's worker does
    /// too), so these are the signed-in user's settings.
    fn double_click(&mut self) -> Option<DoubleClickGuard> {
        let (time_ms, width, height) = unsafe {
            (
                GetDoubleClickTime(),
                GetSystemMetrics(SM_CXDOUBLECLK),
                GetSystemMetrics(SM_CYDOUBLECLK),
            )
        };
        let half = |size: i32| u32::try_from(size).unwrap_or(0) / 2;
        Some(DoubleClickGuard {
            time_ms,
            half_box: (half(width), half(height)),
        })
    }
    /// A release that fails is retried at the next release, quietly.
    fn release(&mut self, pressed: Pressed) -> Result<()> {
        match pressed {
            Pressed::Button(button) => self
                .enigo
                .button(button_code(button), Direction::Release)
                .map_err(error),
            Pressed::Key(key) => {
                self.keyboard.key(key, false);
                self.enigo
                    .key(key_code(key), Direction::Release)
                    .map_err(error)
            }
            Pressed::Physical(usage) => self.send_physical(usage, false).map_err(error),
        }
    }
}
impl WinInput {
    /// One physical key event through `SendInput`; nothing for a usage
    /// with no key here.
    fn send_physical(&mut self, usage: u16, down: bool) -> std::result::Result<(), &'static str> {
        let Some(press) = self.keyboard.physical(usage, down) else {
            return Ok(());
        };
        let flag = |set: bool, flag| if set { flag } else { KEYBD_EVENT_FLAGS(0) };
        let (vk, scan, flags) = match press {
            Press::Scan(scan) => (
                VIRTUAL_KEY(0),
                scan & 0xff,
                KEYEVENTF_SCANCODE | flag(scan >> 8 == 0xe0, KEYEVENTF_EXTENDEDKEY),
            ),
            Press::Virtual(vk, scan, extended) => (vk, scan, flag(extended, KEYEVENTF_EXTENDEDKEY)),
        };
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags | flag(!down, KEYEVENTF_KEYUP),
                    time: 0,
                    dwExtraInfo: self.enigo.get_marker_value(),
                },
            },
        };
        match unsafe { SendInput(&[input], size_of::<INPUT>() as i32) } {
            1 => Ok(()),
            _ => Err("SendInput refused a physical key"),
        }
    }
}

/// How `SendInput` presses a physical key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
    /// A set-1 scancode; `0xe0xx` is an extended one.
    Scan(u16),
    /// A virtual key, its scancode and whether it is extended: Pause, whose
    /// scancode (E1 1D 45) `SendInput` cannot carry, and Num Lock, whose
    /// 0x45 a held Ctrl turns into Pause.
    Virtual(VIRTUAL_KEY, u16, bool),
}

/// Pause alone: E1 1D 45, which `SendInput` cannot carry.
const PAUSE: Press = Press::Virtual(VK_PAUSE, 0x45, false);
/// Pause under a held Ctrl: the keyboard sends Break, E0 46, which every
/// layout maps to `VK_CANCEL`, the key a console takes as Ctrl+Break.
const BREAK: Press = Press::Scan(0xe046);

/// What the keys this injector holds make of the next one, decided as a
/// keyboard decides it from its own keys: a held Ctrl turns Pause into
/// Break. Only keys pressed here count, as only a keyboard's own keys do.
#[derive(Default)]
struct KeyboardState {
    /// The Ctrl keys held: by place, left and right, and the vocabulary's.
    ctrl_left: bool,
    ctrl_right: bool,
    ctrl_key: bool,
    /// How the Pause key held went down, so its up matches.
    pause: Option<Press>,
}
impl KeyboardState {
    /// The vocabulary's `key` went `down`.
    fn key(&mut self, key: Key, down: bool) {
        if key == Key::Control {
            self.ctrl_key = down;
        }
    }

    /// How the physical key `usage` goes `down` now; `None` for a usage
    /// with no key here.
    fn physical(&mut self, usage: u16, down: bool) -> Option<Press> {
        match usage {
            0xe0 => self.ctrl_left = down,
            0xe4 => self.ctrl_right = down,
            0x48 if down => {
                let pause = if self.ctrl_left || self.ctrl_right || self.ctrl_key {
                    BREAK
                } else {
                    PAUSE
                };
                self.pause = Some(pause);
                return Some(pause);
            }
            0x48 => return Some(self.pause.take().unwrap_or(PAUSE)),
            _ => {}
        }
        press(usage)
    }
}

/// The key at keyboard-page `usage`, as Flutter's Windows embedding and
/// Chromium map set-1 scancodes, but for Lang1 and Lang2; `None` for a
/// usage no PC keyboard has.
fn press(usage: u16) -> Option<Press> {
    let scan = match usage {
        // Letters, in usage order A..Z.
        0x04..=0x1d => [
            0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
            0x18, 0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
        ]
        .get(usize::from(usage - 0x04))
        .copied()?,
        // 1..9, then 0.
        0x1e..=0x27 => usage - 0x1e + 0x02,
        0x28 => 0x1c,                       // Enter
        0x29 => 0x01,                       // Escape
        0x2a => 0x0e,                       // Backspace
        0x2b => 0x0f,                       // Tab
        0x2c => 0x39,                       // Space
        0x2d => 0x0c,                       // Minus
        0x2e => 0x0d,                       // Equal
        0x2f => 0x1a,                       // BracketLeft
        0x30 => 0x1b,                       // BracketRight
        0x31 | 0x32 => 0x2b,                // Backslash, IntlHash
        0x33 => 0x27,                       // Semicolon
        0x34 => 0x28,                       // Quote
        0x35 => 0x29,                       // Backquote
        0x36 => 0x33,                       // Comma
        0x37 => 0x34,                       // Period
        0x38 => 0x35,                       // Slash
        0x39 => 0x3a,                       // CapsLock
        0x3a..=0x43 => usage - 0x3a + 0x3b, // F1..F10
        0x44 => 0x57,                       // F11
        0x45 => 0x58,                       // F12
        0x46 => 0xe037,                     // PrintScreen
        0x47 => 0x46,                       // ScrollLock
        0x48 => return Some(PAUSE),
        0x49 => 0xe052, // Insert
        0x4a => 0xe047, // Home
        0x4b => 0xe049, // PageUp
        0x4c => 0xe053, // Delete
        0x4d => 0xe04f, // End
        0x4e => 0xe051, // PageDown
        0x4f => 0xe04d, // ArrowRight
        0x50 => 0xe04b, // ArrowLeft
        0x51 => 0xe050, // ArrowDown
        0x52 => 0xe048, // ArrowUp
        0x53 => return Some(Press::Virtual(VK_NUMLOCK, 0x45, true)),
        0x54 => 0xe035, // NumpadDivide
        0x55 => 0x37,   // NumpadMultiply
        0x56 => 0x4a,   // NumpadSubtract
        0x57 => 0x4e,   // NumpadAdd
        0x58 => 0xe01c, // NumpadEnter
        // Numpad 1..9, then 0 and the decimal point.
        0x59..=0x63 => [
            0x4f, 0x50, 0x51, 0x4b, 0x4c, 0x4d, 0x47, 0x48, 0x49, 0x52, 0x53,
        ]
        .get(usize::from(usage - 0x59))
        .copied()?,
        0x64 => 0x56,                       // IntlBackslash
        0x65 => 0xe05d,                     // ContextMenu
        0x66 => 0xe05e,                     // Power
        0x67 => 0x59,                       // NumpadEqual
        0x68..=0x72 => usage - 0x68 + 0x64, // F13..F23
        0x73 => 0x76,                       // F24
        0x7f => 0xe020,                     // AudioVolumeMute
        0x80 => 0xe030,                     // AudioVolumeUp
        0x81 => 0xe02e,                     // AudioVolumeDown
        0x85 => 0x7e,                       // NumpadComma
        0x87 => 0x73,                       // IntlRo
        0x88 => 0x70,                       // KanaMode
        0x89 => 0x7d,                       // IntlYen
        0x8a => 0x79,                       // Convert
        0x8b => 0x7b,                       // NonConvert
        // Lang1 and Lang2 are E0 F2 and E0 F1, where Windows' layouts put
        // them: Hangul and Hanja in the Korean ones (kbd101a, b, c, kbd103),
        // IME on and off in the Japanese ones. Their plain 0x72 and 0x71,
        // as Chromium names them, are no key there and VK_OEM_RESET.
        0x90 => 0xe0f2,
        0x91 => 0xe0f1,
        0x92 => 0x78,   // Lang3: Katakana
        0x93 => 0x77,   // Lang4: Hiragana
        0xe0 => 0x1d,   // ControlLeft
        0xe1 => 0x2a,   // ShiftLeft
        0xe2 => 0x38,   // AltLeft
        0xe3 => 0xe05b, // MetaLeft
        0xe4 => 0xe01d, // ControlRight
        0xe5 => 0x36,   // ShiftRight
        0xe6 => 0xe038, // AltRight
        0xe7 => 0xe05c, // MetaRight
        _ => return None,
    };
    Some(Press::Scan(scan))
}

fn native_input_error(failure: impl std::fmt::Display) -> std::io::Error {
    // Capture the native error before querying any token information. Windows
    // does not identify UIPI failures with a distinct code; retain the actual
    // launch context so a failed worker can be diagnosed without guessing or
    // requesting broader privileges. This runs only after failed injection.
    let code = unsafe { GetLastError() }.0;
    let context = (|| -> windows::core::Result<String> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }?;
        // Closed once the context is read.
        let _token = unsafe { OwnedHandle::from_raw_handle(token.0) };
        let mut ui_access = 0u32;
        let mut session = 0u32;
        let mut length = 0;
        unsafe {
            GetTokenInformation(
                token,
                TokenUIAccess,
                Some((&mut ui_access as *mut u32).cast()),
                size_of::<u32>() as u32,
                &mut length,
            )?;
            GetTokenInformation(
                token,
                TokenSessionId,
                Some((&mut session as *mut u32).cast()),
                size_of::<u32>() as u32,
                &mut length,
            )?;
        }
        let mut label = [0usize; 16];
        unsafe {
            GetTokenInformation(
                token,
                TokenIntegrityLevel,
                Some(label.as_mut_ptr().cast()),
                size_of_val(&label) as u32,
                &mut length,
            )?;
            let sid = (*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid;
            let count = *GetSidSubAuthorityCount(sid);
            let last = count.checked_sub(1).ok_or_else(|| {
                windows::core::Error::new(E_UNEXPECTED, "integrity label without a level")
            })?;
            let integrity = *GetSidSubAuthority(sid, u32::from(last));
            Ok(format!(
                "session={session}, integrity={integrity:#x}, ui_access={ui_access}"
            ))
        }
    })();
    let detail = format!(
        "{failure}; Win32={code}; {}",
        context.unwrap_or_else(|e| format!("worker token unavailable: {e}"))
    );
    #[cfg(feature = "windows-helper")]
    super::helper::record_input_failure(&detail);
    tracing::warn!(%detail, "native desktop input failed");
    error(format!(
        "Windows rejected remote input (Win32={code}); check the desktop service log"
    ))
}

fn direction(down: bool) -> Direction {
    if down {
        Direction::Press
    } else {
        Direction::Release
    }
}

fn button_code(button: Button) -> enigo::Button {
    match button {
        Button::Left => enigo::Button::Left,
        Button::Right => enigo::Button::Right,
        Button::Middle => enigo::Button::Middle,
    }
}
/// Letters and digits are virtual keys, never `enigo::Key::Unicode`: on a host
/// layout that cannot produce the character, enigo falls back to Unicode text
/// with its own down and up, so Ctrl+C or Win+E would type a stray letter.
fn key_code(key: Key) -> enigo::Key {
    use enigo::Key as K;
    match key {
        Key::Shift => K::Shift,
        Key::Control => K::Control,
        Key::Alt => K::Alt,
        Key::Meta => K::Meta,
        Key::Enter => K::Return,
        Key::Escape => K::Escape,
        Key::Tab => K::Tab,
        Key::Backspace => K::Backspace,
        Key::Delete => K::Delete,
        Key::Left => K::LeftArrow,
        Key::Right => K::RightArrow,
        Key::Up => K::UpArrow,
        Key::Down => K::DownArrow,
        Key::Home => K::Home,
        Key::End => K::End,
        Key::PageUp => K::PageUp,
        Key::PageDown => K::PageDown,
        Key::Insert => K::Insert,
        Key::Space => K::Space,
        Key::A => K::A,
        Key::B => K::B,
        Key::C => K::C,
        Key::D => K::D,
        Key::E => K::E,
        Key::F => K::F,
        Key::G => K::G,
        Key::H => K::H,
        Key::I => K::I,
        Key::J => K::J,
        Key::K => K::K,
        Key::L => K::L,
        Key::M => K::M,
        Key::N => K::N,
        Key::O => K::O,
        Key::P => K::P,
        Key::Q => K::Q,
        Key::R => K::R,
        Key::S => K::S,
        Key::T => K::T,
        Key::U => K::U,
        Key::V => K::V,
        Key::W => K::W,
        Key::X => K::X,
        Key::Y => K::Y,
        Key::Z => K::Z,
        Key::Digit0 => K::Num0,
        Key::Digit1 => K::Num1,
        Key::Digit2 => K::Num2,
        Key::Digit3 => K::Num3,
        Key::Digit4 => K::Num4,
        Key::Digit5 => K::Num5,
        Key::Digit6 => K::Num6,
        Key::Digit7 => K::Num7,
        Key::Digit8 => K::Num8,
        Key::Digit9 => K::Num9,
        Key::F1 => K::F1,
        Key::F2 => K::F2,
        Key::F3 => K::F3,
        Key::F4 => K::F4,
        Key::F5 => K::F5,
        Key::F6 => K::F6,
        Key::F7 => K::F7,
        Key::F8 => K::F8,
        Key::F9 => K::F9,
        Key::F10 => K::F10,
        Key::F11 => K::F11,
        Key::F12 => K::F12,
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn key_code_uses_virtual_keys_for_letters_and_digits() {
        let named = [
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
        ]
        .map(String::from);
        let letters = ('a'..='z').map(String::from);
        let digits = (0..=9).map(|digit| format!("digit{digit}"));
        let functions = (1..=12).map(|n| format!("f{n}"));
        let codes: Vec<_> = named
            .into_iter()
            .chain(letters)
            .chain(digits)
            .chain(functions)
            .map(|name| key_code(serde_json::from_value(serde_json::json!(name)).unwrap()))
            .collect();
        assert_eq!(codes.len(), 19 + 26 + 10 + 12);
        assert!(
            codes
                .iter()
                .all(|code| !matches!(code, enigo::Key::Unicode(_)))
        );
        // Distinct: no two keys of the vocabulary press the same host key.
        assert!(
            codes
                .iter()
                .enumerate()
                .all(|(i, code)| !codes[..i].contains(code))
        );
        assert_eq!(key_code(Key::A), enigo::Key::A);
        assert_eq!(key_code(Key::Z), enigo::Key::Z);
        assert_eq!(key_code(Key::Digit0), enigo::Key::Num0);
        assert_eq!(key_code(Key::Digit9), enigo::Key::Num9);
    }

    /// The ends of each computed run, the extended keys and the two keys
    /// `SendInput` cannot carry as a scancode; no two usages press the same
    /// key but Backslash and IntlHash, which a PC keyboard shares.
    #[test]
    fn physical_keys_are_set_one_scancodes() {
        for (usage, scan) in [
            (0x04, 0x1e),   // A
            (0x1d, 0x2c),   // Z
            (0x1e, 0x02),   // 1
            (0x27, 0x0b),   // 0
            (0x3a, 0x3b),   // F1
            (0x43, 0x44),   // F10
            (0x59, 0x4f),   // Numpad1
            (0x63, 0x53),   // NumpadDecimal
            (0x68, 0x64),   // F13
            (0x72, 0x6e),   // F23
            (0x50, 0xe04b), // ArrowLeft
            (0x58, 0xe01c), // NumpadEnter
            (0xe3, 0xe05b), // MetaLeft
            (0xe4, 0xe01d), // ControlRight
            (0x90, 0xe0f2), // Lang1
            (0x91, 0xe0f1), // Lang2
        ] {
            assert_eq!(press(usage), Some(Press::Scan(scan)), "usage {usage:#x}");
        }
        assert_eq!(press(0x48), Some(Press::Virtual(VK_PAUSE, 0x45, false)));
        assert_eq!(press(0x74), None);
        let pressed: Vec<_> = crate::input::PHYSICAL_KEY_USAGES
            .filter(|usage| *usage != 0x32)
            .filter_map(press)
            .collect();
        assert!(
            pressed
                .iter()
                .enumerate()
                .all(|(i, press)| !pressed[..i].contains(press))
        );
    }

    /// Pause under a Ctrl held here, either one by place or the
    /// vocabulary's, is Break; its up matches its down whatever Ctrl did
    /// meanwhile.
    #[test]
    fn a_held_ctrl_makes_pause_break() {
        let mut keyboard = KeyboardState::default();
        assert_eq!(keyboard.physical(0x48, true), Some(PAUSE));
        assert_eq!(keyboard.physical(0x48, false), Some(PAUSE));
        keyboard.physical(0xe4, true);
        assert_eq!(keyboard.physical(0x48, true), Some(BREAK));
        keyboard.physical(0xe4, false);
        assert_eq!(keyboard.physical(0x48, false), Some(BREAK));
        keyboard.key(Key::Control, true);
        assert_eq!(keyboard.physical(0x48, true), Some(BREAK));
        keyboard.key(Key::Control, false);
        assert_eq!(keyboard.physical(0x48, false), Some(BREAK));
        assert_eq!(keyboard.physical(0x48, true), Some(PAUSE));
    }
}
