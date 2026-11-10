//! Bounded QUIC admission. The routing manager handles the same authenticated
//! requests on either carrier; credentials are checked again on every stream.
use super::{ManagerTask, ManagerTaskSender, QuicServerConfig};
use crate::connection::log_auth_failure;
use pb_mapper_auth::AuthFailure;
use pb_mapper_protocol::{
    MessageWriter,
    command::{MessageSerializer, PbConnResponse, QuicEndpoint},
    quic::*,
    secure::{HeaderProtocol, ServerHeaderSession, ServerSecurity},
    transport::RelayStream,
};
use std::{io, sync::Arc, time::Duration};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

mod admission;

use admission::{Admission, Claim, Handshake, Handshakes};

struct Owner(quinn::Endpoint);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"relay shutdown");
    }
}
#[derive(Debug)]
struct ConnectionGuard(quinn::Connection);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"relay connection closed");
    }
}

/// A running QUIC listener: the task serving it, and the endpoint it
/// announces to clients.
pub(super) struct QuicListener {
    pub(super) task: AbortOnDropHandle<()>,
    pub(super) endpoint: Arc<QuicEndpoint>,
}

/// Listen for QUIC as `config` says, until `stop`. The task this returns
/// ends only once every connection task it started has: each holds the
/// authentication runtime, which a restart in the same process reopens.
pub(super) async fn start(
    config: Option<QuicServerConfig>,
    security: ServerSecurity,
    sender: ManagerTaskSender,
    stop: CancellationToken,
) -> io::Result<Option<QuicListener>> {
    let Some(config) = config else {
        return Ok(None);
    };
    let (tls, certificate) = server_config()?;
    let endpoint = quinn::Endpoint::server(tls, config.listen)?;
    let port = if config.advertise_port == 0 {
        endpoint.local_addr()?.port()
    } else {
        config.advertise_port
    };
    let capability = Arc::new(QuicEndpoint { port, certificate });
    tracing::info!(event = "relay_quic_listening", listen = %endpoint.local_addr()?, advertise_port = port);
    let task = AbortOnDropHandle::new(tokio::spawn(async move {
        let owner = Owner(endpoint);
        let handshakes = Handshakes::new();
        let admission = Admission::new();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                incoming = owner.0.accept() => {
                    let Some(incoming) = incoming else { break; };
                    // A handshake slot is held until the handshake ends.
                    let handshake = match handshakes.claim(
                        incoming.remote_address_validated(),
                        incoming.may_retry(),
                        incoming.remote_address().ip(),
                    ) {
                        Claim::Accept(handshake) => handshake,
                        Claim::Retry => {
                            let _ = incoming.retry();
                            continue;
                        }
                        Claim::Refuse => {
                            incoming.refuse();
                            continue;
                        }
                    };
                    let security = security.clone();
                    let sender = sender.clone();
                    let endpoint = owner.0.clone();
                    let admission = admission.clone();
                    tasks.spawn(handle_connection(incoming, endpoint, security, sender, admission, handshake));
                }
            }
        }
        tasks.shutdown().await;
        owner.0.close(0u32.into(), b"relay shutdown");
        let _ = tokio::time::timeout(Duration::from_secs(1), owner.0.wait_idle()).await;
    }));
    Ok(Some(QuicListener {
        task,
        endpoint: capability,
    }))
}

async fn handle_connection(
    incoming: quinn::Incoming,
    endpoint: quinn::Endpoint,
    security: ServerSecurity,
    sender: ManagerTaskSender,
    admission: Arc<Admission>,
    handshake: Handshake,
) {
    let admitted = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let connection = incoming.await.map_err(io::Error::other)?;
        let guard = ConnectionGuard(connection.clone());
        let peer_addr = connection.remote_address();
        let label = format!("quic:{}", connection.stable_id());
        let shared = Arc::new(QuicConnection {
            relay_addr: peer_addr,
            connection,
            endpoint,
        });
        let pair = shared
            .connection
            .accept_bi()
            .await
            .map_err(io::Error::other)?;
        let mut stream = QuicStream::new(shared.clone(), pair);
        let initial = match security.read_initial(&mut stream).await {
            Ok(initial) => initial,
            Err(error) => {
                log_auth_failure(&security, peer_addr, &label, &error);
                if let Some(session) = error.response_session {
                    reject(&mut stream, &session, &error.failure).await?;
                }
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    error.failure,
                ));
            }
        };
        if initial.session.protocol() != HeaderProtocol::V2
            || initial.payload != binding(&shared.connection)?
        {
            tracing::warn!(
                event = "auth_failed",
                auth_stage = "quic_binding",
                conn_id = %label,
                peer_addr = %peer_addr,
                key_id = initial.session.key_id().as_u64(),
                "QUIC first flight is not bound to this connection"
            );
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "QUIC channel binding mismatch",
            ));
        }
        let cancellation = initial
            .session
            .context()
            .map_err(io::Error::other)?
            .cancellation_token()
            .map_err(io::Error::other)?;
        let permit = match admission.admit(initial.session.key_id()) {
            Ok(permit) => permit,
            Err(failure) => {
                reject(&mut stream, &initial.session, &failure).await?;
                return Err(io::Error::other(failure));
            }
        };
        // Admitted: the full window, and the streams it may open.
        shared.connection.set_receive_window(RECEIVE_WINDOW.into());
        shared
            .connection
            .set_max_concurrent_bi_streams(MAX_STREAMS.into());
        initial
            .session
            .response_writer(&mut stream)
            .map_err(io::Error::other)?
            .write_msg(b"ready")
            .await
            .map_err(io::Error::other)?;
        Ok::<_, io::Error>((shared, guard, cancellation, permit))
    })
    .await;
    drop(handshake);
    let (shared, _guard, cancellation, _permit) = match admitted {
        Ok(Ok(ready)) => ready,
        Ok(Err(error)) => {
            tracing::debug!(%error, "QUIC admission rejected");
            return;
        }
        Err(_) => {
            tracing::debug!("QUIC admission timed out");
            return;
        }
    };
    tracing::info!(event = "relay_quic_authenticated", peer = %shared.connection.remote_address(), connection_id = shared.connection.stable_id());
    loop {
        let pair = tokio::select! {
            biased;
            () = cancellation.cancelled() => break,
            pair = shared.connection.accept_bi() => match pair { Ok(pair) => pair, Err(_) => break },
        };
        let task = ManagerTask::Accept {
            peer_addr: shared.connection.remote_address(),
            stream: RelayStream::Quic(Box::new(QuicStream::new(shared.clone(), pair))),
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => break,
            result = sender.send(task) => if result.is_err() { break; },
        }
    }
    if cancellation.is_cancelled() {
        // Per-stream authorization cancels data immediately. A root rotation
        // must still deliver its already-committed administrator response.
        // Stop accepting streams and give Quinn a bounded close drain.
        let _ = tokio::time::timeout(Duration::from_secs(3), shared.connection.closed()).await;
    }
}

/// The enclosing handshake timeout bounds this drain and keeps its admission
/// permit held. Closing the connection immediately would discard the refusal.
async fn reject(
    stream: &mut QuicStream,
    session: &ServerHeaderSession,
    failure: &AuthFailure,
) -> io::Result<()> {
    let response = PbConnResponse::error(&failure.code, &failure.message, failure.retryable)
        .encode()
        .map_err(io::Error::other)?;
    session
        .response_writer(stream)
        .map_err(io::Error::other)?
        .write_msg(&response)
        .await
        .map_err(io::Error::other)?;
    let (_, send) = stream.split();
    send.finish().map_err(io::Error::other)?;
    send.stopped().await.map_err(io::Error::other)?;
    Ok(())
}
