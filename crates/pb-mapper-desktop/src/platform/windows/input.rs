//! Injection on Windows: enigo for buttons, the wheel, keys and text,
//! `SetCursorPos` for absolute moves and `LockWorkStation`, with the token
//! diagnostics a rejected injection records.
use crate::{
    Button, DoubleClickGuard, Key, Result, error, input::held::Pressed, platform::Injector,
};
use enigo::{Axis, Direction, Enigo, Keyboard, Mouse};
use windows::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE},
    Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY, TokenIntegrityLevel, TokenSessionId, TokenUIAccess,
    },
    System::{
        Shutdown::LockWorkStation,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
    UI::{
        Input::KeyboardAndMouse::GetDoubleClickTime,
        WindowsAndMessaging::{GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK, SetCursorPos},
    },
};

pub(crate) struct WinInput {
    enigo: Enigo,
}
impl WinInput {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            enigo: Enigo::new(&enigo::Settings::default()).map_err(error)?,
        })
    }
}
impl Injector for WinInput {
    const LOCK: bool = true;
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
        self.enigo
            .key(key_code(key), direction(down))
            .map_err(native_input_error)
    }
    fn text(&mut self, text: &str) -> Result<()> {
        self.enigo.text(text).map_err(native_input_error)
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
            Pressed::Button(button) => self.enigo.button(button_code(button), Direction::Release),
            Pressed::Key(key) => self.enigo.key(key_code(key), Direction::Release),
        }
        .map_err(error)
    }
}

fn native_input_error(failure: impl std::fmt::Display) -> std::io::Error {
    // Capture the native error before querying any token information. Windows
    // does not identify UIPI failures with a distinct code; retain the actual
    // launch context so a failed worker can be diagnosed without guessing or
    // requesting broader privileges. This runs only after failed injection.
    let code = unsafe { GetLastError() }.0;
    let mut token = HANDLE::default();
    let context = (|| -> windows::core::Result<String> {
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }?;
        let mut ui_access = 0u32;
        let mut session = 0u32;
        let mut length = 0;
        unsafe {
            GetTokenInformation(
                token,
                TokenUIAccess,
                Some((&mut ui_access as *mut u32).cast()),
                4,
                &mut length,
            )?;
            GetTokenInformation(
                token,
                TokenSessionId,
                Some((&mut session as *mut u32).cast()),
                4,
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
            let integrity = *GetSidSubAuthority(sid, u32::from(count - 1));
            Ok(format!(
                "session={session}, integrity={integrity:#x}, ui_access={ui_access}"
            ))
        }
    })();
    if !token.is_invalid() {
        unsafe {
            let _ = CloseHandle(token);
        }
    }
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
#[allow(clippy::unwrap_used)]
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
}
