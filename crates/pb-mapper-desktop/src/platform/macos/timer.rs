//! Timely capture on macOS: while a session captures, an activity keeps App
//! Nap and timer coalescing off this process, so thread C, the capture queue
//! and the network keep their pace when the app is hidden or idle, and keeps
//! the display on, which a viewer is watching. Counted across the process's
//! sessions; the last guard ends the activity. Idle system sleep beyond that
//! is the owner's settings'.

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
    let reason = NSString::from_str("A remote desktop viewer is watching this screen");
    held.activity = Some(Activity(
        NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &reason),
    ));
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
