use super::*;
use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio_util::sync::CancellationToken;
use windows::{
    Win32::{
        Foundation::*,
        Security::*,
        System::{JobObjects::*, Threading::*},
    },
    core::{PCWSTR, PWSTR},
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType, SessionChangeParam, SessionChangeReason,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

define_windows_service!(service_main, entry);
pub fn run() -> Result<()> {
    service_dispatcher::start(SERVICE, service_main).map_err(error)
}
fn state(current_state: ServiceState, code: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state,
        controls_accepted: if current_state == ServiceState::Running {
            ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::SESSION_CHANGE
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    }
}
#[derive(Default)]
struct SessionEvents(Mutex<Option<(u32, CancellationToken)>>);
impl SessionEvents {
    fn begin(&self, session: u32) -> CancellationToken {
        let cancel = CancellationToken::new();
        let mut active = self.0.lock().unwrap_or_else(|_| std::process::abort());
        if let Some((_, previous)) = active.replace((session, cancel.clone())) {
            previous.cancel();
        }
        cancel
    }
    fn changed(&self, event: SessionChangeParam) {
        use SessionChangeReason::*;
        if !matches!(
            event.reason,
            ConsoleDisconnect | RemoteDisconnect | SessionLogoff | SessionLock | SessionTerminate
        ) {
            return;
        }
        let active = self.0.lock().unwrap_or_else(|_| std::process::abort());
        if let Some((session, cancel)) = active.as_ref()
            && *session == event.notification.session_id
        {
            // Revocation is sticky: a later unlock must not resurrect this
            // connection, even when events arrive faster than our WTS poll.
            cancel.cancel();
        }
    }
}
fn entry(_: Vec<OsString>) {
    let stop = CancellationToken::new();
    let signal = stop.clone();
    let sessions = Arc::new(SessionEvents::default());
    let changes = sessions.clone();
    let Ok(status) = service_control_handler::register(SERVICE, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            signal.cancel();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(event) => {
            changes.changed(event);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }) else {
        return;
    };
    let result = (|| {
        status
            .set_service_status(state(ServiceState::StartPending, 0))
            .map_err(error)?;
        let config = configuration()?;
        if !os::same_path(&std::env::current_exe()?, &config.helper_executable)
            || os::current_sid()? != "S-1-5-18"
        {
            return Err(error("helper service identity is invalid"));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let mut listener = ipc::bind(PIPE, Some(&config.owner_sid), true)?;
            status.set_service_status(state(ServiceState::Running, 0)).map_err(error)?;
            loop {
                tokio::select! { () = stop.cancelled() => break, result = listener.connect() => result? }
                // Keep one live server instance while replacing the listener,
                // so another process can never claim the pipe name in between.
                let next = ipc::bind(PIPE, Some(&config.owner_sid), false)?;
                let connection = std::mem::replace(&mut listener, next);
                let _ = session(connection, &config, stop.clone(), &sessions).await;
            }
            Ok::<_, std::io::Error>(())
        })
    })();
    if let Err(failure) = &result {
        os::record_service_failure(failure);
    }
    let _ = status.set_service_status(state(ServiceState::Stopped, u32::from(result.is_err())));
}
async fn session(
    mut caller: NamedPipeServer,
    config: &Configuration,
    stop: CancellationToken,
    sessions: &SessionEvents,
) -> Result<()> {
    let request: Request = tokio::time::timeout(TIMEOUT, crate::wire::read_json(&mut caller))
        .await
        .map_err(error)??;
    if !matches!(request, Request::Start { .. }) {
        return Err(error("helper requires a start request"));
    }
    let (owner, session) = os::verify_client(&caller, config)?;
    let revoked = sessions.begin(session);
    os::console_unlocked(session)?;
    let name = format!("{PIPE}.worker.{}", crate::security::secret()?);
    let mut private = ipc::bind(&name, None, true)?;
    let child = spawn_worker(config, session, &name)?;
    tokio::time::timeout(TIMEOUT, private.connect())
        .await
        .map_err(error)??;
    let mut worker_pid = 0;
    unsafe {
        windows::Win32::System::Pipes::GetNamedPipeClientProcessId(
            ipc::raw(&private),
            &mut worker_pid,
        )
    }
    .map_err(error)?;
    if worker_pid != child.pid {
        return Err(error("unexpected worker process"));
    }
    if revoked.is_cancelled() || stop.is_cancelled() {
        return Err(error("desktop session ended during worker startup"));
    }
    os::console_unlocked(session)?;
    tokio::time::timeout(TIMEOUT, crate::wire::write_json(&mut private, &request))
        .await
        .map_err(error)??;
    let watch = async {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            if os::console_unlocked(session).is_err()
                || unsafe { WaitForSingleObject(owner.raw(), 0) } != WAIT_TIMEOUT
            {
                break;
            }
        }
    };
    tokio::select! {
        () = stop.cancelled() => {},
        () = revoked.cancelled() => {},
        () = watch => {},
        _ = tokio::io::copy_bidirectional(&mut caller, &mut private) => {},
    }
    drop(private); // EOF wakes the worker and releases its held input.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while unsafe { WaitForSingleObject(child.process.raw(), 0) } == WAIT_TIMEOUT
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    drop(child); // Job close terminates a worker that failed to exit.
    Ok(())
}
struct Child {
    _job: os::Handle,
    process: os::Handle,
    pid: u32,
}
struct LaunchPrivileges {
    token: os::Handle,
    previous: Vec<TOKEN_PRIVILEGES>,
}
impl Drop for LaunchPrivileges {
    fn drop(&mut self) {
        for previous in self.previous.iter().rev() {
            if unsafe {
                AdjustTokenPrivileges(self.token.raw(), false, Some(previous), 0, None, None)
            }
            .is_err()
            {
                // Never carry a failed temporary privilege restoration into the
                // long-lived, externally reachable IPC loop.
                std::process::abort();
            }
        }
    }
}
fn launch_privileges() -> Result<LaunchPrivileges> {
    let mut original = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut original) }
        .map_err(error)?;
    let mut scope = LaunchPrivileges {
        token: unsafe { os::Handle::own(original) },
        previous: vec![],
    };
    for privilege in [
        SE_TCB_NAME,
        SE_ASSIGNPRIMARYTOKEN_NAME,
        SE_INCREASE_QUOTA_NAME,
    ] {
        let mut luid = LUID::default();
        unsafe { LookupPrivilegeValueW(None, privilege, &mut luid) }.map_err(error)?;
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let mut previous = TOKEN_PRIVILEGES::default();
        unsafe {
            AdjustTokenPrivileges(
                scope.token.raw(),
                false,
                Some(&privileges),
                size_of::<TOKEN_PRIVILEGES>() as u32,
                Some(&mut previous),
                None,
            )
        }
        .map_err(error)?;
        scope.previous.push(previous);
        if unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED {
            return Err(error("service lacks a required session-launch privilege"));
        }
    }
    Ok(scope)
}
fn spawn_worker(config: &Configuration, session: u32, pipe: &str) -> Result<Child> {
    let privileges = launch_privileges()?;
    let mut duplicate = HANDLE::default();
    unsafe {
        DuplicateTokenEx(
            privileges.token.raw(),
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut duplicate,
        )
    }
    .map_err(error)?;
    let duplicate = unsafe { os::Handle::own(duplicate) };
    unsafe {
        SetTokenInformation(
            duplicate.raw(),
            TokenSessionId,
            (&session as *const u32).cast(),
            size_of::<u32>() as u32,
        )
    }
    .map_err(error)?;
    let mut restricted = HANDLE::default();
    unsafe {
        CreateRestrictedToken(
            duplicate.raw(),
            DISABLE_MAX_PRIVILEGE,
            None,
            None,
            None,
            &mut restricted,
        )
    }
    .map_err(error)?;
    let restricted = unsafe { os::Handle::own(restricted) };
    let job = unsafe { os::Handle::own(CreateJobObjectW(None, None).map_err(error)?) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    unsafe {
        SetInformationJobObject(
            job.raw(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )
    }
    .map_err(error)?;
    let file = os::wide(&config.helper_executable);
    let mut command = os::wide(format!(
        "\"{}\" worker {pipe} {}",
        config.helper_executable.display(),
        std::process::id()
    ));
    let mut desktop = os::wide("winsta0\\default");
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessAsUserW(
            Some(restricted.raw()),
            PCWSTR(file.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED | CREATE_NO_WINDOW,
            None,
            PCWSTR(
                os::wide(
                    config
                        .helper_executable
                        .parent()
                        .ok_or_else(|| error("invalid helper path"))?,
                )
                .as_ptr(),
            ),
            &startup,
            &mut process,
        )
    }
    .map_err(error)?;
    let handle = unsafe { os::Handle::own(process.hProcess) };
    let thread = unsafe { os::Handle::own(process.hThread) };
    if let Err(e) = unsafe { AssignProcessToJobObject(job.raw(), handle.raw()) } {
        unsafe {
            let _ = TerminateProcess(handle.raw(), 1);
        }
        return Err(error(e));
    }
    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        return Err(error("could not resume desktop worker"));
    }
    Ok(Child {
        _job: job,
        process: handle,
        pid: process.dwProcessId,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_service::service::SessionNotification;

    #[test]
    fn lock_revokes_only_its_session_and_unlock_cannot_restore_it() {
        let sessions = SessionEvents::default();
        let active = sessions.begin(7);
        let event = |session_id, reason| SessionChangeParam {
            reason,
            notification: SessionNotification {
                size: size_of::<SessionNotification>() as u32,
                session_id,
            },
        };
        sessions.changed(event(8, SessionChangeReason::SessionLock));
        sessions.changed(event(7, SessionChangeReason::SessionUnlock));
        assert!(!active.is_cancelled());
        sessions.changed(event(7, SessionChangeReason::SessionLock));
        sessions.changed(event(7, SessionChangeReason::SessionUnlock));
        assert!(active.is_cancelled());
        let replacement = sessions.begin(7);
        assert!(!replacement.is_cancelled());
        sessions.changed(event(7, SessionChangeReason::SessionLogoff));
        assert!(replacement.is_cancelled());
    }
}
