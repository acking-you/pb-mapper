//! Thread C's wait where no platform has its own: a condition variable.

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
