use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "windows-helper")]
use std::sync::mpsc;
use windows::Win32::{Foundation::*, System::StationsAndDesktops::*, UI::WindowsAndMessaging::*};
#[cfg(feature = "windows-helper")]
use windows::Win32::{System::Threading::*, UI::Accessibility::*};
static EPOCH: AtomicU64 = AtomicU64::new(1);
// SessionIdentity::check is an RPC to the session manager (~120 µs), and one
// helper request checks several times. An answer stands this long while no
// desktop switch is seen and the console session is still ours: lock, unlock
// and UAC switch desktops, and a logoff and a new login take far longer. Not
// on the Winlogon desktop: UAC and the lock UI share it, so a lock there may
// raise no switch and the console state is the only signal.
const CONSOLE_REUSE: std::time::Duration = std::time::Duration::from_millis(4);
/// A host that asked for no video this long is paused or gone: the worker
/// gives its fine timers back.
#[cfg(feature = "windows-helper")]
const VIDEO_IDLE: std::time::Duration = std::time::Duration::from_secs(1);
// SendInput requires journal-playback access on the bound desktop handle.
// READ/WRITEOBJECTS alone allows SetCursorPos but rejects buttons and keys,
// even for a SYSTEM process with UIAccess. Do not broaden the worker token.
const INPUT_DESKTOP_ACCESS: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(
    DESKTOP_READOBJECTS.0
        | DESKTOP_WRITEOBJECTS.0
        | DESKTOP_SWITCHDESKTOP.0
        | DESKTOP_JOURNALPLAYBACK.0,
);
#[cfg(feature = "windows-helper")]
unsafe extern "system" fn desktop_event(
    _: HWINEVENTHOOK,
    _: u32,
    _: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    EPOCH.fetch_add(1, Ordering::AcqRel);
}

struct Events {
    id: u32,
    join: Option<std::thread::JoinHandle<()>>,
}
impl Events {
    #[cfg(feature = "windows-helper")]
    fn new() -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel(1);
        let (accept, accepted) = mpsc::sync_channel(1);
        let join = std::thread::spawn(move || Self::listen(tx, accepted));
        let id = rx.recv_timeout(TIMEOUT).map_err(error)??;
        accept.send(()).map_err(error)?;
        Ok(Self {
            id,
            join: Some(join),
        })
    }
    #[cfg(feature = "windows-helper")]
    fn listen(ready: mpsc::SyncSender<Result<u32>>, accepted: mpsc::Receiver<()>) {
        unsafe {
            let mut message = MSG::default();
            let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
            let hook = SetWinEventHook(
                EVENT_SYSTEM_DESKTOPSWITCH,
                EVENT_SYSTEM_DESKTOPSWITCH,
                None,
                Some(desktop_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            if hook.is_invalid() {
                let _ = ready.send(Err(error("desktop switch notifications unavailable")));
                return;
            }
            // Startup can time out while this thread is being scheduled. Only
            // enter an indefinite message pump after the owner accepts us;
            // otherwise unregister the hook and exit without an orphan thread.
            if ready.send(Ok(GetCurrentThreadId())).is_ok()
                && accepted.recv_timeout(TIMEOUT).is_ok()
            {
                while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                    let _ = DispatchMessageW(&message);
                }
            }
            let _ = UnhookWinEvent(hook);
        }
    }
}
impl Drop for Events {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Default)]
struct BindingState {
    epoch: u64,
    generation: u64,
    name: String,
    console: Option<os::ConsoleState>,
}
impl BindingState {
    fn enter(
        &mut self,
        name: String,
        epoch: u64,
        console: os::ConsoleState,
        before_switch: impl FnOnce(),
        bind: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        if self.name == name && self.epoch == epoch && self.console == Some(console) {
            return Ok(());
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| error("desktop generation exhausted"))?;
        // This also covers a name change whose notification has not arrived.
        // Keep the old state if binding fails so the next call retries it.
        before_switch();
        bind()?;
        self.name = name;
        self.epoch = epoch;
        self.console = Some(console);
        self.generation = generation;
        Ok(())
    }
}

/// The last console answer, with the epoch it was asked under and when. A
/// failure is never kept: the next check asks Windows again.
#[derive(Default)]
struct ConsoleMemo(std::cell::Cell<Option<(u64, std::time::Instant, os::ConsoleState)>>);
impl ConsoleMemo {
    /// `desktop` is the bound desktop's name: on Winlogon every check asks.
    fn get(
        &self,
        desktop: &str,
        epoch: u64,
        now: std::time::Instant,
        query: impl FnOnce() -> Result<os::ConsoleState>,
    ) -> Result<os::ConsoleState> {
        if desktop != "winlogon"
            && let Some((asked, at, console)) = self.0.get()
            && asked == epoch
            && now.saturating_duration_since(at) < CONSOLE_REUSE
        {
            return Ok(console);
        }
        self.fresh(epoch, now, query)
    }
    fn fresh(
        &self,
        epoch: u64,
        now: std::time::Instant,
        query: impl FnOnce() -> Result<os::ConsoleState>,
    ) -> Result<os::ConsoleState> {
        self.0.set(None);
        let console = query()?;
        self.0.set(Some((epoch, now, console)));
        Ok(console)
    }
}

/// Owned by exactly one capture thread. The event listener only changes an
/// atomic epoch; it never captures, injects, or opens interactive windows.
pub(crate) struct DesktopBinding {
    original: isize,
    bound: isize,
    session: os::SessionIdentity,
    checked: ConsoleMemo,
    state: BindingState,
    _events: Events,
}
impl DesktopBinding {
    #[cfg(feature = "windows-helper")]
    pub fn new(session: os::SessionIdentity) -> Result<Self> {
        if os::current_sid()? != "S-1-5-18" {
            return Err(error(
                "secure desktop backend requires the installed worker",
            ));
        }
        if os::session(std::process::id())? != session.session {
            return Err(error(
                "worker does not belong to its approved console session",
            ));
        }
        session.check()?;
        let original = unsafe { GetThreadDesktop(GetCurrentThreadId()) }.map_err(error)?;
        Ok(Self {
            original: original.0 as isize,
            bound: 0,
            session,
            checked: ConsoleMemo::default(),
            state: BindingState::default(),
            _events: Events::new()?,
        })
    }
    pub fn changed(&self) -> bool {
        let epoch = EPOCH.load(Ordering::Acquire);
        self.state.epoch != epoch
            || !self
                .console(epoch)
                .is_ok_and(|console| self.state.console == Some(console))
    }
    /// `epoch` is sampled before asking, so a switch during the RPC
    /// invalidates its answer.
    fn console(&self, epoch: u64) -> Result<os::ConsoleState> {
        let now = std::time::Instant::now();
        let check = || self.session.check();
        if os::active_session() != self.session.session {
            // Checking an inactive session fails before any RPC.
            return self.checked.fresh(epoch, now, check);
        }
        self.checked.get(&self.state.name, epoch, now, check)
    }
    pub fn generation(&self) -> u64 {
        self.state.generation
    }
    pub fn kind(&self) -> crate::DesktopKind {
        if self.state.name == "winlogon" {
            crate::DesktopKind::Secure
        } else {
            crate::DesktopKind::Ordinary
        }
    }
    pub fn enter(&mut self, before_switch: impl FnOnce()) -> Result<()> {
        // Sample before asking and opening: a later notification must not be
        // acknowledged as if it described the answer or handle we already have.
        let epoch = EPOCH.load(Ordering::Acquire);
        let console = match self.console(epoch) {
            Ok(console) => console,
            Err(failure) => {
                // A reused session ID must never keep old input or video state.
                before_switch();
                return Err(failure);
            }
        };
        let desktop = unsafe { OpenInputDesktop(Default::default(), false, INPUT_DESKTOP_ACCESS) }
            .map_err(error)?;
        let mut bound = false;
        let result = (|| {
            let mut text = [0u16; 256];
            unsafe {
                GetUserObjectInformationW(
                    HANDLE(desktop.0),
                    UOI_NAME,
                    Some(text.as_mut_ptr().cast()),
                    size_of_val(&text) as u32,
                    None,
                )
            }
            .map_err(error)?;
            let name = String::from_utf16_lossy(
                &text[..text.iter().position(|c| *c == 0).unwrap_or(text.len())],
            )
            .to_ascii_lowercase();
            if name != "default" && name != "winlogon" {
                return Err(error("unsupported input desktop"));
            }
            self.state.enter(name, epoch, console, before_switch, || {
                // Ask Windows itself before binding, and keep its answer: a
                // stale one must not fail every retry until it ages out.
                let fresh = self
                    .checked
                    .fresh(epoch, std::time::Instant::now(), || self.session.check())?;
                if fresh != console || EPOCH.load(Ordering::Acquire) != epoch {
                    return Err(error("input desktop changed before binding"));
                }
                // The caller has released input and desktop-affine capture/COM
                // resources before SetThreadDesktop can reject an old window
                // or hook with ERROR_BUSY.
                unsafe { SetThreadDesktop(desktop) }.map_err(error)?;
                let previous = std::mem::replace(&mut self.bound, desktop.0 as isize);
                bound = true;
                if previous != 0 {
                    unsafe {
                        let _ = CloseDesktop(HDESK(previous as *mut _));
                    }
                }
                Ok(())
            })?;
            if self.changed() {
                return Err(error("input desktop changed during binding"));
            }
            Ok(())
        })();
        if !bound {
            unsafe {
                let _ = CloseDesktop(desktop);
            }
        }
        result
    }
}
impl Drop for DesktopBinding {
    fn drop(&mut self) {
        unsafe {
            let _ = SetThreadDesktop(HDESK(self.original as *mut _));
            if self.bound != 0 {
                let _ = CloseDesktop(HDESK(self.bound as *mut _));
            }
        }
    }
}

#[cfg(feature = "windows-helper")]
pub fn run(name: &str, parent: u32) -> Result<()> {
    if !name.starts_with(&format!("{PIPE}.worker."))
        || name.len() > 160
        || os::current_sid()? != "S-1-5-18"
    {
        return Err(error("invalid worker launch"));
    }
    let config = configuration()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut pipe = ipc::connect(name)?;
        os::verify_server(&pipe, &config.helper_executable, Some(parent))?;
        let Request::Attach {
            display_id,
            session,
        } = tokio::time::timeout(TIMEOUT, ipc::read_json(&mut pipe))
            .await
            .map_err(error)??
        else {
            return Err(error("missing worker start"));
        };
        let (requests, incoming) =
            mpsc::sync_channel::<(Request, tokio::sync::oneshot::Sender<(Response, Vec<u8>)>)>(4);
        let cancel = tokio_util::sync::CancellationToken::new();
        let stopped = cancel.clone();
        let join = std::thread::spawn(move || {
            let mut backend = match crate::platform::windows::create_privileged(display_id, session)
            {
                Ok(backend) => backend,
                Err(e) => {
                    if let Ok((_, reply)) = incoming.recv_timeout(TIMEOUT) {
                        let _ = reply.send((Response::failed(e.to_string(), 0), vec![]));
                    }
                    return;
                }
            };
            let mut sequence = 0;
            let mut last_input = std::time::Instant::now();
            // Fine process timers while the host asks for video; a paused
            // host asks for none.
            let mut timer = None;
            let mut last_video = std::time::Instant::now();
            while !stopped.is_cancelled() {
                if last_input.elapsed() > std::time::Duration::from_secs(2) {
                    backend.release();
                }
                if last_video.elapsed() > VIDEO_IDLE {
                    timer = None;
                }
                let (request, reply) =
                    match incoming.recv_timeout(std::time::Duration::from_millis(15)) {
                        Ok(request) => request,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                if stopped.is_cancelled() {
                    break;
                }
                if matches!(request, Request::Video { .. }) {
                    last_video = std::time::Instant::now();
                    timer.get_or_insert_with(crate::timer::TimerResolution::hold);
                }
                let close = matches!(request, Request::Close {});
                let result = serve(&mut *backend, request, &mut sequence, &mut last_input);
                let pending = u32::try_from(backend.pending_video()).unwrap_or(u32::MAX);
                let response = match result {
                    Ok((mut response, image)) => {
                        response.pending = pending;
                        (response, image)
                    }
                    Err(e) => {
                        backend.release();
                        (Response::failed(e.to_string(), pending), vec![])
                    }
                };
                let _ = reply.send(response);
                if close {
                    break;
                }
            }
            backend.release();
        });
        let exchange = exchange(&mut pipe, display_id, |request| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let sent = requests.try_send((request, tx)).map_err(error);
            async move {
                sent?;
                rx.await.map_err(error)
            }
        })
        .await;
        cancel.cancel();
        drop(requests);
        let _ = join.join();
        exchange
    })
}

/// Hand the host's requests to the desktop-bound thread, one at a time, until
/// `Close`. Each answer is bounded by `TIMEOUT`, but the wait for the next
/// request is not: a host whose viewer is paused sends nothing for as long as
/// the pause lasts. The pipe closing ends that wait, and the service closes
/// it when its caller's pipe closes, or when the owner or its login ends.
#[cfg(feature = "windows-helper")]
async fn exchange<P, A>(
    pipe: &mut P,
    display_id: u32,
    mut serve: impl FnMut(Request) -> A,
) -> Result<()>
where
    P: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    A: std::future::Future<Output = Result<(Response, Vec<u8>)>>,
{
    let mut request = Request::Start { display_id };
    loop {
        let close = matches!(request, Request::Close {});
        let (response, image) = tokio::time::timeout(TIMEOUT, serve(request))
            .await
            .map_err(error)??;
        tokio::time::timeout(TIMEOUT, async {
            ipc::write_json(pipe, &response).await?;
            ipc::write_bytes(pipe, &image, ipc::MAX_FRAME).await
        })
        .await
        .map_err(error)??;
        if close {
            return Ok(());
        }
        request = ipc::read_json(pipe).await?;
    }
}

#[cfg(feature = "windows-helper")]
impl Response {
    fn failed(error: String, pending: u32) -> Self {
        Self {
            context: None,
            error: Some(error),
            video: None,
            cursor: None,
            pending,
            double_click: None,
            h265: None,
        }
    }
}

/// One request on the worker's desktop-bound thread. The host's thread C
/// paces and authorises; this checks the input sequence again, and the
/// backend the whole desktop the host expects, at the injection boundary.
#[cfg(feature = "windows-helper")]
fn serve(
    backend: &mut dyn Backend,
    request: Request,
    sequence: &mut u64,
    last_input: &mut std::time::Instant,
) -> Result<(Response, Vec<u8>)> {
    let mut response = Response {
        context: None,
        error: None,
        video: None,
        cursor: None,
        pending: 0,
        double_click: None,
        h265: None,
    };
    if matches!(request, Request::Release {} | Request::Close {}) {
        backend.release();
        return Ok((response, vec![]));
    }
    if let Request::Input {
        expected,
        sequence: next,
        input,
    } = request
    {
        if next <= *sequence {
            backend.release();
            return Err(error("repeated helper input"));
        }
        *sequence = next;
        // Binds this thread to the input desktop, and fails unless it is
        // still `expected`.
        backend.input_in_context(&expected, &input)?;
        *last_input = std::time::Instant::now();
        // The pointer as the input left it, so that the host's thread C knows
        // its own move when it comes back. The input stands without it.
        response.cursor = backend.cursor(&expected).ok().flatten();
        // The host keeps the desktop each answer reports: this is the one the
        // input reached.
        response.context = Some(expected);
        return Ok((response, vec![]));
    }
    let context = backend.context()?;
    // Context checks observe the pointer too, so it follows while capture is
    // idle. The backend checks this desktop identity; observing it never
    // renews last_input or acknowledges video.
    let observe_cursor = matches!(
        request,
        Request::Context {} | Request::Capture {} | Request::Video { .. }
    );
    let image = match request {
        Request::Start { .. } => {
            // This worker runs in the interactive session: its settings are
            // the user's, which the service in session 0 cannot read.
            response.double_click = backend.double_click_guard();
            vec![]
        }
        Request::Context {} | Request::Release {} | Request::Close {} | Request::Input { .. } => {
            vec![]
        }
        Request::Attach { .. } => return Err(error("worker is already attached")),
        Request::H265 {} => {
            response.h265 = Some(
                backend
                    .describe(true)
                    .is_ok_and(|described| described.caps.h265),
            );
            vec![]
        }
        Request::ConfigureVideo {
            limits,
            nominal_fps,
        } => {
            if !(1..=120).contains(&nominal_fps) {
                return Err(error("invalid video configuration"));
            }
            backend.configure_video(limits, nominal_fps)?;
            vec![]
        }
        Request::Capture {} => backend.capture_consistent(&context)?,
        Request::Video {
            expected,
            codec,
            submit,
            keyframe,
            rate,
            wait_ms,
        } => {
            if expected != context
                || wait_ms > DRAIN_WAIT_MAX_MS
                || submit
                    && (!(crate::qos::ENCODER_BITRATE_MIN..=crate::qos::ENCODER_BITRATE_MAX)
                        .contains(&rate.bitrate)
                        || !(1..=120).contains(&rate.pace_fps))
            {
                return Err(error("invalid or stale video request"));
            }
            let frame = if submit {
                backend.capture_video(&expected, codec, keyframe, rate)?
            } else {
                drain(
                    backend,
                    &expected,
                    std::time::Duration::from_millis(wait_ms.into()),
                )?
            };
            match frame {
                Some(frame) => {
                    response.video = Some(frame.format);
                    frame.bytes
                }
                None => vec![],
            }
        }
        Request::Presented { generation } => {
            if generation != context.generation {
                return Err(error("stale helper presentation"));
            }
            *last_input = std::time::Instant::now();
            vec![]
        }
    };
    if observe_cursor {
        response.cursor = backend.cursor(&context)?;
    }
    response.context = Some(context);
    Ok((response, image))
}

/// Drain one unit, waiting up to `wait` while the encoder still holds units,
/// so the host polls a busy encoder without a pipe round trip per poll.
#[cfg(feature = "windows-helper")]
fn drain(
    backend: &mut dyn Backend,
    expected: &DesktopContext,
    wait: std::time::Duration,
) -> Result<Option<crate::EncodedFrame>> {
    let until = std::time::Instant::now() + wait;
    loop {
        if let Some(frame) = backend.drain_video(expected, std::time::Duration::ZERO)? {
            return Ok(Some(frame));
        }
        if backend.pending_video() == 0 || std::time::Instant::now() >= until {
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[cfg(all(test, feature = "windows-helper"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an unlocked interactive Windows desktop; sends a zero-distance mouse move"]
    fn bound_input_desktop_allows_send_input() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
        };

        // Use a fresh thread with no windows/hooks, just like the worker. A
        // zero-distance relative move checks the OS permission without clicking,
        // typing, moving the pointer or switching the user's visible desktop.
        std::thread::spawn(|| unsafe {
            let original = GetThreadDesktop(GetCurrentThreadId()).unwrap();
            let desktop =
                OpenInputDesktop(Default::default(), false, INPUT_DESKTOP_ACCESS).unwrap();
            let result = (|| -> Result<u32> {
                SetThreadDesktop(desktop).map_err(error)?;
                let input = INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dwFlags: MOUSEEVENTF_MOVE,
                            ..Default::default()
                        },
                    },
                };
                Ok(SendInput(&[input], size_of::<INPUT>() as i32))
            })();
            SetThreadDesktop(original).unwrap();
            CloseDesktop(desktop).unwrap();
            assert_eq!(result.unwrap(), 1, "desktop handle denied native input");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn desktop_transition_releases_before_binding_and_retries_without_committing_failure() {
        let mut state = BindingState::default();
        let live_resources = std::cell::Cell::new(true);
        state
            .enter(
                "default".into(),
                1,
                os::ConsoleState::Unlocked,
                || live_resources.set(false),
                || {
                    assert!(!live_resources.get());
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(state.generation, 1);
        live_resources.set(true);
        state
            .enter(
                "default".into(),
                1,
                os::ConsoleState::Unlocked,
                || panic!("unchanged desktop must keep its encoder"),
                || panic!("unchanged desktop must keep its binding"),
            )
            .unwrap();
        assert!(live_resources.get());
        assert!(
            state
                .enter(
                    "winlogon".into(),
                    1, // The event was missed; the name still forces a switch.
                    os::ConsoleState::Unlocked,
                    || live_resources.set(false),
                    || {
                        assert!(!live_resources.get());
                        Err(error("desktop busy"))
                    },
                )
                .is_err()
        );
        assert_eq!(state.name, "default");
        assert_eq!(state.generation, 1);
        state
            .enter(
                "winlogon".into(),
                1,
                os::ConsoleState::Unlocked,
                || {},
                || Ok(()),
            )
            .unwrap();
        assert_eq!(state.name, "winlogon");
        assert_eq!(state.generation, 2);
        // An away-and-back event invalidates resources even with the same name.
        state
            .enter(
                "winlogon".into(),
                2,
                os::ConsoleState::Unlocked,
                || {},
                || Ok(()),
            )
            .unwrap();
        assert_eq!(state.generation, 3);
        // UAC and the lock UI can share the Winlogon desktop name. A lock
        // transition must still invalidate pending input if no hook arrived.
        live_resources.set(true);
        state
            .enter(
                "winlogon".into(),
                2,
                os::ConsoleState::Locked,
                || live_resources.set(false),
                || {
                    assert!(!live_resources.get());
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(state.generation, 4);
    }

    #[test]
    fn a_console_answer_stands_only_within_one_epoch_and_window_off_winlogon() {
        use os::ConsoleState::{Locked, Unlocked};
        let memo = ConsoleMemo::default();
        let ordinary = "default";
        let t0 = std::time::Instant::now();
        let soon = t0 + std::time::Duration::from_millis(3);
        assert_eq!(
            memo.get(ordinary, 1, t0, || Ok(Unlocked)).unwrap(),
            Unlocked
        );
        assert_eq!(
            memo.get(ordinary, 1, soon, || panic!("asked again"))
                .unwrap(),
            Unlocked
        );
        // A desktop switch asks again inside the window, and so does age.
        assert_eq!(memo.get(ordinary, 2, soon, || Ok(Locked)).unwrap(), Locked);
        let later = soon + CONSOLE_REUSE;
        assert_eq!(
            memo.get(ordinary, 2, later, || Ok(Unlocked)).unwrap(),
            Unlocked
        );
        // A forced check replaces the answer, and a failure is never kept.
        assert!(memo.fresh(2, later, || Err(error("logged off"))).is_err());
        assert_eq!(memo.get(ordinary, 2, later, || Ok(Locked)).unwrap(), Locked);
        // UAC and the lock UI share Winlogon, where a lock may raise no switch:
        // the answer kept just now must not hide one.
        assert_eq!(
            memo.get("winlogon", 2, later, || Ok(Unlocked)).unwrap(),
            Unlocked
        );
    }

    #[test]
    fn input_reaches_only_the_desktop_the_host_expected() {
        /// The bound desktop, which the test changes under the host.
        struct Bound {
            context: DesktopContext,
            injected: u32,
        }
        impl Backend for Bound {
            fn display(&mut self) -> Result<crate::Display> {
                Ok(self.context.display.clone())
            }
            fn context(&mut self) -> Result<DesktopContext> {
                Ok(self.context.clone())
            }
            fn capture(&mut self) -> Result<Vec<u8>> {
                Ok(vec![])
            }
            fn input(&mut self, _: &Input) -> Result<()> {
                self.injected += 1;
                Ok(())
            }
            fn release(&mut self) {}
        }
        let expected = DesktopContext {
            display: crate::Display {
                id: 1,
                name: "bound".into(),
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
            generation: 1,
            desktop: crate::DesktopKind::Ordinary,
        };
        let mut backend = Bound {
            context: expected.clone(),
            injected: 0,
        };
        let (mut sequence, mut last_input) = (0, std::time::Instant::now());
        let mut send = |backend: &mut Bound, next| {
            let request = Request::Input {
                expected: expected.clone(),
                sequence: next,
                input: Input::Move { x: 1, y: 1 },
            };
            serve(backend, request, &mut sequence, &mut last_input).map(|(response, _)| response)
        };
        // The host keeps the desktop an answer reports, for its pointer.
        let response = send(&mut backend, 1).unwrap();
        assert_eq!(response.context, Some(expected.clone()));
        assert!(send(&mut backend, 1).is_err(), "a repeated sequence");
        // The same generation on a resized display is another desktop.
        backend.context.display.width = 32;
        assert!(send(&mut backend, 2).is_err());
        assert_eq!(backend.injected, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_worker_waits_out_a_paused_host() {
        async fn answer(host: &mut tokio::io::DuplexStream) -> Result<Response> {
            let response = ipc::read_json(host).await?;
            ipc::read_bytes(host, ipc::MAX_FRAME).await?;
            Ok(response)
        }
        let (mut host, mut worker) = tokio::io::duplex(1 << 16);
        let exchange = tokio::spawn(async move {
            exchange(&mut worker, 1, |_| async {
                Ok((
                    Response {
                        context: None,
                        error: None,
                        video: None,
                        cursor: None,
                        pending: 0,
                        double_click: None,
                        h265: None,
                    },
                    vec![],
                ))
            })
            .await
        });
        answer(&mut host).await.unwrap();
        // The viewer stays paused for a minute: its host sends nothing.
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        ipc::write_json(&mut host, &Request::Context {})
            .await
            .unwrap();
        answer(&mut host).await.unwrap();
        ipc::write_json(&mut host, &Request::Close {})
            .await
            .unwrap();
        answer(&mut host).await.unwrap();
        exchange.await.unwrap().unwrap();
    }

    #[test]
    fn desktop_hook_exits_when_startup_is_abandoned_or_owner_drops() {
        // Registers a passive hook only; never changes desktops or sends input.
        let (ready, receive) = mpsc::sync_channel(1);
        let (accept, accepted) = mpsc::sync_channel(1);
        let (finished, finish) = mpsc::sync_channel(1);
        let join = std::thread::spawn(move || {
            Events::listen(ready, accepted);
            let _ = finished.send(());
        });
        receive.recv_timeout(TIMEOUT).unwrap().unwrap();
        drop(accept);
        finish.recv_timeout(TIMEOUT).unwrap();
        join.join().unwrap();
        drop(Events::new().unwrap());
    }
}
