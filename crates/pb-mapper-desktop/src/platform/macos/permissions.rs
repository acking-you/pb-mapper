//! macOS Screen Recording and Accessibility (event-posting) permission state,
//! updated live in a running process without requiring an app restart.
//!
//! Two macOS quirks would otherwise keep a freshly granted permission looking
//! denied until the user quits and reopens `pb-mapper`:
//! - **Accessibility (`Permission::Input`)**: on macOS 26/27, granting an app
//!   by dragging its icon into System Settings flips `AXIsProcessTrusted()` to
//!   `true` at once and allows `CGEvent` posting immediately, while `tccd`'s
//!   `PostEvent` preflight (`IOHIDCheckAccess` / `CGPreflightPostEventAccess`)
//!   keeps answering `Denied` for the rest of the process's life. Conversely,
//!   if `tccd` already answered `Granted` and the owner switches the entry off
//!   or deletes it, `tccd` drops to `Denied` or `Unknown` before the in-process
//!   `AXIsProcessTrusted` cache catches up. Combining both signals catches
//!   fresh drag-to-authorize grants and live revocations alike.
//! - **Screen Recording (`Permission::ScreenCapture`)**: CoreGraphics'
//!   `CGPreflightScreenCaptureAccess` can keep returning `false` in a process
//!   that launched before the grant, even after `tccd` writes the new consent
//!   and `ScreenCaptureKit` (`replayd`) is ready to stream. Watching Darwin's
//!   `com.apple.tcc.access.changed` notification lets us probe
//!   `SCShareableContent` only after the owner changes TCC settings (never on
//!   cold startup, where probing an unasked app would raise macOS's modal
//!   prompt), so clicking **Later** on System Settings' "Quit & Reopen" banner
//!   still activates sharing immediately.

/// What `tccd` (`IOHIDCheckAccess(PostEvent)`) reports for event posting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostAccess {
    Granted,
    Denied,
    /// Never asked, or the app's entry was removed from the list.
    Unknown,
}

/// One sample of Accessibility's two non-prompting signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawInputAccess {
    /// `AXIsProcessTrusted()`.
    pub trusted: bool,
    /// `IOHIDCheckAccess(kIOHIDRequestTypePostEvent)` or CoreGraphics preflight.
    pub post: PostAccess,
}

/// Pure assessment of whether Accessibility is currently granted, given the
/// carried baseline reading (if any) and the fresh reading.
pub(crate) fn assess_input(previous: Option<RawInputAccess>, now: RawInputAccess) -> bool {
    let revoked =
        now.post != PostAccess::Granted && previous.is_some_and(|p| p.post == PostAccess::Granted);
    if revoked {
        return false;
    }
    matches!(now.post, PostAccess::Granted) || now.trusted
}

/// The baseline reading carried into the next poll so a revocation observed by
/// `tccd` is not masked by a lagging `AXIsProcessTrusted` cache on subsequent
/// polls before `tccd` grants access again.
pub(crate) fn carry_input(previous: RawInputAccess, reading: RawInputAccess) -> RawInputAccess {
    if reading.post != PostAccess::Granted && previous.post == PostAccess::Granted {
        previous
    } else {
        reading
    }
}

/// Pure decision for whether `SCShareableContent` should be probed after a
/// `CGPreflightScreenCaptureAccess() == false` check: only when TCC posted a
/// change notification and either the owner initiated a Screen Recording grant
/// in this session or a previous live probe already succeeded (so revocations
/// are noticed too), while the console is unlocked.
pub(crate) fn should_probe_screen_capture(
    requested_in_session: bool,
    live_granted: bool,
    tcc_changed: bool,
    screen_locked: bool,
) -> bool {
    tcc_changed && !screen_locked && (requested_in_session || live_granted)
}

#[cfg(target_os = "macos")]
mod os {
    use super::{
        PostAccess, RawInputAccess, assess_input, carry_input, should_probe_screen_capture,
    };
    use crate::Permission;
    use objc2_core_graphics::{CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess};
    use parking_lot::Mutex;
    use std::{
        ffi::{CStr, c_char, c_int},
        sync::atomic::{AtomicBool, Ordering},
    };

    const TCC_CHANGED: &CStr = c"com.apple.tcc.access.changed";
    const NOTIFY_OK: u32 = 0;

    /// `IOHIDRequestType::kIOHIDRequestTypePostEvent`.
    const K_IOHID_REQUEST_TYPE_POST_EVENT: u32 = 0;
    /// `IOHIDAccessType` values from `<IOKit/hidsystem/IOHIDLib.h>`.
    const K_IOHID_ACCESS_TYPE_GRANTED: u32 = 0;
    const K_IOHID_ACCESS_TYPE_DENIED: u32 = 1;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOHIDCheckAccess(request_type: u32) -> u32;
    }

    unsafe extern "C" {
        // libnotify, part of libSystem.
        fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
        fn notify_check(token: c_int, check: *mut c_int) -> u32;
        fn notify_cancel(token: c_int) -> u32;
    }

    struct TccWatch {
        token: c_int,
    }

    impl TccWatch {
        fn new() -> Option<Self> {
            let mut token: c_int = 0;
            // SAFETY: NUL-terminated static notification name and valid out pointer.
            let status = unsafe { notify_register_check(TCC_CHANGED.as_ptr(), &mut token) };
            if status != NOTIFY_OK {
                return None;
            }
            let watch = Self { token };
            // Consume the initial registration check flag so subsequent calls
            // report only writes made after registration.
            let _ = watch.changed();
            Some(watch)
        }

        fn changed(&self) -> bool {
            let mut check: c_int = 0;
            // SAFETY: valid token registered by `notify_register_check`.
            let status = unsafe { notify_check(self.token, &mut check) };
            status == NOTIFY_OK && check != 0
        }
    }

    impl Drop for TccWatch {
        fn drop(&mut self) {
            // SAFETY: cancels the token owned by `Self` once.
            let _ = unsafe { notify_cancel(self.token) };
        }
    }

    #[derive(Default)]
    struct ScreenState {
        watch: Option<Option<TccWatch>>,
        live_granted: bool,
    }

    impl ScreenState {
        fn ensure_watch(&mut self) -> bool {
            let slot = self.watch.get_or_insert_with(TccWatch::new);
            slot.as_ref().is_some_and(TccWatch::changed)
        }
    }

    static INPUT_BASELINE: Mutex<Option<RawInputAccess>> = Mutex::new(None);
    static SCREEN_REQUESTED: AtomicBool = AtomicBool::new(false);
    static SCREEN_STATE: Mutex<ScreenState> = Mutex::new(ScreenState {
        watch: None,
        live_granted: false,
    });

    fn sample_input() -> RawInputAccess {
        // SAFETY: plain queries with no arguments and no UI prompt.
        let trusted = unsafe { AXIsProcessTrusted() };
        let hid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_POST_EVENT) };
        let post = match hid {
            K_IOHID_ACCESS_TYPE_GRANTED => PostAccess::Granted,
            _ if CGPreflightPostEventAccess() => PostAccess::Granted,
            K_IOHID_ACCESS_TYPE_DENIED => PostAccess::Denied,
            _ => PostAccess::Unknown,
        };
        RawInputAccess { trusted, post }
    }

    /// Whether macOS currently lets this process post input events, without
    /// prompting. Picks up drag-to-authorize grants immediately without restart.
    pub(crate) fn input_granted() -> bool {
        let reading = sample_input();
        let mut baseline = INPUT_BASELINE.lock();
        let granted = assess_input(*baseline, reading);
        *baseline = Some(match *baseline {
            Some(prev) => carry_input(prev, reading),
            None => reading,
        });
        granted
    }

    /// Whether macOS currently lets this process capture the screen, without
    /// prompting an unasked user. Picks up live `ScreenCaptureKit` grants after
    /// the owner toggles Screen Recording on in System Settings without restart.
    pub(crate) fn screen_capture_granted() -> bool {
        if CGPreflightScreenCaptureAccess() {
            let mut state = SCREEN_STATE.lock();
            let _ = state.ensure_watch();
            state.live_granted = true;
            return true;
        }
        if !super::super::capture::available() {
            return false;
        }
        let requested = SCREEN_REQUESTED.load(Ordering::Acquire);
        let should_probe = {
            let mut state = SCREEN_STATE.lock();
            if !requested && !state.live_granted {
                return false;
            }
            let changed = state.ensure_watch();
            let locked = super::super::desktop::is_locked();
            if !should_probe_screen_capture(requested, state.live_granted, changed, locked) {
                return state.live_granted;
            }
            true
        };
        let probed = should_probe && super::super::capture::probe_access();
        let mut state = SCREEN_STATE.lock();
        state.live_granted = probed;
        probed
    }

    /// Prepares live permission tracking when the owner clicks Grant in the UI
    /// and answers whether the permission is already in place. Never triggers
    /// macOS's redundant modal prompt, since the UI opens the exact System
    /// Settings pane alongside the drag-to-authorize card.
    pub(crate) fn request(permission: Permission) -> bool {
        match permission {
            Permission::ScreenCapture => {
                {
                    let mut state = SCREEN_STATE.lock();
                    let _ = state.ensure_watch();
                }
                SCREEN_REQUESTED.store(true, Ordering::Release);
                if CGPreflightScreenCaptureAccess() {
                    SCREEN_STATE.lock().live_granted = true;
                    return true;
                }
                // If the owner already toggled Screen Recording on before
                // clicking again, pick it up immediately when unlocked.
                if super::super::capture::available() && !super::super::desktop::is_locked() {
                    let probed = super::super::capture::probe_access();
                    if probed {
                        SCREEN_STATE.lock().live_granted = true;
                        return true;
                    }
                }
                false
            }
            Permission::Input => input_granted(),
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use os::{input_granted, request, screen_capture_granted};

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(trusted: bool, post: PostAccess) -> RawInputAccess {
        RawInputAccess { trusted, post }
    }

    #[test]
    fn drag_to_authorize_grants_input_even_while_post_event_preflight_lags() {
        let before = raw(false, PostAccess::Denied);
        let after_drag = raw(true, PostAccess::Denied);
        assert!(assess_input(Some(before), after_drag));
        assert_eq!(carry_input(before, after_drag), after_drag);

        // Switching the entry off again drops `trusted` back to `false`.
        let switched_off = raw(false, PostAccess::Denied);
        assert!(!assess_input(Some(after_drag), switched_off));
    }

    #[test]
    fn tccd_revocation_wins_over_stale_ax_trust_cache() {
        let granted = raw(true, PostAccess::Granted);
        let removed = raw(true, PostAccess::Unknown);
        assert!(!assess_input(Some(granted), removed));
        let carried = carry_input(granted, removed);
        assert_eq!(carried, granted);
        // Subsequent polls while `AXIsProcessTrusted` is still stale stay false.
        assert!(!assess_input(Some(carried), removed));

        // Re-granting in tccd restores access.
        let restored = raw(true, PostAccess::Granted);
        assert!(assess_input(Some(carried), restored));
    }

    #[test]
    fn screen_capture_live_probe_runs_only_on_unlocked_tcc_changes_after_request() {
        // Cold startup never probes `SCShareableContent`, avoiding unasked popups.
        assert!(!should_probe_screen_capture(false, false, true, false));
        // No TCC change since last check -> reuse cached answer without IPC.
        assert!(!should_probe_screen_capture(true, false, false, false));
        // Locked screen hides displays from `SCShareableContent`, so wait.
        assert!(!should_probe_screen_capture(true, true, true, true));
        // Requested by user + TCC changed + unlocked -> probe live!
        assert!(should_probe_screen_capture(true, false, true, false));
        // Previously live-granted + TCC changed + unlocked -> re-verify for revocation!
        assert!(should_probe_screen_capture(false, true, true, false));
    }
}
