//! Timely capture on macOS, while a session captures, counted across the
//! process's sessions; the last guard gives everything back.
//!
//! - The task's latency QoS is overridden to tier 0. A process that launchd
//!   did not start as the frontmost app (a LaunchAgent or daemon, a child of
//!   a service such as a CI runner) can run at latency tier 3, where the
//!   kernel lets each of its timers fire up to four times its interval late,
//!   at most 75 ms: a 16.7 ms tick then ends about 67 ms late and thread C
//!   paces at 15 fps instead of 60. Neither the activity below nor a thread's
//!   QoS class lifts that tier; the task's override does, for thread C,
//!   tokio's timers and the network's alike. The override in place before is
//!   put back.
//! - An activity keeps App Nap off the process and the display on, which a
//!   viewer is watching. Idle system sleep beyond that is the owner's
//!   settings'.
//!
//! Each call may fail harmlessly: timers then stay as coarse as they were.

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
    /// The override to put back, once ours took effect.
    latency: Option<Qos>,
}
static HELD: parking_lot::Mutex<Held> = parking_lot::const_mutex(Held {
    guards: 0,
    activity: None,
    latency: None,
});

/// `struct task_qos_policy` of `<mach/task_policy.h>`: a latency and a
/// throughput tier, 0 where unspecified.
#[repr(C)]
#[derive(Clone, Copy)]
struct Qos {
    latency: i32,
    throughput: i32,
}

/// `TASK_OVERRIDE_QOS_POLICY`.
const OVERRIDE: u32 = 9;
/// `LATENCY_QOS_TIER_0`: timers coalesced by at most an eighth of their
/// interval, and 1 ms.
const LATENCY_TIER_0: i32 = (0xFF << 16) | 1;
/// `TASK_QOS_POLICY_COUNT`, in `integer_t`s.
const COUNT: u32 = (size_of::<Qos>() / size_of::<i32>()) as u32;

unsafe extern "C" {
    /// What `mach_task_self()` reads: this task's port.
    static mach_task_self_: u32;
    fn task_policy_set(task: u32, flavor: u32, info: *mut i32, count: u32) -> i32;
    fn task_policy_get(
        task: u32,
        flavor: u32,
        info: *mut i32,
        count: *mut u32,
        get_default: *mut i32,
    ) -> i32;
}

/// This task's QoS override; unspecified where it cannot be read.
fn current() -> Qos {
    let mut qos = Qos {
        latency: 0,
        throughput: 0,
    };
    let (mut count, mut get_default) = (COUNT, 0);
    // SAFETY: `qos` holds `count` integers, and the port is this task's.
    let read = unsafe {
        task_policy_get(
            mach_task_self_,
            OVERRIDE,
            (&raw mut qos).cast(),
            &mut count,
            &mut get_default,
        )
    };
    if read == 0 {
        qos
    } else {
        Qos {
            latency: 0,
            throughput: 0,
        }
    }
}

fn set(mut qos: Qos) -> bool {
    // SAFETY: `qos` holds `COUNT` integers, and the port is this task's.
    unsafe { task_policy_set(mach_task_self_, OVERRIDE, (&raw mut qos).cast(), COUNT) == 0 }
}

pub(crate) fn acquire() {
    let mut held = HELD.lock();
    held.guards += 1;
    if held.guards > 1 {
        return;
    }
    let before = current();
    let tightened = set(Qos {
        latency: LATENCY_TIER_0,
        ..before
    });
    if !tightened {
        tracing::debug!(
            target: "pb_mapper_desktop::session",
            event = "macos_latency_qos_not_set",
        );
    }
    held.latency = tightened.then_some(before);
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
    if let Some(before) = held.latency.take() {
        set(before);
    }
}
