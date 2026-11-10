//! The helper's Win32 glue, by concern: the protected installation's
//! ACLs, process and pipe identities, the approved console login, the
//! service manager, the service's diagnostics logs and the elevated
//! installer.
mod acl;
mod console;
mod elevate;
mod identity;
#[cfg(feature = "windows-helper")]
mod log;
mod scm;

pub(crate) use crate::oop::proto::SessionIdentity;
pub(crate) use crate::platform::windows::win32::wide;
#[cfg(feature = "windows-helper")]
pub use acl::{Descriptor, check_acl, create_protected, protected_path, replace_protected};
pub use acl::{install_root, protected_file, same_path};
pub use console::{ConsoleState, active_session};
#[cfg(feature = "windows-helper")]
pub use console::{background_session, owner_session};
pub use elevate::{Action, request_installation};
#[cfg(all(test, feature = "windows-helper"))]
pub use identity::image;
#[cfg(feature = "windows-helper")]
pub use identity::{
    LOCAL_SYSTEM_SID, ScopedPrivileges, session, verify_client, verify_management_client,
};
pub use identity::{current_sid, verify_running_service, verify_server};
#[cfg(feature = "windows-helper")]
pub use log::{open_service_log, record_service_event, record_service_failure, rotate_service_log};
pub use scm::service_state;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::Win32::Foundation::HANDLE;

pub struct Handle(pub OwnedHandle);
impl Handle {
    pub unsafe fn own(handle: HANDLE) -> Self {
        Self(unsafe { OwnedHandle::from_raw_handle(handle.0) })
    }
    pub fn raw(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
}
