use super::*;
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
        check_acl(current)?;
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
pub(super) fn check_acl(path: &Path) -> Result<()> {
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
    let result = check_descriptor(owner, acl);
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    result
}
fn check_descriptor(owner: PSID, acl: *const ACL) -> Result<()> {
    // Descriptors obtained from Windows are validated before reaching this
    // routine. Deny entries cannot create write access, so they are harmless.
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
                // WRITE_DATA, APPEND_DATA, WRITE_EA, DELETE_CHILD, WRITE_ATTRIBUTES, DELETE,
                // WRITE_DAC, WRITE_OWNER, GENERIC_WRITE and GENERIC_ALL.
                let writable = allowed.Mask & 0x500d0156 != 0;
                let sid = PSID((&allowed.SidStart as *const u32).cast_mut().cast());
                if writable && !trusted_sid(sid) {
                    return Err(error(
                        "helper files are writable by an unprivileged identity",
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
pub fn verify_client(
    pipe: &tokio::net::windows::named_pipe::NamedPipeServer,
    config: &Configuration,
) -> Result<(Handle, u32)> {
    let mut id = 0;
    unsafe { GetNamedPipeClientProcessId(super::ipc::raw(pipe), &mut id) }.map_err(error)?;
    let process = process(id)?;
    let session = session(id)?;
    if session == 0
        || session != active_session()
        || !same_path(&image(process.raw())?, &config.ui_executable)
    {
        return Err(error(
            "helper caller is not the approved console application",
        ));
    }
    protected_file(&config.ui_executable)?;
    if sid(token_for_process(process.raw())?.raw())? != config.owner_sid {
        return Err(error("helper caller belongs to another identity"));
    }
    // Bind to the pipe's actual security context too, rather than trusting a
    // PID sent in a message. Revert synchronously before any await point.
    unsafe { ImpersonateNamedPipeClient(super::ipc::raw(pipe)) }.map_err(error)?;
    let verified = (|| {
        let mut token = HANDLE::default();
        unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token) }
            .map_err(error)?;
        let token = unsafe { Handle::own(token) };
        if sid(token.raw())? != config.owner_sid {
            return Err(error("pipe identity differs from process identity"));
        }
        Ok(())
    })();
    if unsafe { RevertToSelf() }.is_err() {
        std::process::abort();
    }
    verified?;
    Ok((process, session))
}

struct ServiceHandle(SC_HANDLE);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}
fn service_status() -> Result<SERVICE_STATUS_PROCESS> {
    let manager =
        ServiceHandle(unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }.map_err(error)?);
    let service = ServiceHandle(
        unsafe {
            OpenServiceW(
                manager.0,
                PCWSTR(wide(SERVICE).as_ptr()),
                SERVICE_QUERY_STATUS,
            )
        }
        .map_err(error)?,
    );
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
pub fn service_running() -> Result<bool> {
    Ok(service_status()?.dwCurrentState == SERVICE_RUNNING)
}
pub fn verify_server(
    pipe: &tokio::net::windows::named_pipe::NamedPipeClient,
    expected: &Path,
    parent: Option<u32>,
) -> Result<()> {
    let mut id = 0;
    unsafe { GetNamedPipeServerProcessId(super::ipc::raw(pipe), &mut id) }.map_err(error)?;
    let expected_id = if let Some(id) = parent {
        id
    } else {
        let status = service_status()?;
        if status.dwCurrentState != SERVICE_RUNNING {
            return Err(error("helper service is not running"));
        }
        status.dwProcessId
    };
    if id == 0 || id != expected_id {
        return Err(error(
            "named pipe is not owned by the installed helper service",
        ));
    }
    // A normal UI does not need SYNCHRONIZE access to a LocalSystem process.
    // Keep this handle alive while checking the image, preventing PID reuse.
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id) }.map_err(error)?;
    let process = unsafe { Handle::own(process) };
    if !same_path(&image(process.raw())?, expected) {
        return Err(error(
            "named pipe is not owned by the installed helper service",
        ));
    }
    Ok(())
}

#[cfg(feature = "windows-helper")]
pub(super) fn record_service_failure(failure: &std::io::Error) {
    let write = || -> Result<()> {
        let root = install_root()?;
        protected_path(&root)?;
        let path = root.join("service.log");
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
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(error)?;
        let mut file = unsafe { std::fs::File::from_raw_handle(handle.0) };
        use std::io::Write;
        let text: String = failure.to_string().chars().take(1000).collect();
        file.write_all(text.as_bytes())?;
        Ok(())
    };
    let _ = write();
}

#[cfg(all(test, feature = "windows-helper"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn validate(sddl: &str) -> Result<()> {
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
        check_descriptor(owner, acl)
    }
    #[test]
    fn protected_acl_rejects_delete_child_and_untrusted_owner() {
        assert!(validate("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGX;;;BU)").is_ok());
        assert!(validate("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x40;;;BU)").is_err());
        assert!(validate("O:BUG:BUD:P(A;;GRGX;;;BU)").is_err());
        let root = Path::new(r"C:\Program Files\pb-mapper-desktop");
        assert!(contained_path(root, &root.join(r"release-safe\helper.exe")));
        assert!(!contained_path(root, &root.join(r"..\outside.exe")));
    }
    #[test]
    fn only_the_logged_in_active_unlocked_session_is_authorized() {
        let mut info = WTSINFOEX_LEVEL1_W {
            SessionId: 7,
            SessionState: WTSActive,
            SessionFlags: WTS_SESSIONSTATE_UNLOCK as i32,
            ..Default::default()
        };
        assert!(!session_is_unlocked(7, &info));
        info.UserName[0] = u16::from(b'u');
        assert!(session_is_unlocked(7, &info));
        assert!(!session_is_unlocked(8, &info));
        info.SessionState = WTSDisconnected;
        assert!(!session_is_unlocked(7, &info));
        info.SessionState = WTSActive;
        info.SessionFlags = WTS_SESSIONSTATE_LOCK as i32;
        assert!(!session_is_unlocked(7, &info));
    }
}

/// Launch a separate installer with the OS consent UI. No password or UAC
/// policy is handled here. The caller's process never gains administrator rights.
pub fn request_installation(action: &str) -> Result<()> {
    if !["install", "disable", "uninstall"].contains(&action) {
        return Err(error("unknown helper action"));
    }
    let executable = std::env::current_exe()?;
    let source = executable
        .parent()
        .ok_or_else(|| error("missing application directory"))?;
    let helper = source.join(EXECUTABLE);
    if !helper.is_file() {
        return Err(error("This build does not include the Windows helper"));
    }
    let params = if action == "install" {
        format!("install \"{}\" {}", source.display(), current_sid()?)
    } else {
        action.to_owned()
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

pub fn console_unlocked(session: u32) -> Result<()> {
    if session == 0 || session != active_session() {
        return Err(error("Only the active console session is supported"));
    }
    let mut buffer = PWSTR::null();
    let mut len = 0;
    unsafe { WTSQuerySessionInformationW(None, session, WTSSessionInfoEx, &mut buffer, &mut len) }
        .map_err(error)?;
    let valid = if len as usize >= size_of::<WTSINFOEXW>() {
        let info = unsafe { &*buffer.0.cast::<WTSINFOEXW>() };
        info.Level == 1 && session_is_unlocked(session, unsafe { &info.Data.WTSInfoExLevel1 })
    } else {
        false
    };
    unsafe {
        WTSFreeMemory(buffer.0.cast::<c_void>());
    }
    if !valid {
        return Err(error(
            "Locked/login desktops are not supported; unlock locally",
        ));
    }
    Ok(())
}

fn session_is_unlocked(session: u32, info: &WTSINFOEX_LEVEL1_W) -> bool {
    // The Winlogon input desktop can mean either UAC or the login/lock screen.
    // Authorize the logged-in session state, never the desktop name alone.
    info.SessionId == session
        && info.SessionState == WTSActive
        && info.SessionFlags == WTS_SESSIONSTATE_UNLOCK as i32
        && info.UserName[0] != 0
}
