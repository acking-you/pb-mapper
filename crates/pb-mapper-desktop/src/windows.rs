//! Uses published Apache-2.0 xcap and MIT enigo. No RustDesk source is copied.
use crate::{Backend, Button, CursorPosition, DesktopContext, Display, Input, Key, Result, error};
use enigo::{Axis, Direction, Enigo, Keyboard, Mouse};
use std::collections::HashSet;
use windows::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, POINT},
    Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFOEXW},
    Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY, TokenIntegrityLevel, TokenSessionId, TokenUIAccess,
    },
    System::{
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
        WindowsAndMessaging::{
            CURSOR_SHOWING, CURSOR_SUPPRESSED, CURSORINFO, GetCursorInfo, SetCursorPos,
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
    error(
        "secure_desktop_requires_helper: This sharing session uses ordinary desktop access. UAC and the lock screen require the authorized Windows service. Prepare full desktop access on the host and restart sharing; otherwise the picture resumes when the ordinary desktop returns.",
    )
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
        .ok_or_else(|| error("selected display is no longer available"))
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
        return Err(error("display is unavailable"));
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
pub(super) fn displays() -> Result<Vec<Display>> {
    let _dpi = DpiScope::enter();
    available()?;
    xcap::Monitor::all()
        .map_err(error)?
        .iter()
        .map(describe)
        .collect()
}
pub(super) fn create(id: u32) -> Result<Box<dyn Backend>> {
    available()?;
    Ok(Box::new(Desktop {
        id,
        monitor_handle: monitor_handle(id)?,
        input: Enigo::new(&enigo::Settings::default()).map_err(error)?,
        buttons: HashSet::new(),
        keys: HashSet::new(),
        secure: None,
        encoder: None,
        video_preferences: crate::VideoPreferences::default(),
    }))
}
struct Desktop {
    id: u32,
    monitor_handle: HMONITOR,
    input: Enigo,
    buttons: HashSet<Button>,
    keys: HashSet<Key>,
    secure: Option<crate::helper::worker::DesktopBinding>,
    encoder: Option<(DesktopContext, crate::video::windows::Encoder)>,
    video_preferences: crate::VideoPreferences,
}
impl Desktop {
    fn finish_video(
        &mut self,
        expected: &DesktopContext,
        image: Result<Option<crate::video::windows::Encoded>>,
    ) -> Result<Option<crate::EncodedFrame>> {
        if &self.context()? != expected {
            self.encoder = None;
            return Err(error("desktop changed during video capture"));
        }
        match image {
            Ok(image) => Ok(image.map(Into::into)),
            Err(e) => {
                self.encoder = None;
                Err(e)
            }
        }
    }
    fn prepare(&mut self) -> Result<()> {
        let Some(secure) = &mut self.secure else {
            return available().inspect_err(|_| {
                release_input(&mut self.input, &mut self.buttons, &mut self.keys);
                // A secure desktop transition can invalidate DXGI duplication
                // without changing monitor geometry. Rebuild on return to the
                // ordinary desktop rather than retaining old encoder resources.
                self.encoder = None;
            });
        };
        secure.enter(|| {
            release_input(&mut self.input, &mut self.buttons, &mut self.keys);
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
                error("Selected display is no longer available; stop sharing and select a display")
            })
    }
}
impl Backend for Desktop {
    fn configure_video(&mut self, preferences: crate::VideoPreferences) -> Result<()> {
        preferences.validate()?;
        if self.video_preferences != preferences {
            self.encoder = None;
            self.video_preferences = preferences;
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
                .map_or_else(|| "ordinary".into(), |s| s.name().into()),
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
        Ok(Some(relative_cursor(
            &expected.display,
            cursor.ptScreenPos,
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
        bitrate: u32,
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
            .is_none_or(|(context, _)| context != expected)
        {
            self.encoder = None;
            self.encoder = Some((
                expected.clone(),
                crate::video::windows::Encoder::new_with_size(
                    &expected.display,
                    self.video_preferences.max_fps,
                    bitrate.min(self.video_preferences.bitrate),
                    self.video_preferences.max_width,
                    self.video_preferences.max_height,
                )?,
            ));
        }
        let encoder = &mut self
            .encoder
            .as_mut()
            .ok_or_else(|| error("video encoder unavailable"))?
            .1;
        encoder.set_bitrate(bitrate.min(self.video_preferences.bitrate))?;
        let image = encoder.next_frame(keyframe);
        self.finish_video(expected, image)
    }
    fn drain_video(&mut self, expected: &DesktopContext) -> Result<Option<crate::EncodedFrame>> {
        let _dpi = DpiScope::enter();
        if &self.context()? != expected {
            return Err(error("desktop changed before video drain"));
        }
        let Some((context, encoder)) = &mut self.encoder else {
            return Ok(None);
        };
        if context != expected {
            return Ok(None);
        }
        let image = encoder.drain();
        self.finish_video(expected, image)
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
            Input::Button { button, down } => {
                apply_held(&mut self.buttons, *button, *down, |direction| {
                    self.input
                        .button(button_code(*button), direction)
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
                apply_held(&mut self.keys, *key, *down, |direction| {
                    self.input
                        .key(key_code(*key), direction)
                        .map_err(native_input_error)
                })?;
            }
            Input::Text { text } => self.input.text(text).map_err(native_input_error)?,
        }
        Ok(())
    }
    fn release(&mut self) {
        release_input(&mut self.input, &mut self.buttons, &mut self.keys);
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

fn release_input(input: &mut Enigo, buttons: &mut HashSet<Button>, keys: &mut HashSet<Key>) {
    buttons.retain(|button| {
        input
            .button(button_code(*button), Direction::Release)
            .is_err()
    });
    keys.retain(|key| input.key(key_code(*key), Direction::Release).is_err());
}

fn apply_held<T: Copy + Eq + std::hash::Hash>(
    held: &mut HashSet<T>,
    value: T,
    down: bool,
    inject: impl FnOnce(Direction) -> Result<()>,
) -> Result<()> {
    // A failed native call can have partially applied input. Preserve the
    // release obligation until a subsequent key/button-up succeeds.
    if down {
        held.insert(value);
    }
    inject(if down {
        Direction::Press
    } else {
        Direction::Release
    })?;
    if !down {
        held.remove(&value);
    }
    Ok(())
}

#[test]
fn failed_native_input_retains_the_release_obligation() {
    let mut held = HashSet::new();
    assert!(apply_held(&mut held, 1, true, |_| Err(error("partial press"))).is_err());
    assert!(held.contains(&1));
    assert!(apply_held(&mut held, 1, false, |_| Err(error("blocked release"))).is_err());
    assert!(held.contains(&1));
    assert!(apply_held(&mut held, 1, false, |_| Ok(())).is_ok());
    assert!(held.is_empty());
}

fn relative_cursor(display: &Display, point: POINT, showing: bool) -> CursorPosition {
    let x = i64::from(point.x) - i64::from(display.x);
    let y = i64::from(point.y) - i64::from(display.y);
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
        buttons: HashSet::new(),
        keys: HashSet::new(),
        secure: Some(binding),
        encoder: None,
        video_preferences: crate::VideoPreferences::default(),
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
fn key_code(key: Key) -> enigo::Key {
    match key {
        Key::Shift => enigo::Key::Shift,
        Key::Control => enigo::Key::Control,
        Key::Alt => enigo::Key::Alt,
        Key::Meta => enigo::Key::Meta,
        Key::Enter => enigo::Key::Return,
        Key::Escape => enigo::Key::Escape,
        Key::Tab => enigo::Key::Tab,
        Key::Backspace => enigo::Key::Backspace,
        Key::Delete => enigo::Key::Delete,
        Key::Left => enigo::Key::LeftArrow,
        Key::Right => enigo::Key::RightArrow,
        Key::Up => enigo::Key::UpArrow,
        Key::Down => enigo::Key::DownArrow,
        Key::Home => enigo::Key::Home,
        Key::End => enigo::Key::End,
        Key::PageUp => enigo::Key::PageUp,
        Key::PageDown => enigo::Key::PageDown,
        Key::A => enigo::Key::Unicode('a'),
        Key::C => enigo::Key::Unicode('c'),
        Key::V => enigo::Key::Unicode('v'),
        Key::X => enigo::Key::Unicode('x'),
        Key::Z => enigo::Key::Unicode('z'),
    }
}

#[cfg(test)]
mod cursor_tests {
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
        let point = POINT { x: -100, y: 300 };
        assert_eq!(
            relative_cursor(&display, point, true),
            CursorPosition {
                x: 1820,
                y: 500,
                visible: true
            }
        );
        // Both an off-display cursor and an OS-hidden cursor withhold position.
        assert_eq!(
            relative_cursor(&display, POINT { x: 0, y: 300 }, true),
            CursorPosition::default()
        );
        assert_eq!(
            relative_cursor(&display, point, false),
            CursorPosition::default()
        );
    }
}
