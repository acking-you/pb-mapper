//! Thread C's one wait: its next deadline, or a wake from a lane push, a pace
//! change, a pause, a resume, an IDR force or a cancel. A wake sent while C is
//! busy stays pending, so the next wait returns at once.
//!
//! On Windows, condition-variable timeout granularity can turn a 2 ms codec
//! poll into a 15.6 ms wait, so the wait uses a per-worker high-resolution
//! timer, without changing the timer resolution of the process or the machine.
//! On macOS, where the kernel may coalesce a background process's timers to
//! four times their interval, it is a kqueue timer marked critical. Elsewhere
//! it is a condition variable.

pub(crate) use crate::platform::{Waiter, Wake};

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    /// A wake sent before a wait ends it at once, and only once; a wake from
    /// another thread ends a 1 s wait. A wake once consumed ends no later
    /// wait early, whether it came before its wait or after a wait's timer
    /// fired.
    macro_rules! wake_ends_a_wait {
        ($implementation:path) => {{
            use $implementation::{Waiter, Wake};
            /// A 20 ms wait that no stale wake ends early.
            macro_rules! lasts {
                ($waiter:expr) => {{
                    let started = Instant::now();
                    assert!(!$waiter.wait(started + Duration::from_millis(20))?);
                    assert!(started.elapsed() >= Duration::from_millis(15));
                }};
            }
            let wake = Wake::new();
            let waiter = Waiter::new(&wake)?;
            wake.notify();
            let started = Instant::now();
            assert!(waiter.wait(started + Duration::from_secs(1))?);
            assert!(started.elapsed() < Duration::from_millis(50));
            lasts!(waiter);

            assert!(!waiter.wait(Instant::now() + Duration::from_millis(2))?);
            wake.notify();
            assert!(waiter.wait(Instant::now() + Duration::from_secs(1))?);
            lasts!(waiter);

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
        // The platform's own, and the portable one every platform may use.
        wake_ends_a_wait!(crate::platform);
        wake_ends_a_wait!(crate::platform::waiter);
        Ok(())
    }
}
