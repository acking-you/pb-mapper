//! Fine process timers while a host session streams. In a process with no
//! visible window, Windows 11 fires every timer on its 15.6 ms tick (tokio's
//! too: B-send's credit recheck, quinn's pacing and ACK timers), and it ignores
//! `timeBeginPeriod` there unless the process first opts out of power
//! throttling for timer resolution. A guard does both, counted across the
//! process's sessions, and the last one dropped undoes both, so an idle host
//! costs no power. Every call may fail harmlessly: timers then stay coarse.
//! Thread C keeps its own high-resolution waitable timer either way.

/// Fine timers for this process while any guard lives.
pub(crate) struct TimerResolution(());
impl TimerResolution {
    pub fn hold() -> Self {
        crate::platform::timers::acquire();
        Self(())
    }
}
impl Drop for TimerResolution {
    fn drop(&mut self) {
        crate::platform::timers::release();
    }
}
