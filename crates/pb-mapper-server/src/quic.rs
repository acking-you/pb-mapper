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
    net::IpAddr,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

/// Handshakes in flight, before any credential is known.
const HANDSHAKES: usize = 64;
/// Of those, how many may come from addresses not yet shown to receive what
/// is sent to them. A forged source address costs nothing to send from, so
/// past this a sender first gets a stateless retry: it costs the relay
/// nothing, and an honest client one round trip.
const UNVALIDATED_HANDSHAKES: usize = 8;
/// And how many one validated address may have in flight: enough for the
/// clients behind one NAT reconnecting at once, too few for one host to
/// take every slot by finishing handshakes and never authenticating.
const HANDSHAKES_PER_ADDRESS: usize = 8;
/// Authenticated connections the listener holds at once.
const CONNECTIONS: usize = 128;
/// Of those, how many one temporary credential may hold: a client pools one
/// per network it has been on and closes those it stops using, so this is
/// generous, and it keeps one tenant from taking every slot. The
/// administrator key, which operators share between their own machines, is
/// bounded by [`CONNECTIONS`] alone.
const CONNECTIONS_PER_CREDENTIAL: usize = 16;
/// Of [`CONNECTIONS`], how many only the administrator may take, so that
/// temporary credentials between them cannot lock it out.
const ADMIN_RESERVED: usize = 8;

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
        if !key.is_admin() && self.total.available_permits() < ADMIN_RESERVED {
            return Err("quic_connection_capacity");
        }
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

/// Handshakes in flight per validated address.
type PerAddress = Arc<StdMutex<HashMap<IpAddr, usize>>>;

/// Handshakes in flight: in all, from addresses not yet validated, and per
/// validated address.
#[derive(Clone)]
struct Handshakes {
    total: Arc<Semaphore>,
    unvalidated: Arc<Semaphore>,
    per_address: PerAddress,
}

/// What to do with an incoming handshake.
enum Claim {
    Accept(Handshake),
    /// Ask the sender to show it receives at its address first.
    Retry,
    Refuse,
}

/// One handshake's share of [`Handshakes`], returned when it ends, either way.
struct Handshake {
    _total: OwnedSemaphorePermit,
    _unvalidated: Option<OwnedSemaphorePermit>,
    address: Option<(IpAddr, PerAddress)>,
}

impl Handshakes {
    fn new() -> Self {
        Self {
            total: Arc::new(Semaphore::new(HANDSHAKES)),
            unvalidated: Arc::new(Semaphore::new(UNVALIDATED_HANDSHAKES)),
            per_address: Arc::default(),
        }
    }

    /// Only a validated address is counted as itself: an unvalidated one may
    /// be forged, and would spend someone else's share.
    fn claim(&self, validated: bool, may_retry: bool, ip: IpAddr) -> Claim {
        let unvalidated = if validated {
            None
        } else {
            match self.unvalidated.clone().try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) if may_retry => return Claim::Retry,
                Err(_) => return Claim::Refuse,
            }
        };
        let Ok(total) = self.total.clone().try_acquire_owned() else {
            return Claim::Refuse;
        };
        let address = if validated {
            let mut held = self.per_address.lock().unwrap_or_else(|e| e.into_inner());
            let held = held.entry(ip).or_default();
            if *held >= HANDSHAKES_PER_ADDRESS {
                return Claim::Refuse;
            }
            *held += 1;
            Some((ip, self.per_address.clone()))
        } else {
            None
        };
        Claim::Accept(Handshake {
            _total: total,
            _unvalidated: unvalidated,
            address,
        })
    }
}

impl Drop for Handshake {
    fn drop(&mut self) {
        let Some((ip, per_address)) = &self.address else {
            return;
        };
        let mut held = per_address.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = held.get_mut(ip) {
            *count -= 1;
            if *count == 0 {
                held.remove(ip);
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
        let handshakes = Handshakes::new();
        let admission = Admission::new();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
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
    Ok((Some(task), Some(capability)))
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
            Err(code) => {
                let failure = pb_mapper_auth::AuthFailure::new(
                    code,
                    if code == "quic_credential_capacity" {
                        "this credential has reached its relay QUIC connection limit"
                    } else {
                        "relay QUIC connection capacity reached"
                    },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Forged sources hold no more than their share before being asked to
    /// prove their address, and one proven address no more than its own:
    /// another still gets in, and a slot comes back when its handshake ends.
    #[test]
    fn no_one_source_takes_every_handshake_slot() {
        let handshakes = Handshakes::new();
        let host: IpAddr = [192, 0, 2, 1].into();
        let other: IpAddr = [192, 0, 2, 2].into();
        let accept = |claim| match claim {
            Claim::Accept(handshake) => handshake,
            _ => panic!("expected the handshake to be accepted"),
        };

        let forged: Vec<_> = (0..UNVALIDATED_HANDSHAKES)
            .map(|_| accept(handshakes.claim(false, true, host)))
            .collect();
        assert!(matches!(handshakes.claim(false, true, other), Claim::Retry));
        assert!(matches!(
            handshakes.claim(false, false, other),
            Claim::Refuse
        ));

        let mut held: Vec<_> = (0..HANDSHAKES_PER_ADDRESS)
            .map(|_| accept(handshakes.claim(true, false, host)))
            .collect();
        assert!(matches!(handshakes.claim(true, false, host), Claim::Refuse));
        let _other = accept(handshakes.claim(true, false, other));
        held.pop();
        let _again = accept(handshakes.claim(true, false, host));
        drop(forged);

        let total: Vec<_> = (0..HANDSHAKES)
            .map_while(
                |n| match handshakes.claim(true, false, [10, 0, (n / 8) as u8, 1].into()) {
                    Claim::Accept(handshake) => Some(handshake),
                    _ => None,
                },
            )
            .collect();
        assert_eq!(total.len() + held.len() + 2, HANDSHAKES);
        assert!(matches!(
            handshakes.claim(true, false, [10, 9, 9, 9].into()),
            Claim::Refuse
        ));
    }

    /// Temporary credentials between them leave the administrator its
    /// reserve.
    #[test]
    fn temporary_credentials_leave_the_administrator_a_reserve() {
        let admission = Admission::new();
        let mut admitted = Vec::new();
        let mut key = 1;
        while let Ok(slot) = admission.admit(KeyId::from_u64(key)) {
            admitted.push(slot);
            if admitted.len() % CONNECTIONS_PER_CREDENTIAL == 0 {
                key += 1;
            }
        }
        assert_eq!(admitted.len(), CONNECTIONS - ADMIN_RESERVED);
        let admin: Vec<_> = (0..ADMIN_RESERVED)
            .map(|_| admission.admit(pb_mapper_auth::ADMIN_KEY_ID).unwrap())
            .collect();
        assert_eq!(admin.len(), ADMIN_RESERVED);
    }
}
