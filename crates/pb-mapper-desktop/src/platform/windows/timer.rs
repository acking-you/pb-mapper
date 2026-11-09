//! Fine process timers on Windows: the power-throttling opt-out and
//! `timeBeginPeriod(1)`, counted across the process's sessions.

use windows::Win32::{
    Media::{TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod},
    System::Threading::{
        GetCurrentProcess, GetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
    },
};

struct Held {
    guards: usize,
    /// `timeBeginPeriod(1)` took effect.
    period: bool,
    /// The throttling state to restore, once the opt-out took effect.
    throttling: Option<PROCESS_POWER_THROTTLING_STATE>,
}
static HELD: parking_lot::Mutex<Held> = parking_lot::const_mutex(Held {
    guards: 0,
    period: false,
    throttling: None,
});
const SIZE: u32 = size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32;

/// A state with no bit under control: the system decides.
fn system() -> PROCESS_POWER_THROTTLING_STATE {
    PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    }
}

fn set(state: &PROCESS_POWER_THROTTLING_STATE) -> bool {
    unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            SIZE,
        )
    }
    .is_ok()
}

pub(crate) fn acquire() {
    let mut held = HELD.lock();
    held.guards += 1;
    if held.guards > 1 {
        return;
    }
    // What this process asked for before, restored at the end; where it
    // cannot be read, the system's own management.
    let mut before = system();
    let read = unsafe {
        GetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&mut before as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            SIZE,
        )
    }
    .is_ok();
    // Opted out of both: timers at the resolution asked for, and no
    // efficiency-class scheduling for the threads that pace and send.
    let opted_out = set(&PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION
            | PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    });
    held.throttling = opted_out.then(|| if read { before } else { system() });
    held.period = unsafe { timeBeginPeriod(1) } == TIMERR_NOERROR;
}

pub(crate) fn release() {
    let mut held = HELD.lock();
    held.guards = held.guards.saturating_sub(1);
    if held.guards > 0 {
        return;
    }
    if std::mem::take(&mut held.period) {
        unsafe {
            timeEndPeriod(1);
        }
    }
    if let Some(before) = held.throttling.take() {
        set(&before);
    }
}

#[cfg(test)]
mod tests {
    use crate::timer::TimerResolution;
    use std::time::{Duration, Instant};

    /// The median time a 2 ms tokio sleep takes.
    async fn sleep_2ms() -> Duration {
        let mut taken = Vec::new();
        for _ in 0..21 {
            let started = Instant::now();
            tokio::time::sleep(Duration::from_millis(2)).await;
            taken.push(started.elapsed());
        }
        taken.sort_unstable();
        taken[taken.len() / 2]
    }

    /// Run alone: another session of this process would hold a guard too.
    #[test]
    #[ignore = "measures this process's timer resolution; run alone on Windows 11"]
    fn tokio_timers_are_fine_only_while_a_guard_lives() -> crate::Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let before = sleep_2ms().await;
            let guard = TimerResolution::hold();
            let held = sleep_2ms().await;
            drop(guard);
            let after = sleep_2ms().await;
            println!("timer_resolution sleep_2ms before={before:?} held={held:?} after={after:?}");
            assert!(held < Duration::from_millis(5), "{held:?}");
            assert!(after > Duration::from_millis(10), "not reverted: {after:?}");
        });
        Ok(())
    }
}
