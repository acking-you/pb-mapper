use super::*;
use crate::oop::proto::{Request, TIMEOUT};
use std::{
    ffi::OsString,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
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

/// The background host's log, beside `service.log`: its sessions, the
/// feedback that changes how it streams, and the path each viewer took, at
/// info. None of those targets logs an invitation, key or credential, and no
/// other target is written. One older copy is kept.
const HOST_LOG: &str = "service-host.log";
const HOST_LOG_BYTES: u64 = 1024 * 1024;

fn trace_to_host_log() {
    use tracing_subscriber::{Layer, filter::Targets, layer::SubscriberExt};
    let targets = Targets::new()
        .with_target("pb_mapper_desktop::session", tracing::Level::INFO)
        .with_target("pb_mapper_desktop::feedback", tracing::Level::INFO)
        .with_target("pb_mapper_client::direct", tracing::Level::INFO);
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(HostLog::default())
        .with_filter(targets);
    let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer));
}

/// `HOST_LOG`, opened at the first record and rotated once it would pass
/// `HOST_LOG_BYTES`. A record that cannot be written is dropped.
#[derive(Default)]
struct HostLog(parking_lot::Mutex<Option<(std::fs::File, u64)>>);
struct HostLogRecord<'a>(parking_lot::MutexGuard<'a, Option<(std::fs::File, u64)>>);
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for HostLog {
    type Writer = HostLogRecord<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        HostLogRecord(self.0.lock())
    }
}
impl std::io::Write for HostLogRecord<'_> {
    fn write(&mut self, record: &[u8]) -> std::io::Result<usize> {
        let slot = &mut *self.0;
        let open = match slot {
            Some(open) => open,
            None => slot.insert(os::open_service_log(HOST_LOG)?),
        };
        if open.1 > 0 && open.1 + record.len() as u64 > HOST_LOG_BYTES {
            // Closed before it is renamed.
            *slot = None;
            os::rotate_service_log(HOST_LOG)?;
        }
        let (file, len) = match slot {
            Some(open) => open,
            None => slot.insert(os::open_service_log(HOST_LOG)?),
        };
        file.write_all(record)?;
        *len += record.len() as u64;
        Ok(record.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
            ConsoleDisconnect | RemoteDisconnect | SessionLogoff | SessionTerminate
        ) {
            return;
        }
        let active = self.0.lock().unwrap_or_else(|_| std::process::abort());
        if let Some((session, cancel)) = active.as_ref()
            && *session == event.notification.session_id
        {
            // Ending/switching a login is sticky. Lock/unlock stays in the same
            // approved login, and the worker invalidates its desktop generation.
            cancel.cancel();
        }
    }
}
fn entry(_: Vec<OsString>) {
    trace_to_host_log();
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
        super::process_acl::allow_identity_query(&config.owner_sid)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let mut listener = pipe::bind(PIPE, Some(&config.owner_sid), true)?;
            status.set_service_status(state(ServiceState::Running, 0)).map_err(error)?;
            let restores = Restores::default();
            restore_left_modes(&config, &restores).await;
            let mut background = tokio::spawn(super::background::run(stop.clone()));
            let capture = async {
                loop {
                    tokio::select! { () = stop.cancelled() => break, result = listener.connect() => result? }
                    // Keep one live server instance while replacing the listener,
                    // so another process can never claim the pipe name in between.
                    let next = pipe::bind(PIPE, Some(&config.owner_sid), false)?;
                    let connection = std::mem::replace(&mut listener, next);
                    let _ = session(connection, &config, stop.clone(), &sessions, &restores).await;
                }
                Ok::<_, std::io::Error>(())
            };
            tokio::select! {
                result = capture => {
                    stop.cancel();
                    background.await.map_err(error)??;
                    result
                },
                result = &mut background => {
                    // A failed management/supervisor task must not leave SCM
                    // advertising a healthy but unreachable background host.
                    stop.cancel();
                    result.map_err(error)?
                },
            }
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
    restores: &Restores,
) -> Result<()> {
    let request: Request = tokio::time::timeout(TIMEOUT, ipc::read_json(&mut caller))
        .await
        .map_err(error)??;
    let Request::Start {
        display_id,
        display_device,
    } = request
    else {
        return Err(error("helper requires a start request"));
    };
    let (owner, session) = os::verify_client(&caller, config)?;
    let revoked = sessions.begin(session.session);
    session.check()?;
    let name = format!("{PIPE}.worker.{}", crate::security::secret()?);
    let mut private = pipe::bind(&name, None, true)?;
    let child = spawn_worker(
        config,
        session.session,
        &format!("worker {name} {}", std::process::id()),
    )?;
    tokio::time::timeout(TIMEOUT, private.connect())
        .await
        .map_err(error)??;
    let mut worker_pid = 0;
    unsafe {
        windows::Win32::System::Pipes::GetNamedPipeClientProcessId(
            pipe::raw(&private),
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
    if os::owner_session(&config.owner_sid)? != session {
        return Err(error("approved login ended during worker startup"));
    }
    let attached = Request::Attach {
        display_id,
        session: session.clone(),
        display_device,
    };
    // The session's journal passes from its restore run to this worker,
    // which replays it as it opens: the run ends only now, once the worker
    // is up and verified, never beside it. A worker that never attaches
    // hands the journal back to a restore run.
    restores.end(session.session).await;
    if let Err(failure) = tokio::time::timeout(TIMEOUT, ipc::write_json(&mut private, &attached))
        .await
        .map_err(error)
        .and_then(|written| written)
    {
        drop(child);
        restores.start(config, session.session).await;
        return Err(failure);
    }
    let watch = async {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            if !os::owner_session(&config.owner_sid).is_ok_and(|current| current == session)
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
    let exit = child.exit_code();
    drop(child); // Job close terminates a worker that failed to exit.
    // Killed, failed, or a display mode still journalled: put it back.
    if exit != Some(0) {
        restores.start(config, session.session).await;
    }
    Ok(())
}

/// The `restore` workers, at most one per console session, each putting
/// back the display mode a worker there left journalled. A worker never
/// acts beside one in its session: both act on the session's journal, so
/// the run ends, and is gone, just before a started and verified worker is
/// attached there, and starts again if that attach fails.
#[derive(Clone, Default)]
struct Restores(Arc<Mutex<Vec<Run>>>);
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
    async fn start(&self, config: &Configuration, session: u32) {
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
    async fn end(&self, session: u32) {
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
async fn restore_left_modes(config: &Configuration, restores: &Restores) {
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

struct Child {
    job: os::Handle,
    process: os::Handle,
    pid: u32,
}
impl Child {
    /// `None` while it runs.
    fn exit_code(&self) -> Option<u32> {
        let mut code = 0;
        unsafe { GetExitCodeProcess(self.process.raw(), &mut code) }.ok()?;
        (code != STILL_ACTIVE.0 as u32).then_some(code)
    }
    /// End it now, and wait until it is gone: two seconds at most.
    async fn end(self) {
        let _ = unsafe { TerminateJobObject(self.job.raw(), 1) };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while unsafe { WaitForSingleObject(self.process.raw(), 0) } == WAIT_TIMEOUT
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }
}
/// The helper in `session` with `args`, in a job that ends it with the job.
fn spawn_worker(config: &Configuration, session: u32, args: &str) -> Result<Child> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows_service::service::SessionNotification;

    #[test]
    fn lock_keeps_approved_login_but_logoff_and_switch_revoke_it() {
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
        assert!(!active.is_cancelled());
        sessions.changed(event(8, SessionChangeReason::SessionLogoff));
        assert!(!active.is_cancelled());
        sessions.changed(event(7, SessionChangeReason::ConsoleDisconnect));
        assert!(active.is_cancelled());
        sessions.changed(event(7, SessionChangeReason::SessionUnlock));
        assert!(active.is_cancelled());
        let replacement = sessions.begin(7);
        assert!(!replacement.is_cancelled());
        sessions.changed(event(7, SessionChangeReason::SessionLogoff));
        assert!(replacement.is_cancelled());
    }
}
