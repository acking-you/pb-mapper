//! The owned test window and its guard: every input must land in it, and
//! a change of foreground or geometry stops input for good.
use pb_mapper_desktop::{Button, Display, Input, Result};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::*,
        UI::{HiDpi::*, Input::Ime::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::w,
};
pub(crate) const TEXT: &str = "远控验证 ✓ 🙂";
/// Text with a line break and a tab, each typed once: the multiline EDIT
/// reads it back as `a\r\nb\tc`.
pub(crate) const LINES: &str = "a\nb\tc";
/// The owned window's thread runs these on its own input context, which
/// IMM ties to the thread that made the window.
const IME_ENABLE: u32 = WM_APP + 1;
const IME_OPEN: u32 = WM_APP + 2;
const IME_COMPOSING: u32 = WM_APP + 3;
const IME_RESTORE: u32 = WM_APP + 4;
/// What the owned window's input context was before `IME_OPEN`.
#[derive(Default)]
struct ImeSaved {
    open: bool,
    conversion: u32,
    sentence: u32,
}
/// Client areas of the owned window whose pointer is a resize arrow, and a
/// monochrome cursor of this test's own ([`custom_planes`]).
pub(crate) const RESIZE_BOX: RECT = RECT {
    left: 560,
    top: 240,
    right: 640,
    bottom: 300,
};
pub(crate) const CUSTOM_BOX: RECT = RECT {
    left: 24,
    top: 240,
    right: 104,
    bottom: 300,
};
/// The custom cursor: 32×32 with its hotspot at (8, 8); rows 4..12 are
/// black on the left half (columns 0..16) and white on the right (16..32),
/// row 20 inverts the screen at columns 8..24; everything else is clear.
pub(crate) const CUSTOM_HOTSPOT: (u32, u32) = (8, 8);
fn custom_planes() -> ([u8; 128], [u8; 128]) {
    // AND: 1 is clear (or inverting); XOR: 1 is white (or inverting).
    let (mut and, mut xor) = ([0xFF_u8; 128], [0_u8; 128]);
    for row in 4..12 {
        and[row * 4..row * 4 + 4].fill(0);
        xor[row * 4 + 2..row * 4 + 4].fill(0xFF);
    }
    xor[20 * 4 + 1..20 * 4 + 3].fill(0xFF);
    (and, xor)
}
pub(crate) fn failure(message: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(message.to_string())
}
fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut std::ffi::c_void)
}
struct DpiScope(DPI_AWARENESS_CONTEXT);
impl DpiScope {
    fn enter() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}
impl Drop for DpiScope {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}
#[derive(Default)]
pub(crate) struct State {
    window: AtomicIsize,
    edit: AtomicIsize,
    /// This test's own cursor, shown over [`CUSTOM_BOX`].
    custom: AtomicIsize,
    armed: AtomicBool,
    aborted: AtomicBool,
    rect: parking_lot::Mutex<RECT>,
    pub(crate) downs: AtomicU32,
    pub(crate) ups: AtomicU32,
    /// Presses Windows merged with the previous click into a double click.
    pub(crate) double_clicks: AtomicU32,
    /// The message time of the last press, and the gap to the one before.
    last_down_ms: AtomicU32,
    pub(crate) press_gap_ms: AtomicU32,
    pub(crate) held: AtomicBool,
    pub(crate) applied: AtomicU32,
    pub(crate) capture_ms: AtomicU32,
    pub(crate) input_error: parking_lot::Mutex<Option<String>>,
    ime_saved: parking_lot::Mutex<Option<ImeSaved>>,
    composition: parking_lot::Mutex<String>,
}
impl State {
    fn window(&self) -> HWND {
        hwnd(self.window.load(Ordering::Acquire))
    }
    fn edit(&self) -> HWND {
        hwnd(self.edit.load(Ordering::Acquire))
    }
    pub(crate) fn check(&self) -> Result<()> {
        let _dpi = DpiScope::enter();
        let window = self.window();
        let mut rect = RECT::default();
        let expected = *self.rect.lock();
        let valid = unsafe {
            !self.aborted.load(Ordering::Acquire)
                && self.armed.load(Ordering::Acquire)
                && GetForegroundWindow() == window
                && IsWindowVisible(window).as_bool()
                && GetWindowRect(window, &mut rect).is_ok()
                && (rect.left, rect.top, rect.right, rect.bottom)
                    == (expected.left, expected.top, expected.right, expected.bottom)
        };
        if !valid {
            self.aborted.store(true, Ordering::Release);
            return Err(failure(format!(
                "owned window guard stopped input: foreground_owned={} expected_rect={expected:?} actual_rect={rect:?} applied={}",
                unsafe { GetForegroundWindow() } == window,
                self.applied.load(Ordering::Acquire)
            )));
        }
        Ok(())
    }
    pub(crate) fn client_point(&self, x: i32, y: i32) -> Result<POINT> {
        let _dpi = DpiScope::enter();
        self.check()?;
        let mut point = POINT { x, y };
        if !unsafe { ClientToScreen(self.window(), &mut point) }.as_bool() {
            return Err(failure("cannot locate owned test window"));
        }
        Ok(point)
    }
    fn contains(&self, point: POINT) -> Result<()> {
        let _dpi = DpiScope::enter();
        let mut client = RECT::default();
        let mut local = point;
        unsafe {
            GetClientRect(self.window(), &mut client).map_err(failure)?;
            if !ScreenToClient(self.window(), &mut local).as_bool()
                || local.x < client.left
                || local.y < client.top
                || local.x >= client.right
                || local.y >= client.bottom
                || GetAncestor(WindowFromPoint(point), GA_ROOT) != self.window()
            {
                return Err(failure(
                    "input target is outside or obscures the owned test window",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn guard_input(&self, display: &Display, input: &Input) -> Result<()> {
        self.check()?;
        match input {
            Input::Move { x, y } => self.contains(POINT {
                x: display.x + *x as i32,
                y: display.y + *y as i32,
            })?,
            Input::Button {
                button: Button::Left,
                ..
            } => {
                let mut point = POINT::default();
                unsafe {
                    GetCursorPos(&mut point).map_err(failure)?;
                }
                self.contains(point)?;
            }
            Input::Text { text } if text == TEXT || text == LINES => {
                self.edit_focused()?;
                if modifier_held() {
                    return Err(failure("physical modifier held; refusing text input"));
                }
            }
            // Keys go to the window with the keyboard focus: the owned EDIT,
            // in the foreground (`check`).
            Input::PhysicalKey { .. } => self.edit_focused()?,
            _ => {
                return Err(failure(
                    "input is outside this acceptance's narrow allowlist",
                ));
            }
        }
        Ok(())
    }
    fn edit_focused(&self) -> Result<()> {
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            GetGUIThreadInfo(GetWindowThreadProcessId(self.window(), None), &mut info)
                .map_err(failure)?;
        }
        if info.hwndFocus != self.edit() {
            return Err(failure("the keyboard target is not the owned EDIT control"));
        }
        Ok(())
    }
    pub(crate) fn text(&self) -> String {
        let mut text = [0u16; 128];
        let count = unsafe { GetWindowTextW(self.edit(), &mut text) };
        String::from_utf16_lossy(&text[..count.max(0) as usize])
    }
    pub(crate) fn clear(&self) -> Result<()> {
        unsafe { SetWindowTextW(self.edit(), w!("")) }.map_err(failure)
    }
    /// What the scancode `scan` types on the owned window's keyboard layout,
    /// with Shift or without.
    pub(crate) fn typed_by_layout(&self, scan: u32, shift: bool) -> String {
        let mut state = [0u8; 256];
        if shift {
            state[usize::from(VK_SHIFT.0)] = 0x80;
        }
        let mut out = [0u16; 8];
        let count = unsafe {
            let layout = GetKeyboardLayout(GetWindowThreadProcessId(self.window(), None));
            let vk = MapVirtualKeyExW(scan, MAPVK_VSC_TO_VK_EX, Some(layout));
            // Flag 4: leave the keyboard state, a dead key's included, as
            // it is.
            ToUnicodeEx(vk, scan, &state, &mut out, 4, Some(layout))
        };
        String::from_utf16_lossy(&out[..count.max(0) as usize])
    }
    fn send(&self, message: u32, enable: bool) -> isize {
        unsafe {
            SendMessageW(
                self.window(),
                message,
                Some(WPARAM(usize::from(enable))),
                Some(LPARAM(0)),
            )
        }
        .0
    }
    /// The owned EDIT's input method on (its default context) or off.
    pub(crate) fn ime(&self, enabled: bool) -> Result<()> {
        if self.send(IME_ENABLE, enabled) == 0 {
            return Err(failure("the owned window's input method did not change"));
        }
        Ok(())
    }
    /// Open the owned EDIT's input method in its native (composing) mode,
    /// keeping what it was for [`State::ime_restore`]; false where it has
    /// none.
    pub(crate) fn ime_open(&self) -> bool {
        self.send(IME_OPEN, true) != 0
    }
    /// What the owned EDIT's input method is composing now.
    pub(crate) fn composing(&self) -> String {
        self.send(IME_COMPOSING, true);
        self.composition.lock().clone()
    }
    /// Cancel any composition and put the input method back as it was.
    pub(crate) fn ime_restore(&self) {
        self.send(IME_RESTORE, true);
    }
}

/// A modifier held on the keyboard, or by an injection still in effect.
pub(crate) fn modifier_held() -> bool {
    [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
        .iter()
        .any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } < 0)
}

/// The window thread's part of the IME controls.
unsafe fn ime_message(state: &State, message: u32, enable: bool) -> isize {
    unsafe {
        let edit = state.edit();
        match message {
            IME_ENABLE => {
                let flags = if enable { IACE_DEFAULT } else { 0 };
                isize::from(ImmAssociateContextEx(edit, HIMC::default(), flags).as_bool())
            }
            IME_OPEN | IME_COMPOSING | IME_RESTORE => {
                let context = ImmGetContext(edit);
                if context.is_invalid() {
                    return 0;
                }
                match message {
                    IME_OPEN => {
                        let (mut conversion, mut sentence) =
                            (IME_CONVERSION_MODE(0), IME_SENTENCE_MODE(0));
                        let _ = ImmGetConversionStatus(
                            context,
                            Some(&mut conversion),
                            Some(&mut sentence),
                        );
                        *state.ime_saved.lock() = Some(ImeSaved {
                            open: ImmGetOpenStatus(context).as_bool(),
                            conversion: conversion.0,
                            sentence: sentence.0,
                        });
                        let _ = ImmSetOpenStatus(context, true);
                        let _ = ImmSetConversionStatus(
                            context,
                            conversion | IME_CMODE_NATIVE,
                            sentence,
                        );
                    }
                    IME_COMPOSING => {
                        let mut text = [0u16; 64];
                        let bytes = ImmGetCompositionStringW(
                            context,
                            GCS_COMPSTR,
                            Some(text.as_mut_ptr().cast()),
                            size_of_val(&text) as u32,
                        );
                        let units = (bytes.max(0) as usize / 2).min(text.len());
                        *state.composition.lock() = String::from_utf16_lossy(&text[..units]);
                    }
                    _ => {
                        let _ = ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                        if let Some(saved) = state.ime_saved.lock().take() {
                            let _ = ImmSetConversionStatus(
                                context,
                                IME_CONVERSION_MODE(saved.conversion),
                                IME_SENTENCE_MODE(saved.sentence),
                            );
                            let _ = ImmSetOpenStatus(context, saved.open);
                        }
                    }
                }
                let _ = ImmReleaseContext(edit, context);
                1
            }
            _ => 0,
        }
    }
}
pub(crate) struct Fixture {
    pub(crate) state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}
impl Fixture {
    pub(crate) fn open(display: &Display) -> Result<Self> {
        let state = Arc::new(State::default());
        let thread_state = state.clone();
        let (ready, started) = mpsc::sync_channel(1);
        let (x, y) = (display.x + 80, display.y + 80);
        let thread = std::thread::spawn(move || {
            if let Err(e) = unsafe { window_loop(&thread_state, x, y, &ready) } {
                let _ = ready.send(Err(e.to_string()));
            }
        });
        let fixture = Self {
            state,
            thread: Some(thread),
        };
        started
            .recv_timeout(Duration::from_secs(5))
            .map_err(failure)?
            .map_err(failure)?;
        Ok(fixture)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.state.window.load(Ordering::Acquire) != 0 {
            // Whatever a failed run left composing or switched goes back.
            self.state.ime_restore();
            unsafe {
                let _ = PostMessageW(Some(self.state.window()), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
unsafe extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const State;
        if let Some(state) = raw.as_ref() {
            match message {
                IME_ENABLE..=IME_RESTORE => {
                    return LRESULT(ime_message(state, message, w.0 != 0));
                }
                WM_ACTIVATE
                    if w.0 & 0xffff == WA_INACTIVE as usize
                        && state.armed.load(Ordering::Acquire) =>
                {
                    state.aborted.store(true, Ordering::Release);
                }
                WM_SETFOCUS => {
                    if state.edit.load(Ordering::Acquire) != 0 {
                        let _ = SetFocus(Some(state.edit()));
                    }
                }
                // The pointer's shape over the boxes; the class cursor, an
                // arrow, elsewhere in the client area.
                WM_SETCURSOR if w.0 == window.0 as usize && (l.0 & 0xffff) as u32 == HTCLIENT => {
                    let mut point = POINT::default();
                    if GetCursorPos(&mut point).is_ok()
                        && ScreenToClient(window, &mut point).as_bool()
                    {
                        let inside = |r: RECT| {
                            point.x >= r.left
                                && point.x < r.right
                                && point.y >= r.top
                                && point.y < r.bottom
                        };
                        let cursor = if inside(RESIZE_BOX) {
                            LoadCursorW(None, IDC_SIZENWSE).ok()
                        } else if inside(CUSTOM_BOX) {
                            Some(HCURSOR(state.custom.load(Ordering::Acquire) as *mut _))
                        } else {
                            None
                        };
                        if let Some(cursor) = cursor {
                            SetCursor(Some(cursor));
                            return LRESULT(1);
                        }
                    }
                }
                WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                    // The class has CS_DBLCLKS: a press Windows merges
                    // with the click before it arrives as a double click.
                    if message == WM_LBUTTONDBLCLK {
                        state.double_clicks.fetch_add(1, Ordering::Release);
                    }
                    let at = GetMessageTime() as u32;
                    let last = state.last_down_ms.swap(at, Ordering::AcqRel);
                    if state.downs.fetch_add(1, Ordering::AcqRel) > 0 {
                        state
                            .press_gap_ms
                            .store(at.wrapping_sub(last), Ordering::Release);
                    }
                    state.held.store(true, Ordering::Release);
                    return LRESULT(0);
                }
                WM_LBUTTONUP => {
                    state.ups.fetch_add(1, Ordering::Release);
                    state.held.store(false, Ordering::Release);
                    return LRESULT(0);
                }
                WM_PAINT => {
                    let mut paint = PAINTSTRUCT::default();
                    let dc = BeginPaint(window, &mut paint);
                    let text: Vec<u16> =
                        "Owned test window only. Unicode field above; button test below."
                            .encode_utf16()
                            .collect();
                    let _ = TextOutW(dc, 24, 140, &text);
                    // The boxes whose pointer shape the viewer must see.
                    for (r, label) in [(CUSTOM_BOX, "custom"), (RESIZE_BOX, "resize")] {
                        let _ = FrameRect(dc, &r, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                        let label: Vec<u16> = label.encode_utf16().collect();
                        let _ = TextOutW(dc, r.left + 6, r.top + 6, &label);
                    }
                    let _ = EndPaint(window, &paint);
                    return LRESULT(0);
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    return LRESULT(0);
                }
                _ => {}
            }
        }
        DefWindowProcW(window, message, w, l)
    }
}
unsafe fn window_loop(
    state: &State,
    x: i32,
    y: i32,
    ready: &mpsc::SyncSender<std::result::Result<(), String>>,
) -> Result<()> {
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let (and, xor) = custom_planes();
        let custom = CreateCursor(
            None,
            CUSTOM_HOTSPOT.0 as i32,
            CUSTOM_HOTSPOT.1 as i32,
            32,
            32,
            and.as_ptr().cast(),
            xor.as_ptr().cast(),
        )
        .map_err(failure)?;
        state.custom.store(custom.0 as isize, Ordering::Release);
        let class = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(window_proc),
            lpszClassName: w!("PbMapperOwnedInputAcceptance"),
            hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(failure)?,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(failure(windows::core::Error::from_thread()));
        }
        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class.lpszClassName,
            w!("pb-mapper SDK input acceptance — owned test window"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            680,
            360,
            None,
            None,
            None,
            Some((state as *const State).cast()),
        )
        .map_err(failure)?;
        state.window.store(window.0 as isize, Ordering::Release);
        let setup = (|| {
            let edit = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w!("EDIT"),
                w!(""),
                // Multiline, so that Enter and Tab are typed into it.
                WS_CHILD
                    | WS_VISIBLE
                    | WINDOW_STYLE((ES_MULTILINE | ES_WANTRETURN | ES_AUTOVSCROLL) as u32),
                24,
                40,
                610,
                52,
                Some(window),
                None,
                None,
                None,
            )
            .map_err(failure)?;
            state.edit.store(edit.0 as isize, Ordering::Release);
            let _ = ShowWindow(window, SW_SHOW);
            if !SetForegroundWindow(window).as_bool() {
                return Err(failure(
                    "Windows did not foreground the owned window; refusing input",
                ));
            }
            // SetFocus returns the previous focus, which may legitimately
            // be null; verify the resulting focus instead of that return.
            let _ = SetFocus(Some(edit));
            if GetFocus() != edit {
                return Err(failure("owned EDIT control could not receive focus"));
            }
            let mut rect = RECT::default();
            GetWindowRect(window, &mut rect).map_err(failure)?;
            *state.rect.lock() = rect;
            state.armed.store(true, Ordering::Release);
            state.check()?;
            Ok::<_, std::io::Error>(())
        })();
        if let Err(e) = setup {
            let _ = DestroyWindow(window);
            return Err(e);
        }
        let _ = ready.send(Ok(()));
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        state.window.store(0, Ordering::Release);
        let _ = DestroyCursor(custom);
        Ok(())
    }
}
