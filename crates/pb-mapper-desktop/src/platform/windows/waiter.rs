//! Thread C's wait on Windows: a per-worker high-resolution waitable timer and
//! an event, because a condition variable's timeout can turn a 2 ms codec poll
//! into a 15.6 ms wait. The process's and the machine's timer resolution stay
//! as they are.

use crate::{Result, error};
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use windows::Win32::{
    Foundation::{HANDLE, WAIT_FAILED},
    System::Threading::*,
};

#[derive(Default)]
struct Signal {
    /// The wake itself; the event only ends a blocked wait.
    pending: AtomicBool,
    event: OnceLock<OwnedHandle>,
}

#[derive(Clone)]
pub(crate) struct Wake(Arc<Signal>);
impl Wake {
    pub fn new() -> Self {
        Self(Arc::default())
    }

    pub fn notify(&self) {
        self.0.pending.store(true, Ordering::Release);
        if let Some(event) = self.0.event.get() {
            // The signal owns the handle for as long as any wake exists.
            unsafe {
                let _ = SetEvent(HANDLE(event.as_raw_handle()));
            }
        }
    }
}

pub(crate) struct Waiter {
    signal: Arc<Signal>,
    timer: OwnedHandle,
}
impl Waiter {
    pub fn new(wake: &Wake) -> Result<Self> {
        let event = unsafe { CreateEventW(None, false, false, None).map_err(error)? };
        let event = unsafe { OwnedHandle::from_raw_handle(event.0) };
        let timer = unsafe {
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
            .map_err(error)?
        };
        let timer = unsafe { OwnedHandle::from_raw_handle(timer.0) };
        wake.0
            .event
            .set(event)
            .map_err(|_| error("capture waiter already initialized"))?;
        Ok(Self {
            signal: wake.0.clone(),
            timer,
        })
    }

    /// Wait until `deadline` or a wake. True when a wake, sent before or
    /// during the wait, ended it; the wake is consumed.
    pub fn wait(&self, deadline: Instant) -> Result<bool> {
        let event = HANDLE(
            self.signal
                .event
                .get()
                .ok_or_else(|| error("capture waiter is not initialized"))?
                .as_raw_handle(),
        );
        let timeout = deadline.saturating_duration_since(Instant::now());
        if !self.signal.pending.load(Ordering::Acquire) && !timeout.is_zero() {
            let due = -(timeout.as_nanos().div_ceil(100).clamp(1, i64::MAX as u128) as i64);
            unsafe {
                SetWaitableTimer(
                    HANDLE(self.timer.as_raw_handle()),
                    &due,
                    0,
                    None,
                    None,
                    false,
                )
                .map_err(error)?;
                if WaitForMultipleObjects(
                    &[event, HANDLE(self.timer.as_raw_handle())],
                    false,
                    INFINITE,
                ) == WAIT_FAILED
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        let woken = self.signal.pending.swap(false, Ordering::AcqRel);
        if woken {
            // Clear the event that wake signalled, so the next wait does
            // not end early. A newer wake stays pending in the flag.
            unsafe {
                WaitForSingleObject(event, 0);
            }
        }
        Ok(woken)
    }
}
