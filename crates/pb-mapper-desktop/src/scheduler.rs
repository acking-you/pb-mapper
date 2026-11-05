//! A capture deadline and input/stop notification share one wait. On Windows,
//! condition-variable timeout granularity can turn a 2 ms codec poll into a
//! 15.6 ms wait. Use a per-worker high-resolution timer, without changing the
//! timer resolution of the process or the machine.

#[cfg(windows)]
pub(crate) use native::{Waiter, Wake};

#[cfg(windows)]
mod native {
    use crate::{Result, error};
    use std::{
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        sync::{Arc, OnceLock, mpsc},
        time::Duration,
    };
    use windows::Win32::{
        Foundation::{HANDLE, WAIT_FAILED},
        System::Threading::*,
    };

    #[derive(Clone)]
    pub(crate) struct Wake(Arc<OnceLock<Arc<OwnedHandle>>>);
    impl Wake {
        pub fn new() -> Self {
            Self(Arc::default())
        }

        pub fn notify(&self) {
            if let Some(event) = self.0.get() {
                // The waiter and sender both retain the handle until use ends.
                unsafe {
                    let _ = SetEvent(HANDLE(event.as_raw_handle()));
                }
            }
        }
    }

    pub(crate) struct Waiter {
        event: Arc<OwnedHandle>,
        timer: OwnedHandle,
    }
    impl Waiter {
        pub fn new(wake: &Wake) -> Result<Self> {
            let event = unsafe { CreateEventW(None, false, false, None).map_err(error)? };
            let event = Arc::new(unsafe { OwnedHandle::from_raw_handle(event.0) });
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
                .set(event.clone())
                .map_err(|_| error("capture waiter already initialized"))?;
            Ok(Self { event, timer })
        }

        pub fn receive<T>(
            &self,
            incoming: &mpsc::Receiver<T>,
            timeout: Duration,
        ) -> Result<Option<T>> {
            match incoming.try_recv() {
                Ok(value) => return Ok(Some(value)),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(error("capture command channel closed"));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if timeout.is_zero() {
                return Ok(None);
            }
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
                let result = WaitForMultipleObjects(
                    &[
                        HANDLE(self.event.as_raw_handle()),
                        HANDLE(self.timer.as_raw_handle()),
                    ],
                    false,
                    INFINITE,
                );
                if result == WAIT_FAILED {
                    return Err(std::io::Error::last_os_error());
                }
            }
            // A stop notification intentionally has no queued command. Return
            // to the outer cancellation check instead of waiting again.
            match incoming.try_recv() {
                Ok(value) => Ok(Some(value)),
                Err(mpsc::TryRecvError::Empty) => Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(error("capture command channel closed"))
                }
            }
        }
    }
}

#[cfg(not(windows))]
#[derive(Clone)]
pub(crate) struct Wake;
#[cfg(not(windows))]
impl Wake {
    pub fn new() -> Self {
        Self
    }

    pub fn notify(&self) {}
}
#[cfg(not(windows))]
pub(crate) struct Waiter;
#[cfg(not(windows))]
impl Waiter {
    pub fn new(_: &Wake) -> crate::Result<Self> {
        Ok(Self)
    }
    pub fn receive<T>(
        &self,
        incoming: &std::sync::mpsc::Receiver<T>,
        timeout: std::time::Duration,
    ) -> crate::Result<Option<T>> {
        match incoming.recv_timeout(timeout) {
            Ok(value) => Ok(Some(value)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(crate::error("capture command channel closed"))
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};
    #[test]
    fn wake_preserves_queued_input_and_interrupts_idle_stop() -> crate::Result<()> {
        let wake = Wake::new();
        let waiter = Waiter::new(&wake)?;
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(7u32).map_err(crate::error)?;
        wake.notify();
        assert_eq!(waiter.receive(&receiver, Duration::from_secs(2))?, Some(7));
        // Consume the coalesced wake left by queued input before testing stop.
        assert_eq!(waiter.receive(&receiver, Duration::from_secs(2))?, None);
        let (completed, completion) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = waiter.receive(&receiver, Duration::from_secs(30));
            let _ = completed.send(result);
        });
        wake.notify();
        // The timer is 30 s; only the input/stop event can finish this wait.
        assert_eq!(
            completion
                .recv_timeout(Duration::from_secs(2))
                .map_err(crate::error)??,
            None
        );
        thread.join().map_err(|_| crate::error("waiter panicked"))?;
        drop(sender);
        Ok(())
    }
}
