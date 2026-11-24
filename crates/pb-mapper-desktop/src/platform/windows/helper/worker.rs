//! The privileged capture worker the service starts in the owner's console
//! session, and the `restore` worker that puts back a display mode one left.
use super::*;
use crate::{
    Backend, Waker,
    modes::Journal,
    oop::{
        proto::{Request, Response, TIMEOUT},
        server::{Outgoing, ServeState, exchange, work},
    },
    scheduler::{Waiter, Wake},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

/// What the capture thread may have handed the exchange that it has not
/// written yet: the units the encoder holds, an answer, a fresh picture and
/// a pointer, with room to spare. A full queue holds the capture thread
/// until the pipe takes more, and a pipe that takes nothing for 3 s ends
/// the exchange.
const OUTGOING_DEPTH: usize = 16;
/// How often a `restore` worker tries again, and for how long at most.
const RESTORE_RETRY: std::time::Duration = std::time::Duration::from_secs(2);
pub(super) const RESTORE_FOR: std::time::Duration = std::time::Duration::from_secs(600);

pub fn run(name: &str, parent: u32) -> Result<()> {
    if !name.starts_with(&format!("{PIPE}.worker."))
        || name.len() > 160
        || os::current_sid()? != os::LOCAL_SYSTEM_SID
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
        let (requests, incoming) = mpsc::sync_channel::<Request>(4);
        let (out, mut outgoing) = tokio::sync::mpsc::channel(OUTGOING_DEPTH);
        // The capture thread's one wake: a request handed to it, a picture
        // its encoder's capture thread published, its encoder's output.
        let wake = Wake::new();
        let stop = Arc::new(AtomicBool::new(false));
        let channels = Channels {
            incoming,
            out,
            wake: wake.clone(),
            stopped: stop.clone(),
        };
        let (device, replayed) = (display_device.clone(), journal.clone());
        let join = std::thread::spawn(move || {
            serve_bound(display_id, device, session, replayed, &channels)
        });
        let exchange = exchange(
            &mut pipe,
            display_id,
            display_device,
            |request| {
                requests.try_send(request).map_err(error)?;
                wake.notify();
                Ok(())
            },
            &mut outgoing,
        )
        .await;
        stop.store(true, Ordering::Release);
        wake.notify();
        drop(requests);
        // A capture thread held on a full queue lets go.
        drop(outgoing);
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

/// What the capture thread's loop talks over: the requests the exchange
/// hands it, what it hands the exchange to write, the wake the exchange and
/// its encoder share, and whether the exchange ended.
struct Channels {
    incoming: mpsc::Receiver<Request>,
    out: tokio::sync::mpsc::Sender<Outgoing>,
    wake: Wake,
    stopped: Arc<AtomicBool>,
}

/// The capture thread: opens the backend bound to `session`'s input desktop
/// on display `display_id` (or its `device`), which replays `journal`, and
/// runs the worker's loop (`server::work`) until the exchange ends or the
/// host asks to close. The loop waits on the wake that the exchange and the
/// backend's encoder share, so a request, a published picture and a
/// finished unit are each taken as they come, and nothing is polled.
fn serve_bound(
    display_id: u32,
    device: Option<String>,
    session: os::SessionIdentity,
    journal: Journal,
    channels: &Channels,
) {
    let Channels {
        incoming,
        out,
        wake,
        stopped,
    } = channels;
    // Opening replays the journal: what a killed worker left changed.
    let opened = super::super::open_bound(display_id, device, session, journal)
        .and_then(|backend| Ok((backend, Waiter::new(wake)?)));
    let (mut backend, waiter) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            if incoming.recv_timeout(TIMEOUT).is_ok() {
                let _ = out.blocking_send(Outgoing::Reply {
                    response: Box::new(Response::failed(&e, 0)),
                    image: vec![],
                    presented: None,
                });
            }
            return;
        }
    };
    // The encoder's capture thread and event watcher wake this loop.
    backend.bind(Waker(wake.clone()));
    // For a viewer that decodes H.265, the worker names the GPU it captures
    // on, under which the host keeps this worker's H.265 answer for the next
    // worker.
    let mut state = ServeState::new(super::super::video::named_adapter);
    work(&mut backend, &mut state, incoming, out, &waiter, &|| {
        stopped.load(Ordering::Acquire)
    });
    backend.release();
    // The backend drops here, on its desktop: it puts back what it
    // changed, or leaves it journalled.
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
    if os::current_sid()? != os::LOCAL_SYSTEM_SID || os::session(std::process::id())? != session {
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
