//! Privileged desktop access: the secure desktop (UAC, the lock screen),
//! and hosting that outlives the app. On Windows it is the installed,
//! explicitly authorized helper service, which never elevates on its own;
//! elsewhere it is not available yet, as [`support`] says on every target.
//! A host uses it through [`Access::Privileged`](crate::Access).
use serde::Serialize;

/// Whether privileged access is available here now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PrivilegedSupport {
    pub available: bool,
    /// Why not, when it is not.
    pub reason: Option<String>,
}

/// Whether privileged access is available here now, and if not why.
pub fn support() -> PrivilegedSupport {
    crate::platform::privileged_support()
}

/// The helper binary's entry point.
#[cfg(all(windows, feature = "windows-helper"))]
#[doc(hidden)]
pub use crate::platform::windows::helper::run;
#[cfg(windows)]
#[cfg_attr(docsrs, doc(cfg(windows)))]
pub use crate::platform::windows::helper::{
    Action, HELPER_PROTOCOL, Status, prepare_application_restart, request_installation, status,
};

/// Hosting owned by the helper service, which keeps sharing while the app is
/// closed and nobody is signed in to it.
#[cfg(windows)]
#[cfg_attr(docsrs, doc(cfg(windows)))]
pub mod background;
