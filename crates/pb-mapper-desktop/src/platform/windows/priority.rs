//! A thread's priority on Windows: `THREAD_PRIORITY_ABOVE_NORMAL`, one step
//! above the ordinary threads of a process in the same priority class. It
//! needs no privilege. A thread already at or above it is left alone, and a
//! raised one gets back the exact priority it had.

use windows::Win32::System::Threading::{
    GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY,
    THREAD_PRIORITY_ABOVE_NORMAL,
};

/// What `GetThreadPriority` answers when it fails,
/// `THREAD_PRIORITY_ERROR_RETURN`, which the `windows` crate keeps behind a
/// feature nothing else here needs.
const ERROR_RETURN: i32 = i32::MAX;

/// The priority this thread had before it was raised.
pub(crate) struct Before(THREAD_PRIORITY);

/// Raise this thread above normal; its earlier priority, if this did.
pub(crate) fn raise() -> Option<Before> {
    // SAFETY: `GetCurrentThread` is a pseudo handle for the calling thread,
    // valid for the call and never closed.
    let current = unsafe { GetThreadPriority(GetCurrentThread()) };
    if current == ERROR_RETURN || current >= THREAD_PRIORITY_ABOVE_NORMAL.0 {
        return None;
    }
    // SAFETY: as above.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) }.ok()?;
    Some(Before(THREAD_PRIORITY(current)))
}

/// Put this thread's priority back. A failure leaves it raised.
pub(crate) fn restore(before: Before) {
    // SAFETY: as in `raise`.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), before.0) };
}

/// The levels the tests set and read.
#[cfg(test)]
pub(crate) mod testing {
    use windows::Win32::System::Threading::{
        GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY,
        THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_HIGHEST,
    };

    pub(crate) type Level = i32;
    pub(crate) const RAISED: Level = THREAD_PRIORITY_ABOVE_NORMAL.0;
    /// Below the raised level.
    pub(crate) const LOWER: Level = THREAD_PRIORITY_BELOW_NORMAL.0;
    /// Above the raised level.
    pub(crate) const HIGHER: Level = THREAD_PRIORITY_HIGHEST.0;

    pub(crate) fn current() -> Level {
        // SAFETY: the calling thread's pseudo handle, as in `raise`.
        unsafe { GetThreadPriority(GetCurrentThread()) }
    }

    pub(crate) fn set(level: Level) -> bool {
        // SAFETY: as above.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY(level)) }.is_ok()
    }
}
