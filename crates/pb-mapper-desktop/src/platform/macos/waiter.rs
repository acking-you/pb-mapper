//! Thread C's wait on macOS: a kqueue with a one-shot timer marked
//! `NOTE_CRITICAL` and a user event for the wake. The kernel may coalesce
//! the timers of a process that is not the frontmost app: on a CI runner a
//! 2 ms condition-variable wait ended 8 ms late and a 16.7 ms tick up to
//! 67 ms late (four times the interval, at most 75 ms), so thread C paced
//! at about 20 fps instead of 60. Neither the task's latency QoS override,
//! the thread's QoS class nor an `NSProcessInfo` activity changed that; a
//! critical timer is exempt, and ended within about 1 ms of a 2 ms wait and
//! 3.6 ms of a 16.7 ms one there.

use crate::{Result, error};
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// The queue's user event, which a wake triggers.
const WAKE: usize = 1;
/// The queue's timer, armed for each wait.
const TIMER: usize = 2;
/// Apply changes, or collect what is ready, without blocking.
const NOW: libc::timespec = libc::timespec {
    tv_sec: 0,
    tv_nsec: 0,
};

#[derive(Default)]
struct Signal {
    /// The wake itself; the user event only ends a blocked wait.
    pending: AtomicBool,
    queue: OnceLock<OwnedFd>,
}

fn event(ident: usize, filter: i16, flags: u16, fflags: u32, data: isize) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data,
        udata: std::ptr::null_mut(),
    }
}

/// Apply `changes`, then collect up to `ready.len()` events, waiting until
/// one arrives or, when given, `timeout` passes.
fn kevent(
    queue: &OwnedFd,
    changes: &[libc::kevent],
    ready: &mut [libc::kevent],
    timeout: Option<&libc::timespec>,
) -> io::Result<usize> {
    // SAFETY: the lists are live slices of their stated lengths (one or two
    // events), and the queue is open for as long as `queue` lives.
    let count = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            changes.as_ptr(),
            changes.len() as libc::c_int,
            ready.as_mut_ptr(),
            ready.len() as libc::c_int,
            timeout.map_or(std::ptr::null(), |timeout| timeout),
        )
    };
    usize::try_from(count).map_err(|_| io::Error::last_os_error())
}

#[derive(Clone)]
pub(crate) struct Wake(Arc<Signal>);
impl Wake {
    pub fn new() -> Self {
        Self(Arc::default())
    }

    pub fn notify(&self) {
        self.0.pending.store(true, Ordering::Release);
        if let Some(queue) = self.0.queue.get() {
            // Ends a blocked wait. Should it fail, the wake stays pending
            // and ends the next wait instead, at its deadline at the latest.
            let trigger = event(WAKE, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER, 0);
            let _ = kevent(queue, &[trigger], &mut [], Some(&NOW));
        }
    }
}

pub(crate) struct Waiter {
    signal: Arc<Signal>,
}
impl Waiter {
    pub fn new(wake: &Wake) -> Result<Self> {
        // SAFETY: takes nothing; a new descriptor or -1.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a descriptor just opened and owned by nothing else.
        let queue = unsafe { OwnedFd::from_raw_fd(raw) };
        let user = event(WAKE, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0, 0);
        kevent(&queue, &[user], &mut [], Some(&NOW))?;
        wake.0
            .queue
            .set(queue)
            .map_err(|_| error("capture waiter already initialized"))?;
        Ok(Self {
            signal: wake.0.clone(),
        })
    }

    /// Wait until `deadline` or a wake. True when a wake, sent before or
    /// during the wait, ended it; the wake is consumed.
    pub fn wait(&self, deadline: Instant) -> Result<bool> {
        let queue = self
            .signal
            .queue
            .get()
            .ok_or_else(|| error("capture waiter is not initialized"))?;
        let timeout = deadline.saturating_duration_since(Instant::now());
        if !self.signal.pending.load(Ordering::Acquire) && !timeout.is_zero() {
            let nanos = isize::try_from(timeout.as_nanos()).unwrap_or(isize::MAX);
            let timer = event(
                TIMER,
                libc::EVFILT_TIMER,
                libc::EV_ADD | libc::EV_ONESHOT,
                libc::NOTE_NSECONDS | libc::NOTE_CRITICAL,
                nanos,
            );
            let mut ready = [event(0, 0, 0, 0, 0); 2];
            match kevent(queue, &[timer], &mut ready, None) {
                Ok(_) => {}
                // A signal ends the wait early: thread C looks again.
                Err(failure) if failure.kind() == io::ErrorKind::Interrupted => {}
                Err(failure) => return Err(failure),
            }
        }
        let woken = self.signal.pending.swap(false, Ordering::AcqRel);
        if woken {
            // So that the next wait does not end early: drop the timer,
            // armed or fired, then collect the user event the wake
            // triggered. In two calls: a change that fails (the timer is
            // gone already) makes the kernel report it and scan nothing
            // else, which would leave the user event behind. A newer wake
            // stays pending in the flag.
            let timer = event(TIMER, libc::EVFILT_TIMER, libc::EV_DELETE, 0, 0);
            let _ = kevent(queue, &[timer], &mut [], Some(&NOW));
            let mut ready = [event(0, 0, 0, 0, 0); 2];
            let _ = kevent(queue, &[], &mut ready, Some(&NOW));
        }
        Ok(woken)
    }
}
