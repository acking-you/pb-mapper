//! Thread C's one wait: its next deadline, or a wake from a lane push, a pace
//! change, a pause, a resume, an IDR force or a cancel. A wake sent while C is
//! busy stays pending, so the next wait returns at once.
//!
//! On Windows, condition-variable timeout granularity can turn a 2 ms codec
//! poll into a 15.6 ms wait, so the wait uses a per-worker high-resolution
//! timer, without changing the timer resolution of the process or the machine.
//! Elsewhere it is a condition variable.

#[cfg(windows)]
pub(crate) use native::{Waiter, Wake};
#[cfg(not(windows))]
pub(crate) use portable::{Waiter, Wake};

#[cfg(windows)]
mod native {
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
}

// Also built for tests on Windows, so the one wake test covers both.
#[cfg(any(not(windows), test))]
mod portable {
    use std::{sync::Arc, time::Instant};

    #[derive(Default)]
    struct Signal {
        pending: parking_lot::Mutex<bool>,
        ready: parking_lot::Condvar,
    }

    #[derive(Clone)]
    pub(crate) struct Wake(Arc<Signal>);
    impl Wake {
        pub fn new() -> Self {
            Self(Arc::default())
        }

        pub fn notify(&self) {
            *self.0.pending.lock() = true;
            self.0.ready.notify_one();
        }
    }

    pub(crate) struct Waiter(Arc<Signal>);
    impl Waiter {
        pub fn new(wake: &Wake) -> crate::Result<Self> {
            Ok(Self(wake.0.clone()))
        }

        /// Wait until `deadline` or a wake. True when a wake, sent before or
        /// during the wait, ended it; the wake is consumed.
        pub fn wait(&self, deadline: Instant) -> crate::Result<bool> {
            let mut pending = self.0.pending.lock();
            while !*pending {
                if self.0.ready.wait_until(&mut pending, deadline).timed_out() {
                    break;
                }
            }
            Ok(std::mem::take(&mut *pending))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    /// A wake sent before a wait ends it at once, and only once; a wake from
    /// another thread ends a 1 s wait.
    macro_rules! wake_ends_a_wait {
        ($implementation:path) => {{
            use $implementation::{Waiter, Wake};
            let wake = Wake::new();
            let waiter = Waiter::new(&wake)?;
            wake.notify();
            let started = Instant::now();
            assert!(waiter.wait(started + Duration::from_secs(1))?);
            assert!(started.elapsed() < Duration::from_millis(50));
            assert!(!waiter.wait(Instant::now() + Duration::from_millis(5))?);

            let remote = wake.clone();
            let (sent, notified) = std::sync::mpsc::sync_channel(1);
            let notifier = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                let at = Instant::now();
                remote.notify();
                let _ = sent.send(at);
            });
            assert!(waiter.wait(Instant::now() + Duration::from_secs(1))?);
            let woken = Instant::now();
            let at = notified
                .recv_timeout(Duration::from_secs(1))
                .map_err(crate::error)?;
            notifier
                .join()
                .map_err(|_| crate::error("notifier panicked"))?;
            assert!(woken.saturating_duration_since(at) < Duration::from_millis(50));
        }};
    }

    #[test]
    fn wake_ends_a_wait_on_every_platform() -> crate::Result<()> {
        wake_ends_a_wait!(super::portable);
        #[cfg(windows)]
        wake_ends_a_wait!(super::native);
        Ok(())
    }
}
