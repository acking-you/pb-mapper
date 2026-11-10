//! A slot holding one C function pointer that Dart installs and Rust calls:
//! the log callback and the change callback each have one.
//!
//! A call holds the slot's read lock until it returns, so clearing the slot
//! waits for the calls in flight, including those from the log worker. Dart
//! closes its `NativeCallable.listener` right after clearing, and a call that
//! started before must not reach a closed one. Such a callable only posts to
//! its isolate, so a call never blocks on Dart while it holds the lock.

use parking_lot::RwLock;

/// A nullable, thread-safe slot for a C function pointer of type `F`.
pub(crate) struct CallbackSlot<F: Copy>(RwLock<Option<F>>);

impl<F: Copy> CallbackSlot<F> {
    pub(crate) const fn empty() -> Self {
        Self(RwLock::new(None))
    }

    /// Installs `callback`, or clears the slot with `None`, once no call
    /// through the slot is in flight.
    pub(crate) fn store(&self, callback: Option<F>) {
        *self.0.write() = callback;
    }

    /// Runs `invoke` with the installed callback, holding the slot until it
    /// returns; `None` if nothing is listening.
    pub(crate) fn with<R>(&self, invoke: impl FnOnce(F) -> R) -> Option<R> {
        let installed = self.0.read();
        (*installed).map(invoke)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use super::*;

    extern "C" fn sample(value: i32) -> i32 {
        value + 1
    }

    #[test]
    fn a_slot_round_trips_a_function_pointer() {
        let slot: CallbackSlot<extern "C" fn(i32) -> i32> = CallbackSlot::empty();
        assert_eq!(
            slot.with(|callback| callback(1)),
            None,
            "an untouched slot is empty"
        );

        slot.store(Some(sample));
        assert_eq!(
            slot.with(|callback| callback(41)),
            Some(42),
            "the stored callback should still be callable"
        );

        slot.store(None);
        assert_eq!(
            slot.with(|callback| callback(1)),
            None,
            "None clears the slot"
        );
    }

    /// What lets Dart close a callable once it has cleared the slot.
    #[test]
    fn clearing_waits_for_a_call_in_flight() {
        let slot: Arc<CallbackSlot<extern "C" fn(i32) -> i32>> = Arc::new(CallbackSlot::empty());
        slot.store(Some(sample));
        let (entered, call_entered) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let caller = std::thread::spawn({
            let slot = slot.clone();
            move || {
                slot.with(|callback| {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                    callback(1)
                })
            }
        });
        call_entered.recv().unwrap();
        let (cleared, slot_cleared) = mpsc::channel();
        let clearer = std::thread::spawn({
            let slot = slot.clone();
            move || {
                slot.store(None);
                cleared.send(()).unwrap();
            }
        });
        assert!(
            slot_cleared
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "the slot was cleared while a call was in flight"
        );
        release.send(()).unwrap();
        slot_cleared.recv().unwrap();
        assert_eq!(caller.join().unwrap(), Some(2));
        clearer.join().unwrap();
        assert_eq!(slot.with(|callback| callback(1)), None);
    }
}
