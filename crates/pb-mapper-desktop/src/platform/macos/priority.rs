//! A thread's priority on macOS: the user-interactive QoS class, which the
//! ScreenCaptureKit queue thread C serves already runs at. A thread is raised
//! only from a public class below it: default (a new thread's), utility,
//! background or user-initiated. One already user-interactive, one opted out
//! of QoS (unspecified: `pthread_setschedparam` does that for good, and the
//! class can then not be set) and a class this `libc` does not name are left
//! alone. A raised thread gets back its exact class and relative priority.

use libc::{c_int, qos_class_t};

/// The class and relative priority this thread had before it was raised.
pub(crate) struct Before {
    class: qos_class_t,
    relative: c_int,
}

/// Every class `libc` names. A class the kernel reports becomes a
/// `qos_class_t` only through this list, so one it does not name
/// (`QOS_CLASS_MAINTENANCE`) never does.
const NAMED: [qos_class_t; 6] = [
    qos_class_t::QOS_CLASS_USER_INTERACTIVE,
    qos_class_t::QOS_CLASS_USER_INITIATED,
    qos_class_t::QOS_CLASS_DEFAULT,
    qos_class_t::QOS_CLASS_UTILITY,
    qos_class_t::QOS_CLASS_BACKGROUND,
    qos_class_t::QOS_CLASS_UNSPECIFIED,
];

fn named(class: u32) -> Option<qos_class_t> {
    NAMED.into_iter().find(|named| *named as u32 == class)
}

/// This thread's class, as the number the kernel reports, and its relative
/// priority.
fn read() -> Option<(u32, c_int)> {
    let mut class: u32 = 0;
    let mut relative: c_int = 0;
    // SAFETY: `pthread_self` is the calling thread, alive for the call. Both
    // out-pointers are live locals. `qos_class_t` is `repr(u32)`, so the
    // class may be written into a `u32`, which is only ever read as one.
    let read = unsafe {
        libc::pthread_get_qos_class_np(
            libc::pthread_self(),
            std::ptr::from_mut(&mut class).cast::<qos_class_t>(),
            &mut relative,
        )
    };
    (read == 0).then_some((class, relative))
}

/// Raise this thread to user-interactive; its earlier class, if this did.
pub(crate) fn raise() -> Option<Before> {
    let (class, relative) = read()?;
    let class = named(class)?;
    match class {
        qos_class_t::QOS_CLASS_DEFAULT
        | qos_class_t::QOS_CLASS_UTILITY
        | qos_class_t::QOS_CLASS_BACKGROUND
        | qos_class_t::QOS_CLASS_USER_INITIATED => {}
        qos_class_t::QOS_CLASS_USER_INTERACTIVE | qos_class_t::QOS_CLASS_UNSPECIFIED => {
            return None;
        }
    }
    // SAFETY: sets the calling thread's own class, a named one, at relative
    // priority 0, which is always in range.
    let set =
        unsafe { libc::pthread_set_qos_class_self_np(qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0) };
    (set == 0).then_some(Before { class, relative })
}

/// Put this thread's class back. A failure leaves it raised.
pub(crate) fn restore(before: Before) {
    // SAFETY: sets the calling thread's own class back to the named class
    // and relative priority the kernel reported for it.
    unsafe { libc::pthread_set_qos_class_self_np(before.class, before.relative) };
}

/// The levels the tests set and read.
#[cfg(test)]
pub(crate) mod testing {
    use libc::qos_class_t;

    pub(crate) type Level = u32;
    pub(crate) const RAISED: Level = qos_class_t::QOS_CLASS_USER_INTERACTIVE as u32;
    /// Below the raised level.
    pub(crate) const LOWER: Level = qos_class_t::QOS_CLASS_UTILITY as u32;
    /// No public class is above user-interactive: the raised one itself.
    pub(crate) const HIGHER: Level = RAISED;

    pub(crate) fn current() -> Level {
        super::read().map_or(u32::MAX, |(class, _)| class)
    }

    pub(crate) fn set(level: Level) -> bool {
        let Some(class) = super::named(level) else {
            return false;
        };
        // SAFETY: sets the calling thread's own class, a named one, at
        // relative priority 0.
        unsafe { libc::pthread_set_qos_class_self_np(class, 0) == 0 }
    }
}
