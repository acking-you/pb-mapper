//! Timely capture on macOS: while a session captures, an activity keeps App
//! Nap off this process, so the capture queue and the network are not
//! suspended or throttled when the app is hidden or idle, and keeps the
//! display on, which a viewer is watching. It does not stop the kernel from
//! coalescing the process's timers (measured on a CI runner: a 2 ms wait
//! still ended 8 ms late); thread C's waits avoid that themselves
//! (`waiter.rs`). Counted across the process's sessions; the last guard ends
//! the activity. Idle system sleep beyond that is the owner's settings'.

use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

/// An activity token.
struct Activity(Retained<ProtocolObject<dyn NSObjectProtocol>>);
// SAFETY: an activity token is an opaque, immutable object that
// `NSProcessInfo`, itself thread-safe, accepts back from any thread.
unsafe impl Send for Activity {}

struct Held {
    guards: usize,
    activity: Option<Activity>,
}
static HELD: parking_lot::Mutex<Held> = parking_lot::const_mutex(Held {
    guards: 0,
    activity: None,
});

pub(crate) fn acquire() {
    let mut held = HELD.lock();
    held.guards += 1;
    if held.guards > 1 {
        return;
    }
    let options = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep
        | NSActivityOptions::IdleDisplaySleepDisabled
        | NSActivityOptions::LatencyCritical;
    held.activity = Some(objc2::rc::autoreleasepool(|_| {
        let reason = NSString::from_str("A remote desktop viewer is watching this screen");
        Activity(NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &reason))
    }));
}

pub(crate) fn release() {
    let mut held = HELD.lock();
    held.guards = held.guards.saturating_sub(1);
    if held.guards > 0 {
        return;
    }
    if let Some(activity) = held.activity.take() {
        // SAFETY: the token is the one `beginActivity` returned, ended once.
        unsafe { NSProcessInfo::processInfo().endActivity(&activity.0) };
    }
}
