//! Bounded QUIC admission. The routing manager handles the same authenticated
//! requests on either carrier; credentials are checked again on every stream.
use super::{ManagerTask, ManagerTaskSender, QuicServerConfig};
use crate::connection::log_auth_failure;
use pb_mapper_auth::KeyId;
use pb_mapper_protocol::{
    MessageWriter,
    command::{MessageSerializer, PbConnResponse, QuicEndpoint},
    quic::*,
    secure::{HeaderProtocol, ServerHeaderSession, ServerSecurity},
    transport::RelayStream,
};
use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

/// Handshakes in flight, before any credential is known.
const HANDSHAKES: usize = 64;
/// Authenticated connections the listener holds at once.
const CONNECTIONS: usize = 128;
/// Of those, how many one temporary credential may hold: a client pools one
/// per network it has been on and closes those it stops using, so this is
/// generous, and it keeps one tenant from taking every slot. The
/// administrator key, which operators share between their own machines, is
/// bounded by [`CONNECTIONS`] alone.
const CONNECTIONS_PER_CREDENTIAL: usize = 16;

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

/// Authenticated connections held, in all and per temporary credential.
struct Admission {
    total: Arc<Semaphore>,
    per_key: StdMutex<HashMap<KeyId, usize>>,
}

/// One admitted connection's share of [`Admission`], returned on drop.
struct Admitted {
    _total: OwnedSemaphorePermit,
    key: KeyId,
    admission: Arc<Admission>,
}

impl Admission {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            total: Arc::new(Semaphore::new(CONNECTIONS)),
            per_key: StdMutex::new(HashMap::new()),
        })
    }

    /// A slot for `key`, or the code of the limit it reached.
    fn admit(self: &Arc<Self>, key: KeyId) -> Result<Admitted, &'static str> {
        let total = self
            .total
            .clone()
            .try_acquire_owned()
            .map_err(|_| "quic_connection_capacity")?;
        if !key.is_admin() {
            let mut per_key = self.per_key.lock().unwrap_or_else(|e| e.into_inner());
            let held = per_key.entry(key).or_default();
            if *held >= CONNECTIONS_PER_CREDENTIAL {
                return Err("quic_credential_capacity");
            }
            *held += 1;
        }
        Ok(Admitted {
            _total: total,
            key,
            admission: self.clone(),
        })
    }
}

impl Drop for Admitted {
    fn drop(&mut self) {
        if self.key.is_admin() {
            return;
        }
        let mut per_key = self
            .admission
            .per_key
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(held) = per_key.get_mut(&self.key) {
            *held -= 1;
            if *held == 0 {
                per_key.remove(&self.key);
            }
        }
    }
}

/// Listen for QUIC as `config` says, until `stop`. The task this returns
/// ends only once every connection task it started has: each holds the
/// authentication runtime, which a restart in the same process reopens.
pub(super) async fn start(
    config: Option<QuicServerConfig>,
    security: ServerSecurity,
    sender: ManagerTaskSender,
    stop: CancellationToken,
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
        let handshakes = Arc::new(Semaphore::new(HANDSHAKES));
        let admission = Admission::new();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                () = stop.cancelled() => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                incoming = owner.0.accept() => {
                    let Some(incoming) = incoming else { break; };
                    // A handshake slot is held until the handshake ends, and a
                    // forged source address costs nothing to send from. Once
                    // half the slots are taken, a sender first shows that it
                    // receives at its address: a stateless retry costs the
                    // relay nothing, and an honest client one round trip.
                    if !incoming.remote_address_validated()
                        && handshakes.available_permits() <= HANDSHAKES / 2
                    {
                        let _ = incoming.retry();
                        continue;
                    }
                    let Ok(handshake) = handshakes.clone().try_acquire_owned() else { incoming.refuse(); continue; };
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
    Ok((Some(task), Some(capability)))
}

async fn handle_connection(
    incoming: quinn::Incoming,
    endpoint: quinn::Endpoint,
    security: ServerSecurity,
    sender: ManagerTaskSender,
    admission: Arc<Admission>,
    handshake: tokio::sync::OwnedSemaphorePermit,
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
            Err(code) => {
                let failure = pb_mapper_auth::AuthFailure::new(
                    code,
                    "relay QUIC connection capacity reached",
                    true,
                );
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
        let _ = tokio::time::timeout(Duration::from_secs(3), shared.connection.closed()).await;
    }
}

/// The enclosing handshake timeout bounds this drain and keeps its admission
/// permit held. Closing the connection immediately would discard the refusal.
async fn reject(
    stream: &mut QuicStream,
    session: &ServerHeaderSession,
    failure: &pb_mapper_auth::AuthFailure,
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
