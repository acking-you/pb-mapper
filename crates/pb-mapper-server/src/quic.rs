//! Bounded QUIC admission. The routing manager handles the same authenticated
//! requests on either carrier; credentials are checked again on every stream.
use super::{ManagerTask, ManagerTaskSender, QuicServerConfig};
use pb_mapper_protocol::{
    MessageWriter,
    command::QuicEndpoint,
    quic::*,
    secure::{HeaderProtocol, ServerSecurity},
    transport::RelayStream,
};
use std::{io, sync::Arc};
use tokio::sync::Semaphore;
use tokio_util::task::AbortOnDropHandle;

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

pub(super) async fn start(
    config: Option<QuicServerConfig>,
    security: ServerSecurity,
    sender: ManagerTaskSender,
) -> io::Result<(Option<AbortOnDropHandle<()>>, Option<Arc<QuicEndpoint>>)> {
    let Some(config) = config else {
        return Ok((None, None));
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
        let handshakes = Arc::new(Semaphore::new(64));
        let connections = Arc::new(Semaphore::new(128));
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                incoming = owner.0.accept() => {
                    let Some(incoming) = incoming else { break; };
                    let Ok(handshake) = handshakes.clone().try_acquire_owned() else { incoming.refuse(); continue; };
                    let security = security.clone();
                    let sender = sender.clone();
                    let endpoint = owner.0.clone();
                    let connections = connections.clone();
                    tasks.spawn(handle_connection(incoming, endpoint, security, sender, connections, handshake));
                }
            }
        }
    }));
    Ok((Some(task), Some(capability)))
}

async fn handle_connection(
    incoming: quinn::Incoming,
    endpoint: quinn::Endpoint,
    security: ServerSecurity,
    sender: ManagerTaskSender,
    connections: Arc<Semaphore>,
    handshake: tokio::sync::OwnedSemaphorePermit,
) {
    let admitted = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let connection = incoming.await.map_err(io::Error::other)?;
        let guard = ConnectionGuard(connection.clone());
        let shared = Arc::new(QuicConnection {
            relay_addr: connection.remote_address(),
            connection,
            endpoint,
        });
        let pair = shared
            .connection
            .accept_bi()
            .await
            .map_err(io::Error::other)?;
        let mut stream = QuicStream::new(shared.clone(), pair);
        let initial = security
            .read_initial(&mut stream)
            .await
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.failure))?;
        if initial.session.protocol() != HeaderProtocol::V2
            || initial.payload != binding(&shared.connection)?
        {
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
        let permit = connections.try_acquire_owned().map_err(io::Error::other)?;
        initial
            .session
            .response_writer(&mut stream)
            .map_err(io::Error::other)?
            .write_msg(b"ready")
            .await
            .map_err(io::Error::other)?;
        shared
            .connection
            .set_max_concurrent_bi_streams(MAX_STREAMS.into());
        Ok::<_, io::Error>((shared, guard, cancellation, permit))
    })
    .await;
    drop(handshake);
    let (shared, _guard, cancellation, _permit) = match admitted {
        Ok(Ok(ready)) => ready,
        result => {
            tracing::debug!(?result, "QUIC admission rejected");
            return;
        }
    };
    tracing::info!(event = "relay_quic_authenticated", peer = %shared.connection.remote_address(), connection_id = shared.connection.stable_id());
    loop {
        let pair = tokio::select! {
            () = cancellation.cancelled() => break,
            pair = shared.connection.accept_bi() => match pair { Ok(pair) => pair, Err(_) => break },
        };
        let task = ManagerTask::Accept {
            peer_addr: shared.connection.remote_address(),
            stream: RelayStream::Quic(Box::new(QuicStream::new(shared.clone(), pair))),
        };
        tokio::select! {
            () = cancellation.cancelled() => break,
            result = sender.send(task) => if result.is_err() { break; },
        }
    }
    if cancellation.is_cancelled() {
        // Per-stream authorization cancels data immediately. A root rotation
        // must still deliver its already-committed administrator response.
        // Stop accepting streams and give Quinn a bounded close drain.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            shared.connection.closed(),
        )
        .await;
    }
}
