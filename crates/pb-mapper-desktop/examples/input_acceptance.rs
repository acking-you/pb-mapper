//! Explicit Windows ordinary-desktop input acceptance. Uses its own window,
//! local loopback relay and expiring credential by default. An explicit
//! --relay-config PATH uses a deployed relay with a new expiring test scope;
//! --tcp overrides its carrier and --consumer-server HOST:PORT selects an
//! already-configured consumer route (for example an existing SSH forward).
//! Through a v2 viewer that presents what it receives, it checks Unicode text,
//! a click, a drag-start press on that spot that Windows must not merge into a
//! double click, and the release of that press when the viewer disconnects.
//! Run: cargo run -p pb-mapper-desktop --example input_acceptance -- --run-owned-window
//! Do not interact with the desktop during the short run. Foreground/geometry
//! changes permanently abort new input. Cleanup still releases held input.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--run-owned-window") {
        println!("Use --run-owned-window for the explicit local SDK/native input acceptance.");
        return Ok(());
    }
    windows_test::run()
}
#[cfg(not(windows))]
fn main() {
    println!("This explicit native-input acceptance requires Windows.");
}

#[cfg(windows)]
mod windows_test {
    use pb_mapper_client::sdk::{Client, ClientConfig, ConnectOptions, PathPolicy, RelayTransport};
    use pb_mapper_desktop::{
        Backend, Button, CursorPosition, DesktopContext, DesktopView, Display, DoubleClickGuard,
        EncodedFrame, Host, HostOptions, Input, Pull, QualityPreset, Result, VideoCodec,
        VideoLimits, VideoRate, Viewer, ViewerConfig,
    };
    use pb_mapper_testkit::Relay;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
            mpsc,
        },
        thread::JoinHandle,
        time::{Duration, Instant},
    };
    use windows::{
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Gdi::*,
            UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
        },
        core::w,
    };
    const TEXT: &str = "远控验证 ✓ 🙂";
    /// The host holds a drag-start press until the double-click time after
    /// the last click plus 20 ms, but never 600 ms past its arrival
    /// (`DRAG_GUARD_*` in the worker): it always waits out up to this much.
    const DRAG_HOLD_COVERS_MS: u32 = 580;
    #[derive(serde::Deserialize)]
    struct Configuration {
        server_address: String,
        msg_header_key: String,
        #[serde(default)]
        relay_transport: RelayTransport,
    }
    fn failure(message: impl std::fmt::Display) -> std::io::Error {
        std::io::Error::other(message.to_string())
    }
    fn hwnd(raw: isize) -> HWND {
        HWND(raw as *mut std::ffi::c_void)
    }
    struct DpiScope(DPI_AWARENESS_CONTEXT);
    impl DpiScope {
        fn enter() -> Self {
            Self(unsafe {
                SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            })
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
    struct State {
        window: AtomicIsize,
        edit: AtomicIsize,
        armed: AtomicBool,
        aborted: AtomicBool,
        rect: parking_lot::Mutex<RECT>,
        downs: AtomicU32,
        ups: AtomicU32,
        /// Presses Windows merged with the previous click into a double click.
        double_clicks: AtomicU32,
        /// The message time of the last press, and the gap to the one before.
        last_down_ms: AtomicU32,
        press_gap_ms: AtomicU32,
        held: AtomicBool,
        applied: AtomicU32,
        capture_ms: AtomicU32,
        input_error: parking_lot::Mutex<Option<String>>,
    }
    impl State {
        fn window(&self) -> HWND {
            hwnd(self.window.load(Ordering::Acquire))
        }
        fn edit(&self) -> HWND {
            hwnd(self.edit.load(Ordering::Acquire))
        }
        fn check(&self) -> Result<()> {
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
        fn client_point(&self, x: i32, y: i32) -> Result<POINT> {
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
        fn guard_input(&self, display: &Display, input: &Input) -> Result<()> {
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
                Input::Text { text } if text == TEXT => {
                    let mut info = GUITHREADINFO {
                        cbSize: size_of::<GUITHREADINFO>() as u32,
                        ..Default::default()
                    };
                    unsafe {
                        GetGUIThreadInfo(GetWindowThreadProcessId(self.window(), None), &mut info)
                            .map_err(failure)?;
                        if info.hwndFocus != self.edit() {
                            return Err(failure("Unicode target is not the owned EDIT control"));
                        }
                        if [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
                            .iter()
                            .any(|key| GetAsyncKeyState(i32::from(key.0)) < 0)
                        {
                            return Err(failure("physical modifier held; refusing text input"));
                        }
                    }
                }
                _ => {
                    return Err(failure(
                        "input is outside this acceptance's narrow allowlist",
                    ));
                }
            }
            Ok(())
        }
        fn text(&self) -> String {
            let mut text = [0u16; 128];
            let count = unsafe { GetWindowTextW(self.edit(), &mut text) };
            String::from_utf16_lossy(&text[..count.max(0) as usize])
        }
    }
    /// The product's backend behind the window guard. Every `Backend` method
    /// is forwarded, because a default left in place would quietly change the
    /// session: no encoder drain, no cursor, no drag-start hold.
    struct Guarded {
        inner: Box<dyn Backend>,
        state: Arc<State>,
    }
    impl Guarded {
        /// One capture-side call between two guard checks, timed for the
        /// diagnostics.
        fn timed<T>(&mut self, call: impl FnOnce(&mut dyn Backend) -> Result<T>) -> Result<T> {
            self.state.check()?;
            let started = Instant::now();
            let value = call(self.inner.as_mut())?;
            self.state
                .capture_ms
                .store(started.elapsed().as_millis() as u32, Ordering::Release);
            self.state.check()?;
            Ok(value)
        }
    }
    impl Backend for Guarded {
        fn configure_video(&mut self, limits: VideoLimits, nominal_fps: u32) -> Result<()> {
            self.inner.configure_video(limits, nominal_fps)
        }
        fn display(&mut self) -> Result<Display> {
            self.state.check()?;
            self.inner.display()
        }
        fn context(&mut self) -> Result<DesktopContext> {
            self.state.check()?;
            self.inner.context()
        }
        fn capture(&mut self) -> Result<Vec<u8>> {
            self.timed(|inner| inner.capture())
        }
        fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<CursorPosition>> {
            self.inner.cursor(expected)
        }
        fn capture_video(
            &mut self,
            expected: &DesktopContext,
            codec: VideoCodec,
            keyframe: bool,
            rate: VideoRate,
        ) -> Result<Option<EncodedFrame>> {
            self.timed(|inner| inner.capture_video(expected, codec, keyframe, rate))
        }
        fn drain_video(
            &mut self,
            expected: &DesktopContext,
            wait: Duration,
        ) -> Result<Option<EncodedFrame>> {
            self.timed(|inner| inner.drain_video(expected, wait))
        }
        fn pending_video(&self) -> usize {
            self.inner.pending_video()
        }
        fn input(&mut self, input: &Input) -> Result<()> {
            let context = self.context()?;
            self.input_in_context(&context, input)
        }
        fn input_in_context(&mut self, expected: &DesktopContext, input: &Input) -> Result<()> {
            self.state
                .guard_input(&expected.display, input)
                .and_then(|()| self.inner.input_in_context(expected, input))
                .inspect_err(|e| *self.state.input_error.lock() = Some(e.to_string()))?;
            self.state.applied.fetch_add(1, Ordering::Release);
            Ok(())
        }
        fn presented(&mut self, expected: &DesktopContext) -> Result<()> {
            self.inner.presented(expected)
        }
        fn release(&mut self) {
            self.inner.release();
        }
        fn double_click_guard(&mut self) -> Option<DoubleClickGuard> {
            self.inner.double_click_guard()
        }
    }
    struct Fixture {
        state: Arc<State>,
        thread: Option<JoinHandle<()>>,
    }
    impl Fixture {
        fn open(display: &Display) -> Result<Self> {
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
                unsafe {
                    let _ = PostMessageW(Some(self.state.window()), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        w: WPARAM,
        l: LPARAM,
    ) -> LRESULT {
        unsafe {
            if message == WM_NCCREATE {
                let create = &*(l.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const State;
            if let Some(state) = raw.as_ref() {
                match message {
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
            let class = WNDCLASSW {
                style: CS_DBLCLKS,
                lpfnWndProc: Some(window_proc),
                lpszClassName: w!("PbMapperOwnedInputAcceptance"),
                hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
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
                    WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
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
            Ok(())
        }
    }
    async fn wait_for(state: &State, what: &str, predicate: impl Fn() -> bool) -> Result<()> {
        let start = Instant::now();
        while !predicate() {
            state.check()?;
            if let Some(error) = state.input_error.lock().as_ref() {
                return Err(failure(error));
            }
            if start.elapsed() > Duration::from_secs(3) {
                return Err(failure(format!(
                    "timed out waiting for {what}; applied={} owned_edit={:?}",
                    state.applied.load(Ordering::Acquire),
                    state.text()
                )));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        state.check()
    }
    /// As the app's input core sends: with the generation of the view it
    /// drew. The viewer refuses it unless that generation's newest frame was
    /// presented recently and control is granted, and stamps that frame and
    /// the sequence itself.
    fn send(viewer: &Viewer, state: &State, input: Input) -> Result<()> {
        state.check()?;
        viewer
            .input(viewer.view().generation, &[input])
            .map_err(failure)
    }
    /// The view's input gate: control, and a fresh presented frame.
    fn live(viewer: &Viewer) -> bool {
        const LIVE: u8 = DesktopView::FLAG_CONTROLLING | DesktopView::FLAG_FRESH;
        viewer.view().flags & LIVE == LIVE
    }
    /// The native renderer's part: present each unit as it arrives, like a
    /// decoder that never falls behind, until the session's video ends. Only
    /// these presentations authorise input.
    fn present(viewer: &Viewer) -> Result<JoinHandle<()>> {
        let consumer = viewer.video_sink().attach().map_err(failure)?;
        Ok(std::thread::spawn(move || {
            let mut capacity = 1 << 20;
            loop {
                match consumer.next(Duration::from_millis(100), capacity) {
                    Pull::Unit(unit) => {
                        consumer.presented(unit.generation, unit.frame_id);
                    }
                    Pull::NeedCapacity(needed) => capacity = needed,
                    Pull::Timeout | Pull::Paused => {}
                    Pull::Closed => break,
                }
            }
        }))
    }
    /// Text, a click and a drag-start press on the owned window, then the
    /// release of that press when the viewer disconnects.
    async fn exercise(
        viewer: &Viewer,
        host: &Host,
        state: &State,
        display: &Display,
        topology: &str,
    ) -> Result<()> {
        wait_for(state, "first presented frame", || {
            viewer.view().presented_frame_id != 0
        })
        .await?;
        viewer.request_control(true)?;
        // A grant starts a new generation: input waits for its first
        // presented frame.
        wait_for(state, "control with a fresh presented frame", || {
            live(viewer)
        })
        .await?;
        send(viewer, state, Input::Text { text: TEXT.into() })?;
        if let Err(e) = wait_for(state, "Unicode text in owned EDIT", || state.text() == TEXT).await
        {
            eprintln!(
                "native input diagnostics: last_capture_ms={} viewer_state={:?} host_state={:?}",
                state.capture_ms.load(Ordering::Acquire),
                viewer.status(),
                host.status()
            );
            return Err(e);
        }
        let point = state.client_point(320, 220)?;
        if point.x < display.x
            || point.y < display.y
            || point.x >= display.x + display.width as i32
            || point.y >= display.y + display.height as i32
        {
            return Err(failure("owned target outside selected physical monitor"));
        }
        let applied = state.applied.load(Ordering::Acquire);
        let (x, y) = ((point.x - display.x) as u32, (point.y - display.y) as u32);
        send(viewer, state, Input::Move { x, y })?;
        wait_for(state, "native pointer move", || {
            state.applied.load(Ordering::Acquire) > applied
        })
        .await?;
        let left = |down, drag_start| Input::Button {
            button: Button::Left,
            down,
            drag_start,
        };
        send(viewer, state, left(true, false))?;
        wait_for(state, "native button down", || {
            state.downs.load(Ordering::Acquire) == 1 && state.held.load(Ordering::Acquire)
        })
        .await?;
        send(viewer, state, left(false, false))?;
        wait_for(state, "native button up", || {
            state.ups.load(Ordering::Acquire) == 1 && !state.held.load(Ordering::Acquire)
        })
        .await?;
        // A drag starting where the click just was: the host holds the press
        // past the double-click time, so Windows sees a new press rather than
        // a double click.
        send(viewer, state, left(true, true))?;
        wait_for(state, "held drag-start press before disconnect", || {
            state.downs.load(Ordering::Acquire) == 2 && state.held.load(Ordering::Acquire)
        })
        .await?;
        let double_click_ms = unsafe { GetDoubleClickTime() };
        let (double_clicks, press_gap_ms) = (
            state.double_clicks.load(Ordering::Acquire),
            state.press_gap_ms.load(Ordering::Acquire),
        );
        if double_clicks > 0 && double_click_ms <= DRAG_HOLD_COVERS_MS {
            return Err(failure(format!(
                "the drag-start press merged into a double click: press_gap_ms={press_gap_ms} double_click_ms={double_click_ms}"
            )));
        }
        let at = Instant::now();
        viewer.stop().await?;
        wait_for(state, "disconnect release", || {
            state.ups.load(Ordering::Acquire) == 2 && !state.held.load(Ordering::Acquire)
        })
        .await?;
        if unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } < 0 {
            return Err(failure("native left button remains held after disconnect"));
        }
        println!(
            "native_input_acceptance path={topology} unicode_verified=true button_down={} button_up={} drag_press_gap_ms={press_gap_ms} double_click_ms={double_click_ms} double_clicks={double_clicks} disconnect_release_ms={:.3} physical_display={}x{}",
            state.downs.load(Ordering::Acquire),
            state.ups.load(Ordering::Acquire),
            at.elapsed().as_secs_f64() * 1000.0,
            display.width,
            display.height
        );
        Ok(())
    }
    pub fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let display = pb_mapper_desktop::displays()?
            .into_iter()
            .next()
            .ok_or_else(|| failure("no ordinary display"))?;
        if unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } < 0 {
            return Err(failure("physical left button already held; refusing test").into());
        }
        let fixture = Fixture::open(&display)?;
        let state = fixture.state.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let args: Vec<_> = std::env::args().collect();
            let value = |name| {
                args.windows(2)
                    .find(|pair| pair[0] == name)
                    .map(|pair| pair[1].as_str())
            };
            for option in ["--relay-config", "--consumer-server"] {
                if args.iter().any(|arg| arg == option) && value(option).is_none() {
                    return Err(failure(format!("{option} requires a value")));
                }
            }
            let local_relay = if value("--relay-config").is_none() {
                Some(Relay::start("desktop-owned-native-input").await)
            } else {
                None
            };
            let (server, transport, admin_client) = if let Some(relay) = &local_relay {
                let server = relay.addr().to_string();
                let client = Client::from_credential(
                    server.clone(),
                    pb_mapper_testkit::admin_credential(),
                    true,
                    None,
                );
                (server, RelayTransport::Tcp, client)
            } else {
                let config = serde_json::from_slice::<Configuration>(&std::fs::read(
                    value("--relay-config").ok_or_else(|| failure("relay config required"))?,
                )?)
                .map_err(failure)?;
                let transport = if args.iter().any(|arg| arg == "--tcp") {
                    RelayTransport::Tcp
                } else {
                    config.relay_transport
                };
                let client = Client::new(ClientConfig {
                    server: config.server_address.clone(),
                    credential: config.msg_header_key,
                    keep_alive: true,
                    namespace: None,
                })
                .map_err(failure)?
                .with_relay_transport(transport)
                .map_err(failure)?;
                (config.server_address, transport, client)
            };
            let admin = admin_client.admin().map_err(failure)?;
            let issued = admin
                .issue_key(Duration::from_secs(120), Some("owned-native-input".into()))
                .await
                .map_err(failure)?;
            let result = async {
                let client = Client::new(ClientConfig {
                    server: server.clone(),
                    credential: issued.credential.clone(),
                    keep_alive: true,
                    namespace: None,
                })
                .map_err(failure)?
                .with_relay_transport(transport)
                .map_err(failure)?;
                let consumer = Client::new(ClientConfig {
                    server: value("--consumer-server").unwrap_or(&server).to_string(),
                    credential: issued.credential.clone(),
                    keep_alive: true,
                    namespace: None,
                })
                .map_err(failure)?
                .with_relay_transport(transport)
                .map_err(failure)?;
                let topology = if value("--consumer-server").is_some() {
                    "configured_separate_relay_entries"
                } else if local_relay.is_some() {
                    "local_tcp_relay"
                } else {
                    "deployed_relay_same_machine_endpoints"
                };
                let backend_state = state.clone();
                let id = display.id;
                let host = Host::start_with_backend(
                    &client,
                    HostOptions {
                        service: "desktop:owned-input-acceptance".into(),
                        display_id: id,
                        allow_control: true,
                        allow_direct: false,
                        lan_first: true,
                    },
                    Arc::new(move || {
                        Ok(Box::new(Guarded {
                            inner: pb_mapper_desktop::native_backend(id)?,
                            state: backend_state.clone(),
                        }))
                    }),
                )
                .await?;
                // Use the product's native GPU path with a small input-acceptance
                // workload. Debug JPEG resizing can hold the capture thread for a
                // second per frame, and input waits behind it.
                let config = ViewerConfig {
                    codecs: vec![VideoCodec::H264],
                    limits: VideoLimits {
                        max_width: 1280,
                        max_height: 720,
                        max_fps: 30,
                    },
                    quality: QualityPreset::Balanced,
                    progress: None,
                };
                let connected = Viewer::connect(
                    &consumer,
                    host.invitation(),
                    ConnectOptions {
                        path: PathPolicy::RelayOnly,
                        ..Default::default()
                    },
                    config,
                )
                .await;
                let viewer = match connected {
                    Ok(viewer) => viewer,
                    Err(error) => {
                        let _ = host.stop().await;
                        return Err(failure(error));
                    }
                };
                let presenter = present(&viewer);
                let result = match &presenter {
                    Ok(_) => exercise(&viewer, &host, &state, &display, topology).await,
                    Err(error) => Err(failure(error)),
                };
                let _ = viewer.stop().await;
                // The stop closed the video sink, which ends the presenting thread.
                if let Ok(presenter) = presenter {
                    let _ = presenter.join();
                }
                let stopped = host.stop().await;
                result?;
                stopped
            }
            .await;
            let revoked = admin.revoke_key(issued.key_id).await;
            result?;
            revoked.map(|_| ()).map_err(failure)
        })?;
        drop(fixture);
        Ok(())
    }
}
