//! The privileged worker's binding to the input desktop of its approved
//! console login: which desktop that is, whether it changed, and the capture
//! thread's switch to it.
use super::os;
use crate::{Result, error, platform::windows::win32::desktop_name};
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::{Foundation::*, System::StationsAndDesktops::*, UI::WindowsAndMessaging::*};
#[cfg(feature = "windows-helper")]
use {
    crate::oop::proto::TIMEOUT,
    std::sync::mpsc,
    windows::Win32::{System::Threading::*, UI::Accessibility::*},
};

static EPOCH: AtomicU64 = AtomicU64::new(1);
// SessionIdentity::check is an RPC to the session manager (~120 µs), and one
// helper request checks several times. An answer stands this long while no
// desktop switch is seen and the console session is still ours: lock, unlock
// and UAC switch desktops, and a logoff and a new login take far longer. Not
// on the Winlogon desktop: UAC and the lock UI share it, so a lock there may
// raise no switch and the console state is the only signal.
const CONSOLE_REUSE: std::time::Duration = std::time::Duration::from_millis(4);

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
            let name = desktop_name(desktop)?.to_ascii_lowercase();
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

#[cfg(all(test, feature = "windows-helper"))]
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
