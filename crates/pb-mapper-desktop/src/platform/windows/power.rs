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
use pb_mapper_core::power::windows::PowerRequest;
use std::{
    sync::OnceLock,
    thread::Thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    System::Power::{ES_DISPLAY_REQUIRED, SetThreadExecutionState},
    UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
    },
};

static TOUCHES: Touches = Touches::new();
/// The watchdog, once the first touch started it; `None` if it could not be.
static WATCHDOG: OnceLock<Option<Thread>> = OnceLock::new();
/// Rate-limits display wake nudges while a sleeping DP monitor is still
/// re-enumerating.
static LAST_NUDGE: parking_lot::Mutex<Option<Instant>> = parking_lot::const_mutex(None);
const NUDGE_EVERY: Duration = Duration::from_secs(2);

/// Capture ran: keep the display and the system awake for `LINGER` more.
/// Never blocks; only the first touch after an idle spell wakes the
/// watchdog, and only the first of all starts it. Returns `true` when this
/// touch woke the watchdog from idle.
pub(crate) fn touch() -> bool {
    if TOUCHES.touch(Instant::now()) {
        wake();
        return true;
    }
    false
}

/// Nudge the active input desktop on the calling thread with a zero-delta
/// relative mouse move so Windows wakes a powered-off or Modern Standby
/// display (including `Winlogon` in the helper worker, where the detached
/// watchdog thread is not bound). When `periodic` is true, rate-limited to
/// once every `NUDGE_EVERY`.
pub(crate) fn nudge_input_desktop(periodic: bool) {
    let now = Instant::now();
    {
        let mut last = LAST_NUDGE.lock();
        if periodic
            && last.is_some_and(|at| {
                now.checked_duration_since(at)
                    .is_some_and(|age| age < NUDGE_EVERY)
            })
        {
            return;
        }
        *last = Some(now);
    }
    let input = [INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }];
    // SAFETY: `input` is a valid single-element array on the stack; a
    // relative (0, 0) move leaves the cursor position unchanged.
    unsafe {
        let _ = SendInput(&input, size_of::<INPUT>() as i32);
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
struct Awake(Option<PowerRequest>);

impl Power for Awake {
    fn acquire(&mut self) {
        self.0 = PowerRequest::for_capture()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows takes the request as built: the reason context and both
    /// kinds are accepted. Dropping it clears and closes it.
    #[test]
    fn a_power_request_sets_both_kinds() -> std::io::Result<()> {
        drop(PowerRequest::for_capture()?);
        Ok(())
    }
}
