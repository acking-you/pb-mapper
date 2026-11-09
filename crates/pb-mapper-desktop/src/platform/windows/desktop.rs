//! Uses published Apache-2.0 xcap and MIT enigo. No RustDesk source is copied.
use crate::input::held::{Held, Pressed};
use crate::{
    Backend, BackendError, Button, CursorPosition, DesktopContext, DesktopKind, Display,
    FailureKind, Input, Key, Result, error,
};
use enigo::{Axis, Direction, Enigo, Keyboard, Mouse};
use windows::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE},
    Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFOEXW},
    Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY, TokenIntegrityLevel, TokenSessionId, TokenUIAccess,
    },
    System::{
        Shutdown::LockWorkStation,
        StationsAndDesktops::{
            CloseDesktop, DESKTOP_READOBJECTS, GetUserObjectInformationW, OpenInputDesktop,
            UOI_NAME,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
    UI::{
        HiDpi::{
            DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            SetThreadDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::GetDoubleClickTime,
        WindowsAndMessaging::{
            CURSOR_SHOWING, CURSOR_SUPPRESSED, CURSORINFO, GetCursorInfo, GetSystemMetrics,
            SM_CXDOUBLECLK, SM_CYDOUBLECLK, SetCursorPos,
        },
    },
};

struct DpiScope(DPI_AWARENESS_CONTEXT);
impl DpiScope {
    fn enter() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}
impl Drop for DpiScope {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

fn available() -> Result<()> {
    // An ordinary process must never treat its cached Default desktop as live
    // while input belongs to Winlogon/UAC or another inaccessible desktop.
    unsafe {
        let desk = OpenInputDesktop(Default::default(), false, DESKTOP_READOBJECTS)
            .map_err(|_| ordinary_desktop_unavailable())?;
        let mut name = [0u16; 256];
        let result = GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            None,
        );
        let _ = CloseDesktop(desk);
        result.map_err(error)?;
        let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
        if !String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("default") {
            return Err(ordinary_desktop_unavailable());
        }
    }
    Ok(())
}
fn ordinary_desktop_unavailable() -> std::io::Error {
    BackendError::new(
        FailureKind::SecureDesktop,
        "secure_desktop_requires_helper: This sharing session uses ordinary desktop access. UAC and the lock screen require the authorized Windows service. Prepare full desktop access on the host and restart sharing; otherwise the picture resumes when the ordinary desktop returns.",
    )
    .into()
}
fn display_lost(message: &str) -> std::io::Error {
    BackendError::new(FailureKind::DisplayLost, message).into()
}
fn describe(monitor: &xcap::Monitor) -> Result<Display> {
    Ok(Display {
        id: monitor.id().map_err(error)?,
        name: monitor.name().map_err(error)?,
        x: monitor.x().map_err(error)?,
        y: monitor.y().map_err(error)?,
        width: monitor.width().map_err(error)?,
        height: monitor.height().map_err(error)?,
    })
}
fn monitor_handle(id: u32) -> Result<HMONITOR> {
    windows_capture::monitor::Monitor::enumerate()
        .map_err(error)?
        .into_iter()
        .find(|m| m.as_raw_hmonitor() as usize as u32 == id)
        .map(|m| HMONITOR(m.as_raw_hmonitor()))
        .ok_or_else(|| display_lost("selected display is no longer available"))
}
fn current_display(id: u32, monitor: HMONITOR) -> Result<Display> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()).ok() }
        .map_err(error)?;
    let rect = info.monitorInfo.rcMonitor;
    let width = u32::try_from(rect.right - rect.left).map_err(error)?;
    let height = u32::try_from(rect.bottom - rect.top).map_err(error)?;
    if width == 0 || height == 0 {
        return Err(display_lost("display is unavailable"));
    }
    let length = info
        .szDevice
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(info.szDevice.len());
    Ok(Display {
        id,
        name: String::from_utf16_lossy(&info.szDevice[..length]),
        x: rect.left,
        y: rect.top,
        width,
        height,
    })
}
pub(crate) fn displays() -> Result<Vec<Display>> {
    let _dpi = DpiScope::enter();
    available()?;
    xcap::Monitor::all()
        .map_err(error)?
        .iter()
        .map(describe)
        .collect()
}
/// The primary display, or the first when Windows names none.
pub(crate) fn primary() -> Result<Display> {
    let _dpi = DpiScope::enter();
    available()?;
    let monitors = xcap::Monitor::all().map_err(error)?;
    monitors
        .iter()
        .find(|monitor| monitor.is_primary().unwrap_or(false))
        .or(monitors.first())
        .ok_or_else(|| display_lost("no display is available"))
        .and_then(describe)
}
/// The encoder's limits until `configure_video`, which every session calls
/// before it captures.
const UNCONFIGURED: crate::VideoLimits = crate::VideoLimits {
    max_width: 1920,
    max_height: 1080,
    max_fps: 60,
};
pub(crate) fn create(id: u32) -> Result<Box<dyn Backend>> {
    available()?;
    Ok(Box::new(Desktop {
        id,
        monitor_handle: monitor_handle(id)?,
        input: Enigo::new(&enigo::Settings::default()).map_err(error)?,
        held: Held::default(),
        secure: None,
        encoder: None,
        video_limits: UNCONFIGURED,
    }))
}
struct Desktop {
    id: u32,
    monitor_handle: HMONITOR,
    input: Enigo,
    held: Held,
    secure: Option<crate::helper::worker::DesktopBinding>,
    encoder: Option<(DesktopContext, crate::video::windows::Encoder)>,
    video_limits: crate::VideoLimits,
}
impl Desktop {
    fn prepare(&mut self) -> Result<()> {
        let Some(secure) = &mut self.secure else {
            return available().inspect_err(|_| {
                release_input(&mut self.input, &mut self.held);
                // A secure desktop transition can invalidate DXGI duplication
                // without changing monitor geometry. Rebuild on return to the
                // ordinary desktop rather than retaining old encoder resources.
                self.encoder = None;
            });
        };
        secure.enter(|| {
            release_input(&mut self.input, &mut self.held);
            // DXGI/MFT/COM resources belong to the old desktop thread. Destroy
            // them before binding, including a name change without an event.
            self.encoder = None;
        })
    }
    fn monitor(&self) -> Result<xcap::Monitor> {
        xcap::Monitor::all()
            .map_err(error)?
            .into_iter()
            .find(|m| m.id().ok() == Some(self.id))
            .ok_or_else(|| {
                display_lost(
                    "Selected display is no longer available; stop sharing and select a display",
                )
            })
    }
}
impl Backend for Desktop {
    fn configure_video(&mut self, limits: crate::VideoLimits, _nominal_fps: u32) -> Result<()> {
        limits.validate()?;
        if self.video_limits != limits {
            self.encoder = None;
            self.video_limits = limits;
        }
        Ok(())
    }
    fn display(&mut self) -> Result<Display> {
        let _dpi = DpiScope::enter();
        self.prepare()?;
        current_display(self.id, self.monitor_handle)
    }
    fn context(&mut self) -> Result<DesktopContext> {
        let display = self.display()?;
        Ok(DesktopContext {
            display,
            generation: self.secure.as_ref().map_or(0, |s| s.generation()),
            desktop: self
                .secure
                .as_ref()
                .map_or(DesktopKind::Ordinary, |s| s.kind()),
        })
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<CursorPosition>> {
        let _dpi = DpiScope::enter();
        if &self.context()? != expected {
            return Err(error("desktop changed before cursor capture"));
        }
        let mut cursor = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            GetCursorInfo(&mut cursor).map_err(error)?;
        }
        if &self.context()? != expected
            || self
                .secure
                .as_ref()
                .is_some_and(|desktop| desktop.changed())
        {
            return Err(error("desktop changed during cursor capture"));
        }
        let showing =
            cursor.flags.0 & CURSOR_SHOWING.0 != 0 && cursor.flags.0 & CURSOR_SUPPRESSED.0 == 0;
        let point = cursor.ptScreenPos;
        Ok(Some(crate::input::cursor::confine(
            &expected.display,
            (point.x, point.y),
            showing,
        )))
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        let _dpi = DpiScope::enter();
        self.prepare()?;
        let image = self.monitor()?.capture_image().map_err(error)?;
        self.prepare()?;
        let image = image::DynamicImage::ImageRgba8(image);
        let rgb = image
            .resize(1600, 1000, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 65)
            .encode_image(&rgb)
            .map_err(error)?;
        Ok(jpeg)
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: crate::VideoCodec,
        keyframe: bool,
        rate: crate::VideoRate,
    ) -> Result<Option<crate::EncodedFrame>> {
        if codec == crate::VideoCodec::Jpeg {
            return Ok(Some(crate::EncodedFrame {
                format: crate::VideoFormat {
                    codec,
                    keyframe: true,
                    width: expected.display.width,
                    height: expected.display.height,
                    pts_us: 0,
                    duplicate: false,
                    encode_us: 0,
                    gop: 0,
                },
                bytes: self.capture_consistent(expected)?,
            }));
        }
        let _dpi = DpiScope::enter();
        if &self.context()? != expected {
            return Err(error("desktop changed before video capture"));
        }
        if self
            .encoder
            .as_ref()
            .is_none_or(|(context, encoder)| context != expected || encoder.codec() != codec)
        {
            self.encoder = None;
            self.encoder = Some((
                expected.clone(),
                crate::video::windows::Encoder::new_with_size(
                    &expected.display,
                    codec,
                    self.video_limits.max_fps,
                    rate.bitrate,
                    self.video_limits.max_width,
                    self.video_limits.max_height,
                )?,
            ));
        }
        let encoder = &mut self
            .encoder
            .as_mut()
            .ok_or_else(|| error("video encoder unavailable"))?
            .1;
        // The worker paces: drain what is finished, then submit one capture.
        let image = encoder
            .set_rate(rate)
            .and_then(|()| encoder.drain())
            .and_then(|image| encoder.submit(keyframe).map(|_| image));
        if &self.context()? != expected {
            self.encoder = None;
            return Err(error("desktop changed during video capture"));
        }
        image
            .map(|image| image.map(Into::into))
            .inspect_err(|_| self.encoder = None)
    }
    /// Never blocks: the wait bounds only the helper's long-poll. A drain
    /// checks no desktop: it only collects output of submits `capture_video`
    /// checked, and the next submit or the worker's context check catches a
    /// switch (on the helper, `serve` checks before and after each video
    /// request).
    fn drain_video(
        &mut self,
        expected: &DesktopContext,
        _wait: std::time::Duration,
    ) -> Result<Option<crate::EncodedFrame>> {
        let Some((context, encoder)) = &mut self.encoder else {
            return Ok(None);
        };
        if context != expected {
            return Ok(None);
        }
        encoder
            .drain()
            .map(|image| image.map(Into::into))
            .inspect_err(|_| self.encoder = None)
    }
    fn pending_video(&self) -> usize {
        self.encoder
            .as_ref()
            .map_or(0, |(_, encoder)| encoder.pending_len())
    }
    /// The helper's worker answers the same way, in the interactive session
    /// where it encodes.
    fn describe(&mut self, probe_h265: bool) -> Result<crate::Description> {
        let display = self.display()?;
        Ok(crate::Description {
            caps: crate::BackendCaps {
                h265: probe_h265 && crate::video::windows::h265_available(self.id),
                lock: true,
                os: Some(crate::HostOs::Windows),
                ..crate::BackendCaps::default()
            },
            display,
        })
    }
    fn input(&mut self, event: &Input) -> Result<()> {
        let context = self.context()?;
        self.input_in_context(&context, event)
    }
    fn input_in_context(&mut self, expected: &DesktopContext, event: &Input) -> Result<()> {
        let _dpi = DpiScope::enter();
        if &self.context()? != expected || self.secure.as_ref().is_some_and(|s| s.changed()) {
            self.release();
            return Err(error("desktop changed before input"));
        }
        event.validate(&expected.display)?;
        // Do not rebind to another desktop below this point. A desktop switch
        // invalidates this operation instead of replaying it on the new desktop.
        match event {
            Input::Move { x, y } => {
                let d = &expected.display;
                // Enigo 0.6 absolute movement normalizes to the primary screen.
                // The Windows API accepts physical virtual-desktop coordinates,
                // including a selected display left/above the primary display.
                unsafe { SetCursorPos(d.x + *x as i32, d.y + *y as i32) }
                    .map_err(native_input_error)?;
            }
            Input::Button { button, down, .. } => {
                self.held.button(*button, *down, |down| {
                    self.input
                        .button(button_code(*button), direction(down))
                        .map_err(native_input_error)
                })?;
            }
            Input::Wheel {
                horizontal,
                vertical,
            } => {
                if *horizontal != 0 {
                    self.input
                        .scroll(*horizontal, Axis::Horizontal)
                        .map_err(native_input_error)?;
                }
                if *vertical != 0 {
                    self.input
                        .scroll(*vertical, Axis::Vertical)
                        .map_err(native_input_error)?;
                }
            }
            Input::Key { key, down } => {
                self.held.key(*key, *down, |down| {
                    self.input
                        .key(key_code(*key), direction(down))
                        .map_err(native_input_error)
                })?;
            }
            Input::Text { text } => self.input.text(text).map_err(native_input_error)?,
            // LockWorkStation arrives with the v2 worker.
            // The interactive session's own lock; the desktop switch that
            // follows invalidates the generation through the context check.
            Input::Lock {} => unsafe { LockWorkStation() }
                .map_err(|e| error(format!("Windows could not lock the workstation: {e}")))?,
        }
        Ok(())
    }
    fn release(&mut self) {
        release_input(&mut self.input, &mut self.held);
    }
    /// This process runs in the interactive session (the helper's worker does
    /// too), so these are the signed-in user's settings.
    fn double_click_guard(&mut self) -> Option<crate::DoubleClickGuard> {
        let _dpi = DpiScope::enter();
        let (time_ms, width, height) = unsafe {
            (
                GetDoubleClickTime(),
                GetSystemMetrics(SM_CXDOUBLECLK),
                GetSystemMetrics(SM_CYDOUBLECLK),
            )
        };
        let half = |size: i32| u32::try_from(size).unwrap_or(0) / 2;
        Some(crate::DoubleClickGuard {
            time_ms,
            half_box: (half(width), half(height)),
        })
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
    crate::helper::record_input_failure(&detail);
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

fn release_input(input: &mut Enigo, held: &mut Held) {
    held.release(|pressed| {
        match pressed {
            Pressed::Button(button) => input.button(button_code(button), Direction::Release),
            Pressed::Key(key) => input.key(key_code(key), Direction::Release),
        }
        .map_err(error)
    });
}

#[cfg(feature = "windows-helper")]
pub(crate) fn create_privileged(
    id: u32,
    session: crate::helper::SessionIdentity,
) -> Result<Box<dyn Backend>> {
    let mut binding = crate::helper::worker::DesktopBinding::new(session)?;
    binding.enter(|| {})?;
    Ok(Box::new(Desktop {
        id,
        monitor_handle: monitor_handle(id)?,
        input: Enigo::new(&enigo::Settings::default()).map_err(error)?,
        held: Held::default(),
        secure: Some(binding),
        encoder: None,
        video_limits: UNCONFIGURED,
    }))
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.release();
        // DesktopBinding restores the thread's original desktop when dropped.
        self.encoder = None;
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
