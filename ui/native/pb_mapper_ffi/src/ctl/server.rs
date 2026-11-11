//! The listener that lives inside the running UI process.
//!
//! Spawned onto the runtime `PbMapperHandle` already owns, holding a clone of
//! the same state the FFI mutates — so a command from a terminal and a click in
//! the window go through exactly the same code.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::ctl::endpoint;
use crate::ctl::proto::{self, FrameError, PROTOCOL_VERSION, Request, Response};
use crate::ctl::{Origin, dispatch};
use crate::error::CtlError;
use crate::state::PbMapperState;

/// How long a peer has to send its request once connected, and to take its
/// answer once written. The command line does each at once, so one silent
/// for this long never will, and its connection's task ends rather than
/// waiting on it for as long as the app runs. The command itself takes as
/// long as it takes.
const PEER_WAIT: Duration = Duration::from_secs(10);

/// Serve one connection: read a request, run it, write the answer, close.
async fn serve_one<S>(state: Arc<Mutex<PbMapperState>>, mut stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request: Request =
        match tokio::time::timeout(PEER_WAIT, proto::read_frame(&mut stream)).await {
            Ok(Ok(request)) => request,
            Ok(Err(e)) => {
                tracing::debug!("control: unreadable request: {e}");
                let response = Response::err(&CtlError::invalid_argument(format!(
                    "could not read the request: {e}"
                )));
                answer(&mut stream, &response).await;
                return;
            }
            Err(_) => {
                tracing::debug!("control: no request within {PEER_WAIT:?}");
                return;
            }
        };

    // Version before command, so a mismatch is reported as itself rather than
    // as an unknown field somewhere inside.
    if request.v > PROTOCOL_VERSION {
        let response = Response::err(&CtlError::invalid_argument(format!(
            "this pb-mapper UI speaks control protocol v{PROTOCOL_VERSION}, \
             the caller asked for v{}. Restart the UI to pick up the newer build.",
            request.v
        )))
        .with_id(request.id);
        answer(&mut stream, &response).await;
        return;
    }

    let id = request.id.clone();
    let response = dispatch(&state, request.command, Origin::Cli)
        .await
        .with_id(id);
    answer(&mut stream, &response).await;
}

/// Writes `response`, for at most [`PEER_WAIT`].
async fn answer<S: AsyncWrite + Unpin>(stream: &mut S, response: &Response) {
    match tokio::time::timeout(PEER_WAIT, proto::write_frame(stream, response)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::debug!("control: could not answer: {e}"),
        Err(_) => tracing::debug!("control: the answer was not taken within {PEER_WAIT:?}"),
    }
}

/// Start listening. Returns an error if the endpoint cannot be claimed, which
/// the caller should treat as "run as a window only" rather than as fatal — a
/// GUI that refuses to open because a pipe is unavailable would be a poor
/// trade.
pub fn start(
    state: Arc<Mutex<PbMapperState>>,
    shutdown: CancellationToken,
) -> std::io::Result<String> {
    let name = endpoint::endpoint();

    #[cfg(windows)]
    {
        // A named pipe serves one client per instance, so the loop hands the
        // connected instance off and immediately creates the next.
        let mut server = endpoint::bind_first(&name)?;
        let loop_name = name.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    result = server.connect() => {
                        if let Err(e) = result {
                            tracing::warn!("control: accept failed: {e}");
                            break;
                        }
                        let connected = std::mem::replace(
                            &mut server,
                            match endpoint::bind_next(&loop_name) {
                                Ok(next) => next,
                                Err(e) => {
                                    tracing::error!("control: could not open the next pipe instance: {e}");
                                    break;
                                }
                            },
                        );
                        tokio::spawn(serve_one(state.clone(), connected));
                    }
                }
            }
            tracing::info!("control server stopped");
        });
    }

    #[cfg(unix)]
    {
        let listener = endpoint::bind_first(&name)?;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    result = listener.accept() => match result {
                        Ok((stream, _)) => {
                            tokio::spawn(serve_one(state.clone(), stream));
                        }
                        Err(e) => {
                            tracing::warn!("control: accept failed: {e}");
                            break;
                        }
                    },
                }
            }
            tracing::info!("control server stopped");
        });
    }

    tracing::info!("control server listening on {name}");
    Ok(name)
}

/// Send one command to a running UI and read its answer.
pub async fn request(command: crate::ctl::Command) -> Result<Response, FrameError> {
    let name = endpoint::endpoint();
    let mut stream = endpoint::connect(&name).await?;
    let request = Request {
        v: PROTOCOL_VERSION,
        id: None,
        command,
    };
    proto::write_frame(&mut stream, &request).await?;
    proto::read_frame(&mut stream).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ctl::Command;

    /// A peer that connects and never sends a request, or never reads its
    /// answer, holds the connection's task only for a bounded while: the
    /// command line sends at once and reads at once, so nothing else is
    /// waited for.
    #[tokio::test(start_paused = true)]
    async fn a_peer_that_goes_quiet_is_let_go() {
        let state = Arc::new(Mutex::new(PbMapperState::unloaded()));
        let served = |stream| {
            tokio::time::timeout(Duration::from_secs(600), serve_one(state.clone(), stream))
        };

        let (_silent, stream) = tokio::io::duplex(64);
        served(stream)
            .await
            .expect("a request that never comes is not waited for");

        // An answer larger than the channel holds, which is never read.
        let (mut deaf, stream) = tokio::io::duplex(64);
        let request = Request {
            v: PROTOCOL_VERSION,
            id: Some("x".repeat(256)),
            command: Command::Hello,
        };
        let asking = async move {
            let _ = proto::write_frame(&mut deaf, &request).await;
            std::future::pending::<()>().await;
        };
        tokio::select! {
            served = served(stream) => served.expect("an answer never read is not waited for"),
            () = asking => unreachable!("the asking side never ends"),
        }
    }
}
