//! Who a process, token or pipe peer is: SIDs, images and sessions, the
//! scoped privileges the service enables, and the checks that admit an
//! approved caller or trust a helper server.
use super::{Handle, acl::same_path, scm::running_service_pid};
use crate::{Result, error};
use std::path::{Path, PathBuf};
use windows::{
    Win32::{
        Foundation::*,
        Security::Authorization::*,
        Security::*,
        System::{Pipes::*, Threading::*},
    },
    core::PWSTR,
};
#[cfg(feature = "windows-helper")]
use {
    super::{
        SessionIdentity,
        acl::protected_file,
        console::{active_session, owner_session},
    },
    crate::platform::windows::helper::Configuration,
    windows::{Win32::System::RemoteDesktop::ProcessIdToSessionId, core::PCWSTR},
};

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

#[cfg(feature = "windows-helper")]
static PRIVILEGES: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(feature = "windows-helper")]
pub struct ScopedPrivileges {
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
pub fn verify_management_client(
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
    unsafe { GetNamedPipeClientProcessId(super::super::pipe::raw(pipe), &mut id) }
        .map_err(error)?;
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
    unsafe { ImpersonateNamedPipeClient(super::super::pipe::raw(pipe)) }.map_err(error)?;
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

pub fn verify_server(
    pipe: &tokio::net::windows::named_pipe::NamedPipeClient,
    expected: &Path,
    parent: Option<u32>,
) -> Result<()> {
    let mut id = 0;
    unsafe { GetNamedPipeServerProcessId(super::super::pipe::raw(pipe), &mut id) }
        .map_err(error)?;
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

#[cfg(all(test, feature = "windows-helper"))]
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
}
