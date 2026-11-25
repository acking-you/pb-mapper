//! The macOS host: ScreenCaptureKit capture fused with a VideoToolbox
//! encoder, `CGEvent` injection (physical keys too), the pointer and its
//! shape, Core Graphics display
//! modes changed for this app only, ScreenCaptureKit stills; Screen
//! Recording and Accessibility (event posting) as the permissions the owner
//! grants; App Nap held off while a session captures, a sleeping display
//! lit for a viewer, and thread C's waits on timers the kernel does not
//! coalesce; a Keychain key for the remembered grant.
//!
//! ScreenCaptureKit arrived in macOS 12.3 while the app runs from 10.15: the
//! FFI library links it weakly, and below 12.3 `support` answers
//! `Platform` and nothing here touches it. Symbols of later systems are
//! looked up at run time ([`cf::symbol`]).
//!
//! The pure parts (points and pixels, key codes, click counts, mode lists,
//! the pointer the injector and the cursor share, how long a desktop check
//! is reused, when a sleeping display is woken, how often ScreenCaptureKit
//! may deliver a picture, the size a session codes) build and are tested on
//! every platform.

// Pure: built and tested everywhere, used by the OS parts on macOS alone.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod cadence;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod clicks;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod frame;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod geometry;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod keys;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod mode_list;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod permissions;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod pixels;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod pointer;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod reuse;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod wake;

#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
pub(crate) mod cf;
#[cfg(target_os = "macos")]
mod cursor;
#[cfg(target_os = "macos")]
mod desktop;
#[cfg(target_os = "macos")]
mod encoder;
#[cfg(target_os = "macos")]
mod input;
#[cfg(target_os = "macos")]
pub(crate) mod keychain;
#[cfg(target_os = "macos")]
mod layout;
#[cfg(target_os = "macos")]
mod modes;
#[cfg(target_os = "macos")]
pub(crate) mod power;
#[cfg(target_os = "macos")]
mod still;
#[cfg(test)]
mod tests;
#[cfg(target_os = "macos")]
pub(crate) mod timer;
#[cfg(target_os = "macos")]
mod unlock;
#[cfg(target_os = "macos")]
mod video;
#[cfg(test)]
mod vt_measure;
#[cfg(target_os = "macos")]
pub(crate) mod waiter;

#[cfg(target_os = "macos")]
pub(crate) use platform::Macos;

#[cfg(target_os = "macos")]
mod platform {
    use super::{capture, cursor, desktop, input, modes, permissions, power, still, video};
    use crate::{
        BackendError, Display, DisplaySelection, HostOs, Permission, Result,
        platform::{HostPermissions, HostSupport, Parts, Platform, UnsupportedReason},
    };
    use std::{
        future::Future,
        path::{Path, PathBuf},
    };

    pub(crate) enum Macos {}

    /// The owner granted Screen Recording when the host started: what every
    /// session's capture rests on. Withdrawn later, capture fails and says
    /// so.
    pub(crate) struct ScreenAccess(());

    /// Screen Recording missing, as a failure.
    pub(crate) fn screen_capture_missing() -> BackendError {
        BackendError::permission_missing(
            Permission::ScreenCapture,
            "macOS has not granted pb-mapper Screen Recording: allow it in System Settings › Privacy & Security › Screen Recording",
        )
    }

    impl Platform for Macos {
        const OS: HostOs = HostOs::Macos;
        type Grant = ScreenAccess;
        type Desktop = desktop::MacDesktop;
        type Video = video::MacVideo;
        type Input = input::MacInput;
        type Cursor = cursor::MacCursor;
        type Modes = modes::MacModes;
        type Still = still::SckStill;

        /// ScreenCaptureKit (macOS 12.3) and a display, then Screen
        /// Recording; posting input only gates control, so it never stops a
        /// host.
        fn support() -> HostSupport {
            let permissions = HostPermissions {
                screen_capture: permissions::screen_capture_granted(),
                input: permissions::input_granted(),
            };
            let reason = if !capture::available() {
                Some(UnsupportedReason::Platform)
            } else if !desktop::any_display() {
                Some(UnsupportedReason::NoDisplay)
            } else if !permissions.screen_capture {
                Some(UnsupportedReason::PermissionMissing)
            } else {
                None
            };
            HostSupport {
                host: reason.is_none(),
                viewer: true,
                secure_desktop: false,
                os: Some(HostOs::Macos),
                reason,
                permissions: Some(permissions),
            }
        }
        /// Prepares live permission tracking and checks whether the permission
        /// is already granted; System Settings and the drag-to-authorize card
        /// handle interactive authorization without a redundant modal alert.
        fn request(permission: Permission) -> bool {
            permissions::request(permission)
        }
        fn displays() -> Result<Vec<Display>> {
            desktop::displays()
        }
        /// Ready at once: the owner grants Screen Recording in System
        /// Settings, never in a dialog a host could wait on.
        fn acquire(
            selection: &DisplaySelection,
        ) -> impl Future<Output = Result<(ScreenAccess, Display)>> + Send {
            std::future::ready(if permissions::screen_capture_granted() {
                desktop::resolve(selection).map(|display| (ScreenAccess(()), display))
            } else {
                Err(screen_capture_missing().into())
            })
        }
        /// A display that sleeps is lit first, when this login owns the
        /// screen, unlocked: asleep, it gives capture nothing and may not
        /// be listed.
        fn open(_: &ScreenAccess, display: &Display) -> Result<Parts<Self>> {
            power::wake_for_session(display);
            let desktop = desktop::MacDesktop::open(display)?;
            let geometry = desktop.shared();
            let pointer = super::pointer::Shared::default();
            Ok(Parts {
                input: input::MacInput::new(geometry.clone(), pointer.clone())?,
                cursor: cursor::MacCursor::new(geometry, pointer),
                modes: modes::MacModes,
                still: still::SckStill::default(),
                desktop,
            })
        }
        /// `~/Library/Application Support/pb-mapper/desktop`, inside the
        /// app's container when it is sandboxed.
        fn default_state_dir() -> Option<PathBuf> {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("pb-mapper")
                    .join("desktop")
            })
        }
        fn recover(state_dir: &Path) -> Result<usize> {
            crate::modes::recover(
                &mut modes::MacModes,
                &crate::modes::Journal::in_dir(Some(state_dir.to_path_buf())),
            )
        }
    }
}
