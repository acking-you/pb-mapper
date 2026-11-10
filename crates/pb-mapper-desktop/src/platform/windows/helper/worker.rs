//! The privileged capture worker the service starts in the owner's console
//! session, and the `restore` worker that puts back a display mode one left.
use super::*;
use crate::{
    Backend,
    modes::Journal,
    oop::{
        proto::{Request, Response, TIMEOUT},
        server::{ServeState, exchange, serve},
    },
};
use std::sync::mpsc;

/// A host that asked for no video this long is paused or gone: the worker
/// gives its fine timers back.
const VIDEO_IDLE: std::time::Duration = std::time::Duration::from_secs(1);
/// How often a `restore` worker tries again, and for how long at most.
const RESTORE_RETRY: std::time::Duration = std::time::Duration::from_secs(2);
pub(super) const RESTORE_FOR: std::time::Duration = std::time::Duration::from_secs(600);

pub fn run(name: &str, parent: u32) -> Result<()> {
    if !name.starts_with(&format!("{PIPE}.worker."))
        || name.len() > 160
        || os::current_sid()? != "S-1-5-18"
    {
        return Err(error("invalid worker launch"));
    }
    let config = configuration()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let journal = runtime.block_on(async {
        let mut pipe = pipe::connect(name)?;
        os::verify_server(&pipe, &config.helper_executable, Some(parent))?;
        let Request::Attach {
            display_id,
            session,
            display_device,
        } = tokio::time::timeout(TIMEOUT, ipc::read_json(&mut pipe))
            .await
            .map_err(error)??
        else {
            return Err(error("missing worker start"));
        };
        // Only ever the protected one of this console session: no caller
        // names where it is.
        let journal = journal(session.session)?;
        let (requests, incoming) =
            mpsc::sync_channel::<(Request, tokio::sync::oneshot::Sender<(Response, Vec<u8>)>)>(4);
        let cancel = tokio_util::sync::CancellationToken::new();
        let stopped = cancel.clone();
        let (device, replayed) = (display_device.clone(), journal.clone());
        let join = std::thread::spawn(move || {
            // Opening replays the journal: what a killed worker left changed.
            let mut backend = match super::super::open_bound(display_id, device, session, replayed)
            {
                Ok(backend) => backend,
                Err(e) => {
                    if let Ok((_, reply)) = incoming.recv_timeout(TIMEOUT) {
                        let _ = reply.send((Response::failed(&e, 0), vec![]));
                    }
                    return;
                }
            };
            let mut state = ServeState::new();
            // Fine process timers while the host asks for video; a paused
            // host asks for none.
            let mut timer = None;
            let mut last_video = std::time::Instant::now();
            while !stopped.is_cancelled() {
                if state.last_input.elapsed() > std::time::Duration::from_secs(2) {
                    backend.release();
                }
                if last_video.elapsed() > VIDEO_IDLE {
                    timer = None;
                }
                let (request, reply) =
                    match incoming.recv_timeout(std::time::Duration::from_millis(15)) {
                        Ok(request) => request,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                if stopped.is_cancelled() {
                    break;
                }
                if matches!(request, Request::Video { .. }) {
                    last_video = std::time::Instant::now();
                    timer.get_or_insert_with(crate::timer::TimerResolution::hold);
                }
                let close = matches!(request, Request::Close {});
                let result = serve(&mut backend, request, &mut state);
                let pending = u32::try_from(backend.pending_video()).unwrap_or(u32::MAX);
                let response = match result {
                    Ok((mut response, image)) => {
                        response.pending = pending;
                        (response, image)
                    }
                    Err(e) => {
                        backend.release();
                        (Response::failed(&e, pending), vec![])
                    }
                };
                let _ = reply.send(response);
                if close {
                    break;
                }
            }
            backend.release();
            // The backend drops here, on its desktop: it puts back what it
            // changed, or leaves it journalled.
        });
        let exchange = exchange(&mut pipe, display_id, display_device, |request| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let sent = requests.try_send((request, tx)).map_err(error);
            async move {
                sent?;
                rx.await.map_err(error)
            }
        })
        .await;
        cancel.cancel();
        drop(requests);
        let _ = join.join();
        exchange.map(|()| journal)
    })?;
    if journal.pending() {
        // A display mode could not be put back: the service starts the
        // `restore` worker for it.
        std::process::exit(RESTORE_PENDING as i32);
    }
    Ok(())
}

/// This console session's display-mode journal, in the helper's protected
/// directory.
pub(super) fn journal(session: u32) -> Result<Journal> {
    Ok(Journal::at(
        os::install_root()?.join(format!("display-modes-{session}.json")),
    ))
}

/// The worker the service starts when one ended with a display mode it
/// changed still journalled: it puts that back from the session's protected
/// journal, again every 2 s while the ordinary desktop is away, for at most
/// ten minutes, then exits. A logoff or a reboot undoes a dynamic mode
/// anyway.
pub fn restore(session: u32) -> Result<()> {
    if os::current_sid()? != "S-1-5-18" || os::session(std::process::id())? != session {
        return Err(error("invalid restore launch"));
    }
    let journal = journal(session)?;
    let until = std::time::Instant::now() + RESTORE_FOR;
    loop {
        if let Err(failure) = crate::modes::recover(&mut super::super::modes::WinModes, &journal) {
            os::record_service_event("display_mode_not_restored", &failure.to_string());
        }
        if !journal.pending() || std::time::Instant::now() >= until {
            return Ok(());
        }
        std::thread::sleep(RESTORE_RETRY);
    }
}
