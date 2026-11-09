//! Let the approved UI verify this service's image without elevating the UI.
use super::{Result, error, os};
use windows::{
    Win32::{Foundation::*, Security::Authorization::*, Security::*, System::Threading::*},
    core::{PCWSTR, PWSTR},
};

struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

/// Called by the service itself before publishing Running/listening. Preserve
/// its existing DACL and add only image-query access for the configured owner.
/// No process-control, memory, token, synchronization or handle-duplication
/// permission is granted. IPC still independently verifies the approved app.
pub(super) fn allow_identity_query(owner: &str) -> Result<()> {
    let process = unsafe { GetCurrentProcess() };
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let mut old_acl = std::ptr::null_mut();
    unsafe {
        GetSecurityInfo(
            process,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut old_acl),
            None,
            Some(&mut descriptor),
        )
    }
    .ok()
    .map_err(error)?;
    let _descriptor = LocalAllocation(descriptor.0);
    if old_acl.is_null() {
        return Err(error("helper process has no restrictive DACL"));
    }
    let mut sid = PSID::default();
    unsafe { ConvertStringSidToSidW(PCWSTR(os::wide(owner).as_ptr()), &mut sid) }.map_err(error)?;
    let _sid = LocalAllocation(sid.0);
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: PROCESS_QUERY_LIMITED_INFORMATION.0,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: PWSTR(sid.0.cast()),
            ..Default::default()
        },
    };
    let mut new_acl = std::ptr::null_mut();
    unsafe { SetEntriesInAclW(Some(&[entry]), Some(old_acl), &mut new_acl) }
        .ok()
        .map_err(error)?;
    let _acl = LocalAllocation(new_acl.cast());
    unsafe {
        SetSecurityInfo(
            process,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_acl),
            None,
        )
    }
    .ok()
    .map_err(error)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, Read, Write},
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };

    #[test]
    fn identity_query_does_not_grant_process_control() {
        const CHILD: &str = "PB_MAPPER_QUERY_ACL_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            // Isolate the process DACL from the parallel test runner. The
            // child retains WRITE_DAC so the real startup function can merge
            // its query ACE; the parent's original child handle permits cleanup.
            let owner = os::current_sid().unwrap();
            let descriptor =
                os::Descriptor::new(&format!("D:P(A;;GA;;;SY)(A;;0x60000;;;{owner})")).unwrap();
            let mut present = windows::core::BOOL(0);
            let mut defaulted = windows::core::BOOL(0);
            let mut acl = std::ptr::null_mut();
            unsafe {
                GetSecurityDescriptorDacl(
                    PSECURITY_DESCRIPTOR((*descriptor.attributes()).lpSecurityDescriptor),
                    &mut present,
                    &mut acl,
                    &mut defaulted,
                )
                .unwrap();
                SetSecurityInfo(
                    GetCurrentProcess(),
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(acl),
                    None,
                )
                .ok()
                .unwrap();
            }
            let mut command = [0];
            println!("restricted");
            std::io::stdout().flush().unwrap();
            std::io::stdin().read_exact(&mut command).unwrap();
            allow_identity_query(&owner).unwrap();
            println!("query-granted");
            std::io::stdout().flush().unwrap();
            std::io::stdin().read_exact(&mut command).unwrap();
            return;
        }
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Child(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "platform::windows::helper::process_acl::tests::identity_query_does_not_grant_process_control",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .creation_flags(CREATE_NO_WINDOW.0)
                .spawn()
                .unwrap(),
        );
        let output = std::io::BufReader::new(child.0.stdout.take().unwrap());
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in output.lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let wait_for = |marker: &str| {
            loop {
                let line = receive
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("helper permission test child did not respond");
                if line.trim() == marker {
                    break;
                }
            }
        };
        // A runner's token may hold SeDebugPrivilege enabled, as GitHub's
        // elevated Windows runners do, and that opens any process past its
        // DACL. Probe as a copy of this process's token with every privilege
        // disabled, on this thread and only around each open, so the DACL
        // alone decides and the parallel tests keep their own token.
        let unprivileged = {
            let mut original = HANDLE::default();
            unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_QUERY | TOKEN_DUPLICATE,
                    &mut original,
                )
            }
            .unwrap();
            let original = unsafe { os::Handle::own(original) };
            let mut duplicate = HANDLE::default();
            unsafe {
                DuplicateTokenEx(
                    original.raw(),
                    TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES | TOKEN_IMPERSONATE,
                    None,
                    SecurityImpersonation,
                    TokenImpersonation,
                    &mut duplicate,
                )
            }
            .unwrap();
            let duplicate = unsafe { os::Handle::own(duplicate) };
            unsafe { AdjustTokenPrivileges(duplicate.raw(), true, None, 0, None, None) }.unwrap();
            duplicate
        };
        let pid = child.0.id();
        let open = |access| {
            unsafe { SetThreadToken(None, Some(unprivileged.raw())) }.unwrap();
            let result = unsafe { OpenProcess(access, false, pid) };
            unsafe { RevertToSelf() }.unwrap();
            result
        };
        let denied = |access: PROCESS_ACCESS_RIGHTS| match open(access) {
            Ok(handle) => {
                drop(unsafe { os::Handle::own(handle) });
                panic!("unexpected process access {:#x}", access.0);
            }
            Err(failure) => assert_eq!(
                failure.code(),
                windows::core::HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
            ),
        };
        wait_for("restricted");
        denied(PROCESS_QUERY_LIMITED_INFORMATION);
        child.0.stdin.as_mut().unwrap().write_all(b"q").unwrap();
        wait_for("query-granted");
        let handle = unsafe { os::Handle::own(open(PROCESS_QUERY_LIMITED_INFORMATION).unwrap()) };
        assert!(os::same_path(
            &os::image(handle.raw()).unwrap(),
            &std::env::current_exe().unwrap()
        ));
        for access in [
            PROCESS_QUERY_INFORMATION,
            PROCESS_VM_READ,
            PROCESS_VM_WRITE,
            PROCESS_CREATE_THREAD,
            PROCESS_DUP_HANDLE,
            PROCESS_TERMINATE,
            PROCESS_SYNCHRONIZE,
        ] {
            denied(access);
        }
        child.0.stdin.as_mut().unwrap().write_all(b"x").unwrap();
        assert!(child.0.wait().unwrap().success());
    }
}
