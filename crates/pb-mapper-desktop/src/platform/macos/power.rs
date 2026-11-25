//! Lighting a sleeping display for a viewer (`wake.rs` says when). A
//! declaration of local user activity (`IOPMAssertionDeclareUserActivity`,
//! as `caffeinate -u` makes) turns the displays on and holds off their
//! sleep for the owner's display-sleep time, as a touch at the Mac would;
//! the session's activity (`timer.rs`) then keeps them on while it
//! captures. Every caller has passed the session check first, so a locked
//! Mac, or one whose screen another login has, stays dark: its picture is
//! the secure desktop, which nothing here can show.
//!
//! Where a display is woken: when a session's parts open, waiting a moment
//! for the shared display to draw; when its capture opens, a recovery's
//! included; at the first capture after a resume; when an open capture
//! delivered no picture or ScreenCaptureKit did not list the display;
//! before each still; and when it turned off under a running stream. One
//! that turned off under a running capture, its stream or its stills, is
//! lit once it has been found dark for `wake::SETTLE`. The power manager's
//! assertion id is kept and passed back with each declaration, as its
//! header asks, and released with the session's activity once the display
//! had the time to light.

use super::{
    desktop,
    wake::{WakeLimiter, wake_unlisted, wakeable},
};
use crate::{BackendError, Display, FailureKind};
use objc2_core_foundation::{CFBoolean, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayIsAsleep, CGDisplayIsBuiltin, CGGetOnlineDisplayList,
};
use std::{
    ffi::{c_char, c_void},
    ptr::NonNull,
    time::{Duration, Instant},
};

/// `IOPMAssertionID`.
type AssertionId = u32;
/// `kIOPMNullAssertionID`: none held.
const NO_ASSERTION: AssertionId = 0;
/// `kIOPMUserActiveLocal`: someone at the Mac, which lights its displays.
const USER_ACTIVE_LOCAL: u32 = 0;
/// `kIOReturnSuccess`.
const SUCCESS: i32 = 0;
/// `kIOMainPortDefault`, which is `MACH_PORT_NULL`.
const MAIN_PORT_DEFAULT: u32 = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    /// `userType` is the `IOPMUserActiveType` enum, an `int`; the id is
    /// read, and written with the one to pass next time.
    fn IOPMAssertionDeclareUserActivity(
        name: &CFString,
        user_type: u32,
        id: *mut AssertionId,
    ) -> i32;
    fn IOPMAssertionRelease(id: AssertionId) -> i32;
    /// A matching dictionary, retained, or null.
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    /// Consumes `matching`; the service is released with `IOObjectRelease`.
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
    /// Follows the Create rule.
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: &CFString,
        allocator: *const c_void,
        options: u32,
    ) -> Option<NonNull<CFType>>;
    fn IOObjectRelease(object: u32) -> i32;
}

/// How long a session's start waits for its display to draw once woken:
/// under the time a new session has to describe itself, so that a slow
/// display costs it none of what it reports. Capture retries after that.
const WAKE_WAIT: Duration = Duration::from_secs(2);
/// How often that wait looks.
const WAKE_POLL: Duration = Duration::from_millis(100);
const _: () = assert!(
    WAKE_WAIT.as_millis() + 500 <= crate::host::BACKEND_ANSWER_WAIT.as_millis(),
    "a woken display leaves the backend time to describe itself"
);

/// The most displays enumerated.
const MAX_DISPLAYS: usize = 32;

/// The last declaration and the id it returned.
struct Woken {
    limiter: WakeLimiter,
    id: AssertionId,
}
static WOKEN: parking_lot::Mutex<Woken> = parking_lot::const_mutex(Woken {
    limiter: WakeLimiter::new(),
    id: NO_ASSERTION,
});

/// Capture's failure while the display it woke lights.
pub(crate) fn waking() -> std::io::Error {
    BackendError::new(
        FailureKind::CaptureFailed,
        "the display is asleep; waking it",
    )
    .into()
}

/// Whether display `id` sleeps and can be lit: not the built-in panel
/// behind a closed lid. One Core Graphics read while it is awake.
pub(crate) fn sleeping(id: CGDirectDisplayID) -> bool {
    wakeable(CGDisplayIsAsleep(id), || CGDisplayIsBuiltin(id), lid_closed)
}

/// Light whichever of `ids` sleeps and can be lit ([`light`]). Whether one
/// of them sleeps and is being lit.
pub(crate) fn wake(ids: impl IntoIterator<Item = CGDirectDisplayID>) -> bool {
    let asleep = ids.into_iter().any(sleeping);
    if asleep {
        light();
    }
    asleep
}

/// Light sleeping displays, or declare activity when no display is awake at
/// all (an external monitor in deep standby that dropped HPD) and no lid is
/// closed. Every caller has verified `desktop::available()` first.
pub(crate) fn wake_any() -> bool {
    let mut any_awake = false;
    let mut should_light = false;
    for id in online() {
        if !CGDisplayIsAsleep(id) {
            any_awake = true;
        } else if wakeable(true, || CGDisplayIsBuiltin(id), lid_closed) {
            should_light = true;
            break;
        }
    }
    if !should_light {
        should_light = wake_unlisted(any_awake, lid_closed);
    }
    if should_light {
        light();
    }
    should_light
}

/// At a session's start, before its display is bound: when this login owns
/// the console, light every display that sleeps (including on the lock screen
/// or an external monitor that dropped HPD in deep standby), and wait up to
/// `WAKE_WAIT` for `display` to draw, so that a fast panel starts with a
/// picture while a slower monitor continues waking under `Engine`'s retry.
pub(crate) fn wake_for_session(display: &Display) {
    if desktop::on_console().is_err() || drawing(display) || !wake_any() {
        return;
    }
    let until = Instant::now() + WAKE_WAIT;
    while !drawing(display) {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(WAKE_POLL.min(left));
    }
}

/// When the session's activity ends: give the power manager back the
/// declaration, unless it was made within `WAKE_EVERY`, while a display
/// it woke may still be lighting; the power manager ends that one itself,
/// and the next declaration passes it back.
pub(crate) fn release() {
    let mut woken = WOKEN.lock();
    if woken.id == NO_ASSERTION || !woken.limiter.settled(Instant::now()) {
        return;
    }
    let id = std::mem::replace(&mut woken.id, NO_ASSERTION);
    // SAFETY: an id a declaration returned, released once; one the power
    // manager already timed out is refused harmlessly.
    let _ = unsafe { IOPMAssertionRelease(id) };
}

/// Declare the local user active, which lights every display, unless that
/// was done within `WAKE_EVERY`.
pub(crate) fn light() {
    let mut woken = WOKEN.lock();
    if !woken.limiter.admit(Instant::now()) {
        return;
    }
    declare_activity(&mut woken.id);
}

/// Declare the local user active unconditionally (for an explicit remote
/// lock-screen wake and password unlock), recording the wake timestamp.
pub(crate) fn force_light() {
    let mut woken = WOKEN.lock();
    let _ = woken.limiter.admit(Instant::now());
    declare_activity(&mut woken.id);
}

fn declare_activity(id: &mut AssertionId) {
    let name = CFString::from_static_str("pb-mapper: a remote desktop viewer is watching");
    // SAFETY: a name, the local user and the id the last declaration gave
    // (or none), which the call replaces with the one to pass next.
    let status = unsafe { IOPMAssertionDeclareUserActivity(&name, USER_ACTIVE_LOCAL, id) };
    if status == SUCCESS {
        tracing::debug!(target: "pb_mapper_desktop::session", event = "desktop_display_wake");
    } else {
        tracing::warn!(
            target: "pb_mapper_desktop::session",
            event = "desktop_display_wake_failed",
            status = format_args!("{status:#x}"),
        );
    }
}

/// The displays connected: drawing, mirrored or asleep. A sleeping one is
/// not among the active displays.
pub(crate) fn online() -> Vec<CGDirectDisplayID> {
    let mut ids = [0; MAX_DISPLAYS];
    let mut count = 0;
    // SAFETY: both pointers are to local storage of the size given.
    let failed =
        unsafe { CGGetOnlineDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
    if failed.0 != 0 {
        return Vec::new();
    }
    ids.get(..count as usize)
        .map_or_else(Vec::new, <[u32]>::to_vec)
}

/// Whether `display` is among the active displays and awake: by its UUID,
/// or by its id when it has none, as the desktop binds it.
pub(crate) fn drawing(display: &Display) -> bool {
    desktop::active().into_iter().any(|id| {
        !CGDisplayIsAsleep(id)
            && if display.name.is_empty() {
                id == display.id
            } else {
                desktop::uuid(id).as_deref() == Some(display.name.as_str())
            }
    })
}

/// Whether this portable's lid is closed: the power manager's
/// `AppleClamshellState`. False on a Mac without a lid, or unread.
pub(crate) fn lid_closed() -> bool {
    // SAFETY: a C string; the result is a retained dictionary, or null.
    let matching = unsafe { IOServiceMatching(c"IOPMrootDomain".as_ptr()) };
    if matching.is_null() {
        return false;
    }
    // SAFETY: the default main port and a matching dictionary, which the
    // call consumes.
    let root = unsafe { IOServiceGetMatchingService(MAIN_PORT_DEFAULT, matching) };
    if root == 0 {
        return false;
    }
    let key = CFString::from_static_str("AppleClamshellState");
    // SAFETY: the registry entry just found, a key, the default allocator
    // and no options.
    let value = unsafe { IORegistryEntryCreateCFProperty(root, &key, std::ptr::null(), 0) };
    // SAFETY: the service `IOServiceGetMatchingService` returned, released
    // once.
    let _ = unsafe { IOObjectRelease(root) };
    // SAFETY: a Create-rule result, released when this drops.
    value
        .map(|value| unsafe { CFRetained::from_raw(value) })
        .is_some_and(|value| {
            value
                .downcast_ref::<CFBoolean>()
                .is_some_and(CFBoolean::as_bool)
        })
}
