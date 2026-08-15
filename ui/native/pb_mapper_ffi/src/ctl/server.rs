//! The listener that lives inside the running UI process.
//!
//! Spawned onto the runtime `PbMapperHandle` already owns, holding a clone of
//! the same state the FFI mutates — so a command from a terminal and a click in
//! the window go through exactly the same code.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::ctl::endpoint;
use crate::ctl::proto::{self, Request, Response, PROTOCOL_VERSION};
use crate::ctl::{dispatch, Origin};
use crate::error::CtlError;
use crate::state::PbMapperState;

/// Serve one connection: read a request, run it, write the answer, close.
async fn serve_one<S>(state: Arc<Mutex<PbMapperState>>, mut stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request: Request = match proto::read_frame(&mut stream).await {
        Ok(request) => request,
        Err(e) => {
            tracing::debug!("control: unreadable request: {e}");
            let response = Response::err(&CtlError::invalid_argument(format!(
                "could not read the request: {e}"
            )));
            let _ = proto::write_frame(&mut stream, &response).await;
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
        let _ = proto::write_frame(&mut stream, &response).await;
        return;
    }

    let id = request.id.clone();
    let response = dispatch(&state, request.command, Origin::Cli)
        .await
        .with_id(id);
    if let Err(e) = proto::write_frame(&mut stream, &response).await {
        tracing::debug!("control: could not answer: {e}");
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
pub async fn request(command: crate::ctl::Command) -> std::io::Result<Response> {
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
