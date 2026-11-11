//! The worker processes the service starts, each in its console session and
//! in a job that ends it, and the `restore` runs among them.
use super::{Configuration, os};
use crate::{Result, error};
use std::{
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::*,
        Security::*,
        System::{JobObjects::*, Threading::*},
    },
    core::{PCWSTR, PWSTR},
};

/// The `restore` workers, at most one per console session, each putting
/// back the display mode a worker there left journalled. A worker never
/// acts beside one in its session: both act on the session's journal, so
/// the run ends, and is gone, just before a started and verified worker is
/// attached there, and starts again if that attach fails.
#[derive(Clone, Default)]
pub(super) struct Restores(Arc<Mutex<Vec<Run>>>);
struct Run {
    session: u32,
    serial: u64,
    child: Child,
}
static RUNS: AtomicU64 = AtomicU64::new(0);
impl Restores {
    fn runs(&self) -> MutexGuard<'_, Vec<Run>> {
        self.0.lock().unwrap_or_else(|_| std::process::abort())
    }

    /// Start the run in `session`, after ending one still there, and keep it
    /// until it exits or overruns its ten minutes.
    pub(super) async fn start(&self, config: &Configuration, session: u32) {
        self.end(session).await;
        let child = match spawn_worker(config, session, &format!("restore {session}")) {
            Ok(child) => child,
            Err(failure) => {
                return os::record_service_event("restore_failed", &failure.to_string());
            }
        };
        let serial = RUNS.fetch_add(1, Ordering::Relaxed);
        self.runs().push(Run {
            session,
            serial,
            child,
        });
        let runs = self.clone();
        tokio::spawn(async move {
            let deadline =
                tokio::time::Instant::now() + super::worker::RESTORE_FOR + Duration::from_secs(10);
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let mut runs = runs.runs();
                // Gone already: a worker's start ended it.
                let Some(at) = runs.iter().position(|run| run.serial == serial) else {
                    return;
                };
                if runs[at].child.exit_code().is_some() || tokio::time::Instant::now() >= deadline {
                    // Dropped: its job ends one that overran.
                    runs.swap_remove(at);
                    return;
                }
            }
        });
    }

    /// End the run in `session`, if there is one, and wait until it is gone.
    pub(super) async fn end(&self, session: u32) {
        let run = {
            let mut runs = self.runs();
            runs.iter()
                .position(|run| run.session == session)
                .map(|at| runs.swap_remove(at))
        };
        if let Some(run) = run {
            run.child.end().await;
        }
    }
}

/// At service start: a journal a worker left from before (the service
/// crashed, or was stopped mid-change) is put back in its session.
pub(super) async fn restore_left_modes(config: &Configuration, restores: &Restores) {
    let Ok(entries) = os::install_root().and_then(std::fs::read_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let session = name
            .to_str()
            .and_then(|name| name.strip_prefix("display-modes-"))
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|session| session.parse::<u32>().ok());
        if let Some(session) = session
            && super::worker::journal(session).is_ok_and(|journal| journal.pending())
        {
            restores.start(config, session).await;
        }
    }
}

pub(super) struct Child {
    job: os::Handle,
    process: os::Handle,
    pub(super) pid: u32,
}
impl Child {
    /// `None` while it runs.
    pub(super) fn exit_code(&self) -> Option<u32> {
        let mut code = 0;
        unsafe { GetExitCodeProcess(self.process.raw(), &mut code) }.ok()?;
        (code != STILL_ACTIVE.0 as u32).then_some(code)
    }
    /// End it now, and wait until it is gone: two seconds at most.
    async fn end(self) {
        let _ = unsafe { TerminateJobObject(self.job.raw(), 1) };
        self.wait_exit(Duration::from_secs(2)).await;
    }
    /// Wait until it has exited, `within` at most.
    pub(super) async fn wait_exit(&self, within: Duration) {
        let deadline = tokio::time::Instant::now() + within;
        while unsafe { WaitForSingleObject(self.process.raw(), 0) } == WAIT_TIMEOUT
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }
}
/// The helper in `session` with `args`, in a job that ends it with the job.
pub(super) fn spawn_worker(config: &Configuration, session: u32, args: &str) -> Result<Child> {
    let privileges = os::ScopedPrivileges::new(&[
        SE_TCB_NAME,
        SE_ASSIGNPRIMARYTOKEN_NAME,
        SE_INCREASE_QUOTA_NAME,
    ])?;
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
    let mut command = os::wide(format!("\"{}\" {args}", config.helper_executable.display()));
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
        job,
        process: handle,
        pid: process.dwProcessId,
    })
}
