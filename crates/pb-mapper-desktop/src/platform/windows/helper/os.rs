use super::*;
pub(crate) use crate::oop::proto::SessionIdentity;
use std::{
    ffi::c_void,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
};
use windows::{
    Win32::{
        Foundation::*,
        Security::Authorization::*,
        Security::*,
        Storage::FileSystem::*,
        System::{Com::CoTaskMemFree, Pipes::*, RemoteDesktop::*, Services::*, Threading::*},
        UI::{Shell::*, WindowsAndMessaging::SW_HIDE},
    },
    core::{PCWSTR, PWSTR},
};

pub fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
pub struct Handle(pub OwnedHandle);
impl Handle {
    pub unsafe fn own(handle: HANDLE) -> Self {
        Self(unsafe { OwnedHandle::from_raw_handle(handle.0) })
    }
    pub fn raw(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
}

#[cfg(feature = "windows-helper")]
pub struct Descriptor {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}
#[cfg(feature = "windows-helper")]
impl Descriptor {
    pub fn new(sddl: &str) -> Result<Self> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide(sddl).as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(error)?;
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            },
        })
    }
    pub fn attributes(&self) -> *mut SECURITY_ATTRIBUTES {
        (&self.attributes as *const SECURITY_ATTRIBUTES).cast_mut()
    }
}
#[cfg(feature = "windows-helper")]
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.descriptor.0)));
        }
    }
}

pub fn install_root() -> Result<PathBuf> {
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, None) }
        .map_err(error)?;
    let text = unsafe { path.to_string() }.map_err(error);
    unsafe {
        CoTaskMemFree(Some(path.0.cast()));
    }
    Ok(PathBuf::from(text?).join("pb-mapper-desktop"))
}
pub fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy()),
        _ => false,
    }
}

pub fn protected_file(path: &Path) -> Result<()> {
    protected_path(path)?;
    if !path.is_file() {
        return Err(error("expected a protected installation file"));
    }
    Ok(())
}
pub(super) fn protected_path(path: &Path) -> Result<()> {
    let root = install_root()?;
    if !contained_path(&root, path) {
        return Err(error("helper path is outside its protected installation"));
    }
    let mut current = path;
    loop {
        let attributes = unsafe { GetFileAttributesW(PCWSTR(wide(current).as_ptr())) };
        if attributes == INVALID_FILE_ATTRIBUTES || attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        {
            return Err(error(
                "reparse points are forbidden in the helper installation",
            ));
        }
        check_acl(current, WRITE_ACCESS)?;
        if current == root {
            break;
        }
        current = current
            .parent()
            .ok_or_else(|| error("invalid installation root"))?;
    }
    Ok(())
}
fn contained_path(root: &Path, path: &Path) -> bool {
    path.is_absolute()
        && path.starts_with(root)
        && !path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}
fn trusted_sid(sid: PSID) -> bool {
    unsafe {
        IsWellKnownSid(sid, WinLocalSystemSid).as_bool()
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid).as_bool()
    }
}
/// WRITE_DATA, APPEND_DATA, WRITE_EA, DELETE_CHILD, WRITE_ATTRIBUTES, DELETE,
/// WRITE_DAC, WRITE_OWNER, GENERIC_WRITE and GENERIC_ALL.
const WRITE_ACCESS: u32 = 0x500d0156;
/// `path` must be owned by SYSTEM or administrators, and grant nobody else
/// any of the `forbidden` access bits.
pub(super) fn check_acl(path: &Path, forbidden: u32) -> Result<()> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let mut owner = PSID::default();
    let mut acl = std::ptr::null_mut();
    unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(wide(path).as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut acl),
            None,
            &mut descriptor,
        )
    }
    .ok()
    .map_err(error)?;
    let result = check_descriptor(owner, acl, forbidden);
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    result
}
fn check_descriptor(owner: PSID, acl: *const ACL, forbidden: u32) -> Result<()> {
    // Descriptors obtained from Windows are validated before reaching this
    // routine. Deny entries cannot grant access, so they are harmless.
    (|| {
        if !trusted_sid(owner) || acl.is_null() {
            return Err(error("helper installation has an unsafe owner or DACL"));
        }
        for index in 0..unsafe { (*acl).AceCount } {
            let mut ace = std::ptr::null_mut();
            unsafe { GetAce(acl, u32::from(index), &mut ace) }.map_err(error)?;
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            if header.AceType == 0 {
                // ACCESS_ALLOWED_ACE_TYPE
                let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                let sid = PSID((&allowed.SidStart as *const u32).cast_mut().cast());
                if allowed.Mask & forbidden != 0 && !trusted_sid(sid) {
                    return Err(error(
                        "helper files grant an unprivileged identity forbidden access",
                    ));
                }
            } else if header.AceType != 1 {
                return Err(error("unsupported installation ACL"));
            }
        }
        Ok(())
    })()
}
pub fn token_for_process(process: HANDLE) -> Result<Handle> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(error)?;
    Ok(unsafe { Handle::own(token) })
}
pub fn sid(token: HANDLE) -> Result<String> {
    let mut length = 0;
    unsafe {
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut length);
    }
    let mut data = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(data.as_mut_ptr().cast()),
            length,
            &mut length,
        )
    }
    .map_err(error)?;
    let info = unsafe { &*data.as_ptr().cast::<TOKEN_USER>() };
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(info.User.Sid, &mut text) }.map_err(error)?;
    let result = unsafe { text.to_string() }.map_err(error);
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    }
    result
}
pub fn current_sid() -> Result<String> {
    sid(token_for_process(unsafe { GetCurrentProcess() })?.raw())
}
pub fn image(process: HANDLE) -> Result<PathBuf> {
    let mut text = vec![0u16; 32768];
    let mut len = text.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(text.as_mut_ptr()),
            &mut len,
        )
    }
    .map_err(error)?;
    Ok(PathBuf::from(String::from_utf16_lossy(
        &text[..len as usize],
    )))
}
#[cfg(feature = "windows-helper")]
pub fn process(id: u32) -> Result<Handle> {
    unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            id,
        )
    }
    .map(|h| unsafe { Handle::own(h) })
    .map_err(error)
}
#[cfg(feature = "windows-helper")]
pub fn session(id: u32) -> Result<u32> {
    let mut session = 0;
    unsafe { ProcessIdToSessionId(id, &mut session) }.map_err(error)?;
    Ok(session)
}
pub fn active_session() -> u32 {
    unsafe { WTSGetActiveConsoleSessionId() }
}

#[cfg(feature = "windows-helper")]
static PRIVILEGES: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(feature = "windows-helper")]
pub(super) struct ScopedPrivileges {
    pub token: Handle,
    previous: Vec<TOKEN_PRIVILEGES>,
    _scope: parking_lot::MutexGuard<'static, ()>,
}
#[cfg(feature = "windows-helper")]
impl ScopedPrivileges {
    pub fn new(names: &[PCWSTR]) -> Result<Self> {
        let mut token = HANDLE::default();
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES | TOKEN_DUPLICATE,
                &mut token,
            )
        }
        .map_err(error)?;
        Self::on_token(unsafe { Handle::own(token) }, names)
    }

    fn on_token(token: Handle, names: &[PCWSTR]) -> Result<Self> {
        // Process privileges are shared by service tasks. No scope survives an
        // await, and serialized restoration cannot disable another operation's
        // temporary privileges or leave them enabled after both operations.
        let guard = PRIVILEGES.lock();
        let mut scope = Self {
            token,
            previous: Vec::with_capacity(names.len()),
            _scope: guard,
        };
        for name in names {
            let mut luid = LUID::default();
            unsafe { LookupPrivilegeValueW(None, *name, &mut luid) }.map_err(error)?;
            let privileges = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            let mut previous = TOKEN_PRIVILEGES::default();
            // Windows requires ReturnLength whenever PreviousState is supplied.
            // A null pointer fails with ERROR_NOACCESS before any session query.
            let mut previous_length = 0;
            unsafe {
                AdjustTokenPrivileges(
                    scope.token.raw(),
                    false,
                    Some(&privileges),
                    size_of::<TOKEN_PRIVILEGES>() as u32,
                    Some(&mut previous),
                    Some(&mut previous_length),
                )
            }
            .map_err(|e| error(format!("AdjustTokenPrivileges failed: {e}")))?;
            let adjusted = unsafe { GetLastError() };
            scope.previous.push(previous);
            if adjusted == ERROR_NOT_ALL_ASSIGNED {
                return Err(error("service lacks a required session privilege"));
            }
        }
        Ok(scope)
    }
}
#[cfg(feature = "windows-helper")]
impl Drop for ScopedPrivileges {
    fn drop(&mut self) {
        for previous in self.previous.iter().rev() {
            if unsafe {
                AdjustTokenPrivileges(self.token.raw(), false, Some(previous), 0, None, None)
            }
            .is_err()
            {
                std::process::abort();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConsoleState {
    Locked,
    Unlocked,
}

#[cfg(feature = "windows-helper")]
pub(super) fn background_session(owner_sid: &str) -> Result<Option<u32>> {
    Ok(available_owner_session(owner_sid)?.map(|identity| identity.session))
}

#[cfg(feature = "windows-helper")]
pub(super) fn owner_session(owner_sid: &str) -> Result<SessionIdentity> {
    available_owner_session(owner_sid)?
        .ok_or_else(|| error("Waiting for the approved user to sign in to the active console"))
}

/// An absent/different login is ordinary waiting. OS query/privilege failures
/// are errors, never evidence that the installation owner has logged out.
#[cfg(feature = "windows-helper")]
fn available_owner_session(owner_sid: &str) -> Result<Option<SessionIdentity>> {
    let session = active_session();
    if session == 0 || session == u32::MAX {
        return Ok(None);
    }
    let Some((identity, _)) = session_state(session, &console_info(session)?) else {
        return Ok(None);
    };
    // WTSQueryUserToken requires LocalSystem/SeTcbPrivilege. Limit that
    // privilege to this synchronous query, restoring it before any await.
    let privileges = ScopedPrivileges::new(&[SE_TCB_NAME])
        .map_err(|e| error(format!("Cannot query the approved Windows login: {e}")))?;
    let mut token = HANDLE::default();
    unsafe { WTSQueryUserToken(session, &mut token) }
        .map_err(|e| error(format!("WTSQueryUserToken failed: {e}")))?;
    let token = unsafe { Handle::own(token) };
    if sid(token.raw())? != owner_sid {
        return Ok(None);
    }
    drop(privileges);
    identity.check()?;
    Ok(Some(identity))
}

#[cfg(feature = "windows-helper")]
pub fn verify_client(
    pipe: &tokio::net::windows::named_pipe::NamedPipeServer,
    config: &Configuration,
) -> Result<(Handle, SessionIdentity)> {
    let owner = verify_pipe_identity(pipe, config, true)?;
    Ok((owner, owner_session(&config.owner_sid)?))
}

/// Management remains available to the installed owner UI even while that
/// user's console is disconnected. It grants no capture or input by itself.
#[cfg(feature = "windows-helper")]
pub(super) fn verify_management_client(
    pipe: &tokio::net::windows::named_pipe::NamedPipeServer,
    config: &Configuration,
) -> Result<Handle> {
    verify_pipe_identity(pipe, config, false)
}

#[cfg(feature = "windows-helper")]
fn verify_pipe_identity(
    pipe: &tokio::net::windows::named_pipe::NamedPipeServer,
    config: &Configuration,
    capture: bool,
) -> Result<Handle> {
    let mut id = 0;
    unsafe { GetNamedPipeClientProcessId(super::pipe::raw(pipe), &mut id) }.map_err(error)?;
    let process = process(id)?;
    let caller_session = session(id)?;
    let caller_sid = sid(token_for_process(process.raw())?.raw())?;
    // Only this exact service process may use the background-host factory.
    // Another SYSTEM process does not inherit the application's authorization.
    let internal = capture
        && id == std::process::id()
        && caller_session == 0
        && caller_sid == "S-1-5-18"
        && same_path(&image(process.raw())?, &config.helper_executable);
    if !internal
        && (caller_session == 0
            || (capture && caller_session != active_session())
            || caller_sid != config.owner_sid
            || !same_path(&image(process.raw())?, &config.ui_executable))
    {
        return Err(error(
            "helper caller is not the approved console application",
        ));
    }
    protected_file(&config.ui_executable)?;
    // Bind to the pipe's actual security context too, rather than trusting a
    // PID sent in a message. Revert synchronously before any await point.
    unsafe { ImpersonateNamedPipeClient(super::pipe::raw(pipe)) }.map_err(error)?;
    let verified = (|| {
        let mut token = HANDLE::default();
        unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token) }
            .map_err(error)?;
        let token = unsafe { Handle::own(token) };
        if sid(token.raw())? != caller_sid {
            return Err(error("pipe identity differs from process identity"));
        }
        Ok(())
    })();
    if unsafe { RevertToSelf() }.is_err() {
        std::process::abort();
    }
    verified?;
    Ok(process)
}

struct ServiceHandle(SC_HANDLE);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}
fn open_service(access: u32) -> windows::core::Result<ServiceHandle> {
    let manager = ServiceHandle(unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }?);
    Ok(ServiceHandle(unsafe {
        OpenServiceW(manager.0, PCWSTR(wide(SERVICE).as_ptr()), access)
    }?))
}
fn service_status() -> Result<SERVICE_STATUS_PROCESS> {
    query_service_status(&open_service(SERVICE_QUERY_STATUS).map_err(error)?)
}
fn query_service_status(service: &ServiceHandle) -> Result<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            Some(std::slice::from_raw_parts_mut(
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
                size_of::<SERVICE_STATUS_PROCESS>(),
            )),
            &mut needed,
        )
    }
    .map_err(error)?;
    Ok(status)
}
pub fn service_state() -> Result<Option<(bool, bool)>> {
    let service = match open_service(SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG) {
        Ok(service) => service,
        Err(e)
            if e.code() == windows::core::HRESULT::from_win32(ERROR_SERVICE_DOES_NOT_EXIST.0) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(error(e)),
    };
    let disabled = service_startup_disabled(&service)?;
    let state = query_service_status(&service)?.dwCurrentState;
    // A disabled startup policy alone says nothing about an existing process.
    // Pending transitions and failed queries must never allow a second host.
    Ok(Some((
        state == SERVICE_RUNNING,
        state == SERVICE_STOPPED && disabled,
    )))
}
fn service_startup_disabled(service: &ServiceHandle) -> Result<bool> {
    // QueryServiceConfig has a documented 8 KiB maximum. Word storage keeps
    // QUERY_SERVICE_CONFIGW aligned and avoids a second, racy size query.
    let mut buffer = [0usize; 8192 / size_of::<usize>()];
    let config = buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    let mut needed = 0;
    unsafe { QueryServiceConfigW(service.0, Some(config), 8192, &mut needed) }.map_err(error)?;
    Ok(unsafe { (*config).dwStartType } == SERVICE_DISABLED)
}
pub fn verify_server(
    pipe: &tokio::net::windows::named_pipe::NamedPipeClient,
    expected: &Path,
    parent: Option<u32>,
) -> Result<()> {
    let mut id = 0;
    unsafe { GetNamedPipeServerProcessId(super::pipe::raw(pipe), &mut id) }.map_err(error)?;
    let expected_id = if let Some(id) = parent {
        id
    } else {
        running_service_pid()?
    };
    if id == 0 || id != expected_id {
        return Err(error(
            "named pipe is not owned by the installed helper service",
        ));
    }
    verify_process_image(id, expected)
}

fn running_service_pid() -> Result<u32> {
    let status = service_status()?;
    if status.dwCurrentState != SERVICE_RUNNING || status.dwProcessId == 0 {
        return Err(error("helper service is not running"));
    }
    Ok(status.dwProcessId)
}

pub fn verify_running_service(expected: &Path) -> Result<()> {
    verify_process_image(running_service_pid()?, expected)
}

fn verify_process_image(id: u32, expected: &Path) -> Result<()> {
    // A normal UI does not need SYNCHRONIZE access to a LocalSystem process.
    // Keep this handle alive while checking the image, preventing PID reuse.
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id) }.map_err(|e| {
            error(format!(
                "Cannot query helper process identity; update the Windows desktop service: {e}"
            ))
        })?;
    let process = unsafe { Handle::own(process) };
    if !same_path(&image(process.raw())?, expected) {
        return Err(error(
            "helper process image does not match the protected installation",
        ));
    }
    Ok(())
}

#[cfg(feature = "windows-helper")]
pub(super) fn record_service_failure(failure: &std::io::Error) {
    record_service_event("service_failed", &failure.to_string());
}

/// A diagnostics file in the protected installation root, opened to append:
/// SYSTEM and administrators own it, users may read it. Its length.
#[cfg(feature = "windows-helper")]
pub(super) fn open_service_log(name: &str) -> Result<(std::fs::File, u64)> {
    use std::io::{Seek, SeekFrom};
    let root = install_root()?;
    protected_path(&root)?;
    let path = root.join(name);
    if path.exists() {
        protected_file(&path)?;
    }
    let descriptor = Descriptor::new("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GR;;;BU)")?;
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide(&path).as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            Some(descriptor.attributes()),
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(error)?;
    let mut file = unsafe { std::fs::File::from_raw_handle(handle.0) };
    let len = file.seek(SeekFrom::End(0))?;
    Ok((file, len))
}

/// Keep one older copy of a diagnostics file: `name` becomes `name.1`,
/// replacing the last one.
#[cfg(feature = "windows-helper")]
pub(super) fn rotate_service_log(name: &str) -> Result<()> {
    let root = install_root()?;
    protected_path(&root)?;
    std::fs::rename(root.join(name), root.join(format!("{name}.1")))
}

/// Bounded control-path diagnostics; never pass grants, keys or input events.
#[cfg(feature = "windows-helper")]
pub(super) fn record_service_event(event: &str, detail: &str) {
    let write = || -> Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        let (mut file, len) = open_service_log("service.log")?;
        if len > 64 * 1024 {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
        }
        let text: String = detail
            .chars()
            .take(2000)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        writeln!(file, "{now} {event}: {text}")?;
        Ok(())
    };
    let _ = write();
}

#[cfg(all(test, feature = "windows-helper"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn privilege_scope_adjusts_and_restores_only_its_owned_token() {
        let mut original = HANDLE::default();
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE,
                &mut original,
            )
        }
        .unwrap();
        let original = unsafe { Handle::own(original) };
        let mut duplicate = HANDLE::default();
        unsafe {
            DuplicateTokenEx(
                original.raw(),
                TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES,
                None,
                SecurityImpersonation,
                TokenImpersonation,
                &mut duplicate,
            )
        }
        .unwrap();
        let duplicate = unsafe { Handle::own(duplicate) };
        let probe = Handle(duplicate.0.try_clone().unwrap());
        let mut luid = LUID::default();
        unsafe { LookupPrivilegeValueW(None, SE_CHANGE_NOTIFY_NAME, &mut luid) }.unwrap();
        // Modify only an independent token, never the parallel runner's token.
        let disabled = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: TOKEN_PRIVILEGES_ATTRIBUTES(0),
            }],
        };
        unsafe { AdjustTokenPrivileges(duplicate.raw(), false, Some(&disabled), 0, None, None) }
            .unwrap();
        let enabled = || {
            let mut needed = 0;
            unsafe {
                let _ = GetTokenInformation(probe.raw(), TokenPrivileges, None, 0, &mut needed);
            }
            let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
            unsafe {
                GetTokenInformation(
                    probe.raw(),
                    TokenPrivileges,
                    Some(buffer.as_mut_ptr().cast()),
                    needed,
                    &mut needed,
                )
            }
            .unwrap();
            let token = unsafe { &*buffer.as_ptr().cast::<TOKEN_PRIVILEGES>() };
            unsafe {
                std::slice::from_raw_parts(token.Privileges.as_ptr(), token.PrivilegeCount as usize)
            }
            .iter()
            .find(|entry| entry.Luid == luid)
            .unwrap()
            .Attributes
            .contains(SE_PRIVILEGE_ENABLED)
        };
        assert!(!enabled());
        let scope = ScopedPrivileges::on_token(duplicate, &[SE_CHANGE_NOTIFY_NAME]).unwrap();
        assert!(enabled());
        drop(scope);
        assert!(!enabled());
        // A failure after the first adjustment must also restore that change.
        assert!(
            ScopedPrivileges::on_token(
                Handle(probe.0.try_clone().unwrap()),
                &[
                    SE_CHANGE_NOTIFY_NAME,
                    windows::core::w!("pb-mapper-invalid-privilege")
                ]
            )
            .is_err()
        );
        assert!(!enabled());
    }

    fn validate(sddl: &str, forbidden: u32) -> Result<()> {
        let descriptor = Descriptor::new(sddl)?;
        let mut owner = PSID::default();
        let mut acl = std::ptr::null_mut();
        let mut defaulted = windows::core::BOOL(0);
        let mut present = windows::core::BOOL(0);
        unsafe {
            GetSecurityDescriptorOwner(descriptor.descriptor, &mut owner, &mut defaulted)
                .map_err(error)?;
            GetSecurityDescriptorDacl(
                descriptor.descriptor,
                &mut present,
                &mut acl,
                &mut defaulted,
            )
            .map_err(error)?;
        }
        check_descriptor(owner, acl, forbidden)
    }
    #[test]
    fn protected_acl_rejects_forbidden_access_and_untrusted_owner() {
        let readable = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGX;;;BU)";
        assert!(validate(readable, WRITE_ACCESS).is_ok());
        assert!(
            validate(
                "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x40;;;BU)",
                WRITE_ACCESS
            )
            .is_err()
        );
        assert!(validate("O:BUG:BUD:P(A;;GRGX;;;BU)", WRITE_ACCESS).is_err());
        // A background grant may not even be read by anyone else.
        assert!(validate("O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)", u32::MAX).is_ok());
        assert!(validate(readable, u32::MAX).is_err());
        let root = Path::new(r"C:\Program Files\pb-mapper-desktop");
        assert!(contained_path(root, &root.join(r"release-safe\helper.exe")));
        assert!(!contained_path(root, &root.join(r"..\outside.exe")));
    }
    #[test]
    fn console_identity_survives_lock_but_not_logoff_or_session_reuse() {
        let mut info = WTSINFOEX_LEVEL1_W {
            SessionId: 7,
            SessionState: WTSActive,
            SessionFlags: WTS_SESSIONSTATE_UNLOCK as i32,
            LogonTime: 100,
            ..Default::default()
        };
        assert!(session_state(7, &info).is_none());
        info.UserName[0] = u16::from(b'u');
        let (identity, state) = session_state(7, &info).unwrap();
        assert_eq!(state, ConsoleState::Unlocked);
        assert!(identity.matches(&info));
        assert!(session_state(8, &info).is_none());
        info.SessionState = WTSDisconnected;
        assert!(session_state(7, &info).is_none());
        info.SessionState = WTSActive;
        info.SessionFlags = WTS_SESSIONSTATE_LOCK as i32;
        assert_eq!(
            session_state(7, &info),
            Some((identity.clone(), ConsoleState::Locked))
        );
        // Even the same user logging in again at a reused session ID is a new
        // authorization boundary; the old worker must never follow it.
        info.LogonTime += 1;
        assert_ne!(session_state(7, &info).unwrap().0, identity);
        assert!(!identity.matches(&info));
        info.LogonTime -= 1;
        info.UserName[0] = u16::from(b'v');
        assert_ne!(session_state(7, &info).unwrap().0, identity);
        assert!(!identity.matches(&info));
        info.UserName[0] = 0;
        assert!(session_state(7, &info).is_none());
    }
}

/// What the elevated installer is asked to do, each behind one consent prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Install this bundle, or update the installed helper in place. An update
    /// keeps the service's startup type and running state, stopped or Disabled
    /// included, unless `enable` restores automatic startup and starts it, as
    /// `Enable` does.
    Install {
        enable: bool,
    },
    Enable,
    Disable,
    Uninstall,
}

/// Launch a separate installer with the OS consent UI. No password or UAC
/// policy is handled here. The caller's process never gains administrator rights.
pub fn request_installation(action: Action) -> Result<()> {
    let executable = std::env::current_exe()?;
    let source = executable
        .parent()
        .ok_or_else(|| error("missing application directory"))?;
    let helper = source.join(EXECUTABLE);
    if !helper.is_file() {
        return Err(error("This build does not include the Windows helper"));
    }
    let params = match action {
        Action::Install { enable } => format!(
            "install \"{}\" {}{}",
            source.display(),
            current_sid()?,
            if enable { " enable" } else { "" }
        ),
        Action::Enable => "enable".into(),
        Action::Disable => "disable".into(),
        Action::Uninstall => "uninstall".into(),
    };
    let file = wide(&helper);
    let parameters = wide(params);
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    let started = std::time::SystemTime::now();
    unsafe { ShellExecuteExW(&mut info) }.map_err(error)?;
    let process = unsafe { Handle::own(info.hProcess) };
    if unsafe { WaitForSingleObject(process.raw(), 300_000) } != WAIT_OBJECT_0 {
        return Err(error("helper installation did not finish"));
    }
    let mut code = 1;
    unsafe { GetExitCodeProcess(process.raw(), &mut code) }.map_err(error)?;
    if code != 0 {
        // The log lives beside the protected installation, never at a caller-
        // supplied location that an elevated process could be tricked into writing.
        let log = install_root()?.join("installation.log");
        let detail = protected_file(&log).ok().and_then(|()| {
            let metadata = std::fs::metadata(&log).ok()?;
            if metadata.len() > 4096 || metadata.modified().ok()? < started {
                return None;
            }
            std::fs::read_to_string(log).ok()
        });
        return Err(error(detail.unwrap_or_else(|| {
            "helper installer failed before it could write its protected installation log".into()
        })));
    }
    Ok(())
}

fn console_info(session: u32) -> Result<WTSINFOEX_LEVEL1_W> {
    if session == 0 || session == u32::MAX || session != active_session() {
        return Err(error("Only the active console session is supported"));
    }
    let mut buffer = PWSTR::null();
    let mut len = 0;
    unsafe { WTSQuerySessionInformationW(None, session, WTSSessionInfoEx, &mut buffer, &mut len) }
        .map_err(error)?;
    let info = if len as usize >= size_of::<WTSINFOEXW>() {
        let info = unsafe { &*buffer.0.cast::<WTSINFOEXW>() };
        if info.Level == 1 {
            Some(unsafe { info.Data.WTSInfoExLevel1 })
        } else {
            None
        }
    } else {
        None
    };
    unsafe {
        WTSFreeMemory(buffer.0.cast::<c_void>());
    }
    info.ok_or_else(|| error("Console session information is unavailable"))
}

fn console_state(session: u32, info: &WTSINFOEX_LEVEL1_W) -> Option<ConsoleState> {
    if session == 0
        || session == u32::MAX
        || info.SessionId != session
        || info.SessionState != WTSActive
        || info.UserName[0] == 0
        || info.LogonTime <= 0
    {
        return None;
    }
    match info.SessionFlags as u32 {
        WTS_SESSIONSTATE_LOCK => Some(ConsoleState::Locked),
        WTS_SESSIONSTATE_UNLOCK => Some(ConsoleState::Unlocked),
        _ => None,
    }
}

#[cfg(feature = "windows-helper")]
fn session_state(
    session: u32,
    info: &WTSINFOEX_LEVEL1_W,
) -> Option<(SessionIdentity, ConsoleState)> {
    let state = console_state(session, info)?;
    let text = |value: &[u16]| {
        String::from_utf16_lossy(
            &value[..value.iter().position(|c| *c == 0).unwrap_or(value.len())],
        )
    };
    Some((
        SessionIdentity {
            session,
            logon_time: info.LogonTime,
            user: text(&info.UserName),
            domain: text(&info.DomainName),
        },
        state,
    ))
}

impl SessionIdentity {
    pub(super) fn check(&self) -> Result<ConsoleState> {
        let info = console_info(self.session)?;
        let state = console_state(self.session, &info)
            .ok_or_else(|| error("The approved console login session is unavailable"))?;
        if !self.matches(&info) {
            return Err(error("The approved console login session has ended"));
        }
        Ok(state)
    }

    fn matches(&self, info: &WTSINFOEX_LEVEL1_W) -> bool {
        // Called on the capture/input path: compare existing UTF-16 slices,
        // allocating user/domain strings only when a login is first attested.
        self.session == info.SessionId
            && self.logon_time == info.LogonTime
            && self
                .user
                .encode_utf16()
                .eq(info.UserName.iter().copied().take_while(|c| *c != 0))
            && self.domain.encode_utf16().eq(info
                .DomainName
                .iter()
                .copied()
                .take_while(|c| *c != 0))
    }
}
