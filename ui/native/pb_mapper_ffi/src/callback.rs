//! A slot holding one C function pointer that Dart installs and Rust calls.
//!
//! There is one of these per callback the UI registers — logging today, state
//! changes next. Written once so the `transmute` is written once: a function
//! pointer is not a sized target, so `AtomicPtr` has to hold it as `*mut ()`
//! and cast it back on every call, and that cast is the part worth not
//! repeating per callback.

use std::marker::PhantomData;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

/// A nullable, thread-safe slot for a C function pointer of type `F`.
///
/// `F` is expected to be an `extern "C" fn(..)` type. Anything else with the
/// same size would compile and then misbehave, which is why the constructor is
/// only reachable inside this crate and every slot is a `static`.
pub(crate) struct CallbackSlot<F: Copy> {
    ptr: AtomicPtr<()>,
    _marker: PhantomData<fn() -> F>,
}

impl<F: Copy> CallbackSlot<F> {
    pub(crate) const fn empty() -> Self {
        Self {
            ptr: AtomicPtr::new(ptr::null_mut()),
            _marker: PhantomData,
        }
    }

    /// Installs `callback`, or clears the slot with `None`.
    ///
    /// # Safety
    /// `callback` must be a valid function pointer for the lifetime of the
    /// process, or until it is replaced. Dart's `NativeCallable.listener` gives
    /// one that stays valid until `close()`, which is why the UI closes its
    /// callables in `dispose` before the handle goes away.
    pub(crate) fn store(&self, callback: Option<F>) {
        debug_assert_eq!(
            size_of::<F>(),
            size_of::<*mut ()>(),
            "CallbackSlot only holds pointer-sized function pointers"
        );
        let raw = match callback {
            // `transmute_copy` rather than `transmute`: the latter needs both
            // sizes known at compile time, which they are not through a generic.
            Some(f) => unsafe { std::mem::transmute_copy::<F, *mut ()>(&f) },
            None => ptr::null_mut(),
        };
        self.ptr.store(raw, Ordering::SeqCst);
    }

    /// The installed callback, or `None` if nothing is listening.
    pub(crate) fn load(&self) -> Option<F> {
        let raw = self.ptr.load(Ordering::SeqCst);
        if raw.is_null() {
            return None;
        }
        Some(unsafe { std::mem::transmute_copy::<*mut (), F>(&raw) })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    extern "C" fn sample(value: i32) -> i32 {
        value + 1
    }

    #[test]
    fn a_slot_round_trips_a_function_pointer() {
        let slot: CallbackSlot<extern "C" fn(i32) -> i32> = CallbackSlot::empty();
        assert!(slot.load().is_none(), "an untouched slot is empty");

        slot.store(Some(sample));
        let loaded = slot.load().expect("the stored callback should come back");
        assert_eq!(loaded(41), 42, "and it should still be callable");

        slot.store(None);
        assert!(slot.load().is_none(), "None clears the slot");
    }
}
