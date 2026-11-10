//! The Windows service: it serves capture sessions to the approved
//! application on the helper pipe, each through a worker in the owner's
//! console session, and runs the background host.
use super::{
    child::{Restores, restore_left_modes, spawn_worker},
    host_log::trace_to_host_log,
    *,
};
use crate::oop::proto::{Request, TIMEOUT};
use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio_util::sync::CancellationToken;
use windows::Win32::{Foundation::*, System::Threading::*};
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
            || os::current_sid()? != os::LOCAL_SYSTEM_SID
        {
            return Err(error("helper service identity is invalid"));
        }
        super::process_acl::allow_identity_query(&config.owner_sid)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let mut listener = pipe::bind(PIPE, Some(&config.owner_sid), true)?;
            status
                .set_service_status(state(ServiceState::Running, 0))
                .map_err(error)?;
            let restores = Restores::default();
            restore_left_modes(&config, &restores).await;
            let mut background = tokio::spawn(super::background::run(stop.clone()));
            let capture = async {
                loop {
                    tokio::select! {
                        biased;
                        () = stop.cancelled() => break,
                        result = listener.connect() => result?,
                    }
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
    child.wait_exit(Duration::from_secs(2)).await;
    let exit = child.exit_code();
    drop(child); // Job close terminates a worker that failed to exit.
    // Killed, failed, or a display mode still journalled: put it back.
    if exit != Some(0) {
        restores.start(config, session.session).await;
    }
    Ok(())
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
