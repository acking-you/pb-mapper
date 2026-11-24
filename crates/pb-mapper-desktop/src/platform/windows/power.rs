//! Keeps a Windows host's display and the system awake while a session
//! captures. A dark display gives Desktop Duplication no new pictures, so a
//! viewer that only watches would freeze at the owner's display-off timeout,
//! and a laptop that sleeps ends the session. A viewer's pointer moves do not
//! count as input either (`SetCursorPos`).
//!
//! Every capture call and pointer read calls [`touch`], one atomic store, in
//! the app's process and in the background helper's worker alike, which runs
//! in the owner's console session. A watchdog thread, started by the first
//! touch and parked while nothing captures, holds one power request (display
//! and system required) from the first capture after an idle spell until
//! `awake::LINGER` after the last, and turns a dark display on when it takes
//! it, so a session that starts or resumes on one gets its first picture
//! without anyone at the host. A lid close, the power button or a sleep the
//! owner chooses still win: Windows ends every request then. Process exit
//! closes the request. When to take it and give it back is
//! `awake::Watchdog`'s decision; this module only carries it out.

use crate::platform::awake::{Power, Touches, Wait, Watchdog};
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::OnceLock,
    thread::Thread,
    time::Instant,
};
use windows::{
    Win32::{
        Foundation::HANDLE,
        System::{
            Power::{
                ES_DISPLAY_REQUIRED, POWER_REQUEST_TYPE, PowerClearRequest, PowerCreateRequest,
                PowerRequestDisplayRequired, PowerRequestSystemRequired, PowerSetRequest,
                SetThreadExecutionState,
            },
            Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
        },
    },
    core::{PCWSTR, PWSTR, w},
};

static TOUCHES: Touches = Touches::new();
/// The watchdog, once the first touch started it; `None` if it could not be.
static WATCHDOG: OnceLock<Option<Thread>> = OnceLock::new();

/// Capture ran: keep the display and the system awake for `LINGER` more.
/// Never blocks; only the first touch after an idle spell wakes the
/// watchdog, and only the first of all starts it.
pub(crate) fn touch() {
    if TOUCHES.touch(Instant::now()) {
        wake();
    }
}

#[cold]
fn wake() {
    let watchdog = WATCHDOG.get_or_init(|| {
        match std::thread::Builder::new()
            .name("pb-desktop-awake".into())
            .spawn(watch)
        {
            // Detached: it lives as long as the process.
            Ok(handle) => Some(handle.thread().clone()),
            Err(failure) => {
                tracing::warn!(
                    target: "pb_mapper_desktop::session",
                    event = "desktop_keep_awake_failed",
                    detail = %failure,
                );
                // No watchdog will park: touches stop asking for one.
                TOUCHES.run();
                None
            }
        }
    });
    if let Some(thread) = watchdog {
        thread.unpark();
    }
}

/// The watchdog's thread: looks, then sleeps or parks as the look says, for
/// the life of the process.
fn watch() {
    let mut watchdog = Watchdog::new(&TOUCHES);
    let mut power = Awake::default();
    loop {
        match watchdog.look(Instant::now(), &mut power) {
            Wait::Until(until) => {
                std::thread::sleep(until.saturating_duration_since(Instant::now()));
            }
            Wait::Touch => std::thread::park(),
        }
    }
}

/// The power request while the watchdog holds it; `None` if Windows would
/// not create it.
#[derive(Default)]
struct Awake(Option<Request>);

impl Power for Awake {
    fn acquire(&mut self) {
        self.0 = Request::create()
            .inspect_err(|failure| {
                tracing::warn!(
                    target: "pb_mapper_desktop::session",
                    event = "desktop_keep_awake_failed",
                    detail = %failure,
                );
            })
            .ok();
        // SAFETY: takes flags only. Without ES_CONTINUOUS it resets the idle
        // timers once, as if someone at the host touched it, which turns a
        // dark display on.
        unsafe { SetThreadExecutionState(ES_DISPLAY_REQUIRED) };
        tracing::debug!(
            target: "pb_mapper_desktop::session",
            event = "desktop_keep_awake",
            held = self.0.is_some(),
        );
    }

    fn release(&mut self) {
        drop(self.0.take());
        tracing::debug!(
            target: "pb_mapper_desktop::session",
            event = "desktop_keep_awake",
            held = false,
        );
    }
}

/// `powercfg /requests` shows it beside the process.
const REASON: PCWSTR = w!("pb-mapper: a remote desktop viewer is watching this screen");
/// `POWER_REQUEST_CONTEXT_VERSION`, in a module this crate does not build.
const CONTEXT_VERSION: u32 = 0;
const KINDS: [POWER_REQUEST_TYPE; 2] = [PowerRequestDisplayRequired, PowerRequestSystemRequired];

/// One power request with the display and the system required. Dropping it
/// clears both and closes it.
struct Request(OwnedHandle);

impl Request {
    fn create() -> windows::core::Result<Self> {
        let reason = REASON_CONTEXT {
            Version: CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: PWSTR(REASON.as_ptr().cast_mut()),
            },
        };
        // SAFETY: `reason` and the static string it names outlive the call,
        // which only reads them.
        let handle = unsafe { PowerCreateRequest(&reason) }?;
        // SAFETY: a valid handle that nothing else owns or closes.
        let request = Self(unsafe { OwnedHandle::from_raw_handle(handle.0) });
        for kind in KINDS {
            // SAFETY: the request's own open handle. On failure, drop
            // clears what was set and closes it.
            unsafe { PowerSetRequest(request.handle(), kind) }?;
        }
        Ok(request)
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        for kind in KINDS {
            // SAFETY: the request's own open handle; clearing a kind that
            // was never set only fails. The handle closes after this.
            let _ = unsafe { PowerClearRequest(self.handle(), kind) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows takes the request as built: the reason context and both
    /// kinds are accepted. Dropping it clears and closes it.
    #[test]
    fn a_power_request_sets_both_kinds() -> windows::core::Result<()> {
        drop(Request::create()?);
        Ok(())
    }
}
