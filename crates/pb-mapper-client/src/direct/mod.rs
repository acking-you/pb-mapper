//! Client-owned direct sessions. Control sockets never own an established data task.
mod candidates;
mod control;
mod endpoint;
mod identity;
#[cfg(feature = "port-mapping")]
mod mapping;
mod provider;
mod reports;
mod socket;

use crate::{
    diagnostics::{Diagnostics, DirectStage},
    endpoint::RelayEndpoint,
};
use control::{Control, io_error};
use endpoint::Network;
use pb_mapper_core::checksum::Credential;
use pb_mapper_protocol::{
    command::{PbConnRequest, PbConnStatusReq, PbConnStatusResp},
    data::DataCodec,
    direct::*,
    forward::StreamForward,
};
pub(crate) use provider::Provider;
use std::{io, sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, OnceCell},
    time::Instant,
};
use uni_stream::stream::NetworkStream;

/// First pause after a direct setup falls back to the relay.
const COOLDOWN_INITIAL: Duration = Duration::from_secs(10);
/// Longest pause between direct attempts on a network that keeps refusing them.
const COOLDOWN_MAX: Duration = Duration::from_secs(300);

/// When a tunnel may try direct again after falling back.
///
/// A path that cannot be punched — a symmetric NAT, blocked UDP, a relay or
/// publisher without direct — fails the same way for every connection, and
/// each failure costs that connection its whole setup budget before the relay
/// takes over. Pausing direct after a fallback sends the next connections
/// straight to the relay instead. The pause doubles while fallbacks repeat; a
/// direct success or a network change, which can make a path punchable,
/// clears it.
#[derive(Debug, Default)]
struct Cooldown {
    until: Option<Instant>,
    next: Duration,
}

impl Cooldown {
    fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// Start the next pause, unless one is running. A burst of connections
    /// that were all mid-attempt when the path failed is one failure, not one
    /// per connection: counting each would jump straight to the cap.
    fn back_off(&mut self, now: Instant) {
        if self.active(now) {
            return;
        }
        let pause = if self.next.is_zero() {
            COOLDOWN_INITIAL
        } else {
            self.next
        };
        self.until = Some(now + pause);
        self.next = (pause * 2).min(COOLDOWN_MAX);
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

pub(crate) struct Consumer {
    remote: RelayEndpoint,
    credential: Credential,
    namespace: Option<u64>,
    is_datagram: bool,
    keep_alive: bool,
    control: Arc<Mutex<Option<Arc<Control>>>>,
    reports: Arc<reports::Latest>,
    report_wake: Arc<tokio::sync::Notify>,
    _reporter: tokio_util::task::AbortOnDropHandle<()>,
    network: OnceCell<Arc<Network>>,
    diagnostics: Diagnostics,
    cooldown: std::sync::Mutex<Cooldown>,
    port_mapping: bool,
}
impl Consumer {
    pub fn new(
        remote: RelayEndpoint,
        credential: Credential,
        namespace: Option<u64>,
        is_datagram: bool,
        keep_alive: bool,
        diagnostics: Diagnostics,
        port_mapping: bool,
    ) -> Arc<Self> {
        let control = Arc::new(Mutex::new(None::<Arc<Control>>));
        let reports = Arc::new(reports::Latest::default());
        let report_wake = Arc::new(tokio::sync::Notify::new());
        let reporter = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(report_loop(
            remote.clone(),
            credential,
            namespace,
            control.clone(),
            reports.clone(),
            report_wake.clone(),
        )));
        Arc::new(Self {
            remote,
            credential,
            namespace,
            is_datagram,
            keep_alive,
            control,
            reports,
            report_wake,
            _reporter: reporter,
            network: OnceCell::new(),
            diagnostics,
            cooldown: std::sync::Mutex::default(),
            port_mapping,
        })
    }
    fn cooldown(&self) -> std::sync::MutexGuard<'_, Cooldown> {
        self.cooldown.lock().unwrap_or_else(|e| e.into_inner())
    }
    async fn control(&self, key: &str) -> io::Result<Option<Arc<Control>>> {
        let mut slot = self.control.lock().await;
        if let Some(control) = slot.as_ref().filter(|c| c.alive()) {
            return Ok(Some(control.clone()));
        }
        // Concurrent setups queue here. If the one ahead learned that direct
        // is unavailable, the rest go to the relay instead of asking again.
        if self.cooldown().active(Instant::now()) {
            return Ok(None);
        }
        let mut stream = self.remote.connect(self.credential, &mut None).await?;
        let response = crate::client::status::get_status_with_credential(
            &mut stream,
            PbConnStatusReq::Service {
                key: key.into(),
                direct_query: Some(true),
            },
            self.namespace,
            &self.credential,
        )
        .await
        .map_err(io_error)?;
        let PbConnStatusResp::Service {
            direct: Some(capability),
            ..
        } = response
        else {
            *slot = None;
            // Set before the lock is released, so no queued setup repeats the query.
            self.cooldown().back_off(Instant::now());
            return Ok(None);
        };
        if capability.version != DIRECT_VERSION {
            self.cooldown().back_off(Instant::now());
            return Ok(None);
        }
        let control = Control::connect(&self.remote, self.credential, self.namespace).await?;
        *slot = Some(control.clone());
        Ok(Some(control))
    }
    pub fn invalidate(&self) {
        self.cooldown().clear();
        if let Some(network) = self.network.get() {
            network.invalidate();
        }
    }
    pub async fn stop(&self) {
        if let Some(network) = self.network.get() {
            network.shutdown().await;
        }
    }
    pub async fn prepare(
        self: &Arc<Self>,
        key: &str,
        deadline: Instant,
    ) -> io::Result<Option<Prepared>> {
        // A recent fallback says this path will not punch: take the ordinary
        // relay path without spending the connection's setup budget on it.
        if self.cooldown().active(Instant::now()) {
            return Ok(None);
        }
        // Before a grant arrives no endpoint can have committed, so a failed
        // capability/control/discovery attempt can use the ordinary relay path.
        let mut attempt = self.diagnostics.direct_attempt();
        let negotiation = async {
            let Some(control) = self.control(key).await? else {
                return Ok(None);
            };
            let network = self
                .network
                .get_or_try_init(|| async {
                    Network::new(false, self.port_mapping.then(|| self.diagnostics.clone()))
                })
                .await?
                .clone();
            attempt.stage(DirectStage::Discovering);
            let peer = network.peer(control.capability.stun_addr).await;
            let Some(grant) = control.open(key.into(), self.is_datagram, peer).await? else {
                return Ok(None);
            };
            Ok::<_, io::Error>(Some((control, network, grant)))
        };
        let Ok(Ok(Some((control, network, grant)))) = tokio::time::timeout_at(
            deadline.min(Instant::now() + Duration::from_secs(3)),
            negotiation,
        )
        .await
        else {
            attempt.fell_back("direct_negotiation_unavailable");
            self.cooldown().back_off(Instant::now());
            return Ok(None);
        };
        let deadline = deadline.min(Instant::now() + Duration::from_millis(grant.ttl_ms));
        if grant.service != key
            || grant.is_datagram != self.is_datagram
            || grant.consumer_key != self.credential.key_id()
            || grant.namespace != self.namespace.unwrap_or(self.credential.key_id())
            || grant.ttl_ms == 0
            || grant.ttl_ms > SETUP_TTL_MS
            || grant.consumer.certificate != network.identity.certificate
            || !grant.provider.valid()
        {
            control.cancel(grant.id);
            return Err(io::Error::other("invalid direct session grant"));
        }
        network.authorize(&grant, false, deadline)?;
        let mut pending = PendingSession {
            network: network.clone(),
            control: control.clone(),
            id: grant.id,
            completed: false,
        };
        attempt.stage(DirectStage::Punching);
        let direct_deadline = deadline.min(Instant::now() + Duration::from_secs(2));
        let candidate = tokio::time::timeout_at(direct_deadline, async {
            let connection = network.connect(&grant, direct_deadline).await?;
            tracing::debug!(id=?grant.id, peer=%connection.remote_address(), "direct TLS connected");
            let guard = QuicGuard(connection);
            let (mut send, mut recv) = guard.0.open_bi().await.map_err(io_error)?;
            send.write_all(&grant.id).await.map_err(io_error)?;
            send.write_all(&endpoint::binding(&guard.0, &grant)?)
                .await
                .map_err(io_error)?;
            if recv.read_u8().await? != 1 {
                return Err(io::Error::other("direct path was not ready"));
            }
            Ok::<_, io::Error>((guard, send, recv))
        })
        .await;
        if let Ok(Ok((connection, mut send, mut recv))) = candidate {
            // From this point on, timeout/cancellation/ACK loss MUST NOT select relay.
            pending.completed = true;
            attempt.stage(DirectStage::Committing);
            send.write_all(&[2]).await.map_err(io_error)?;
            if recv.read_u8().await? != 3 {
                return Err(io::Error::other(
                    "provider could not open the local service",
                ));
            }
            let framing_key = endpoint::framing_key(&connection.0, &grant)?;
            attempt.established();
            self.cooldown().clear();
            tracing::info!(event = "session_path_selected", path = "direct", service = %key, peer = %connection.0.remote_address());
            Ok(Some(Prepared {
                path: DataPath::Quic {
                    connection,
                    send,
                    recv,
                    framing_key,
                    _network: network,
                },
                grant,
                owner: self.clone(),
                diagnostics: self.diagnostics.clone(),
            }))
        } else {
            tracing::debug!(reason = ?candidate.as_ref().map(|r| r.as_ref().map(|_| ())), "direct attempt failed before commit");
            attempt.fell_back("direct_timeout_or_unreachable");
            self.cooldown().back_off(Instant::now());
            control.relay(grant.id).await?;
            let ready = crate::client::stream::subscribe_request(
                PbConnRequest::DirectRelay {
                    id: grant.id,
                    secret: grant.secret,
                    namespace: self.namespace,
                    data_protocol: Some(pb_mapper_protocol::data::DATA_PROTOCOL_V2),
                },
                self.remote.clone(),
                self.keep_alive,
                self.credential,
                true,
                &mut None,
            )
            .await
            .map_err(io_error)?;
            pending.completed = true;
            tracing::info!(event = "session_path_selected", path = "relay", service = %key, reason = "direct_timeout_or_unreachable");
            Ok(Some(Prepared {
                path: DataPath::Relay {
                    socket: ready.0,
                    codec: ready.1,
                },
                grant,
                owner: self.clone(),
                diagnostics: self.diagnostics.clone(),
            }))
        }
    }
}
struct PendingSession {
    network: Arc<Network>,
    control: Arc<Control>,
    id: SessionId,
    completed: bool,
}
impl Drop for PendingSession {
    fn drop(&mut self) {
        self.network.forget(&self.id);
        if !self.completed {
            self.control.cancel(self.id);
        }
    }
}
pub(super) struct QuicGuard(pub quinn::Connection);
impl Drop for QuicGuard {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"session finished");
    }
}
enum DataPath {
    Quic {
        connection: QuicGuard,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
        framing_key: [u8; 32],
        _network: Arc<Network>,
    },
    Relay {
        socket: pb_mapper_protocol::transport::RelayStream,
        codec: Option<DataCodec>,
    },
}
pub(crate) struct Prepared {
    path: DataPath,
    grant: SessionGrant,
    owner: Arc<Consumer>,
    diagnostics: Diagnostics,
}
impl Prepared {
    pub async fn forward<L: NetworkStream + StreamForward>(
        self,
        mut local: L,
        credential: Credential,
    ) -> io::Result<()> {
        let path = if matches!(self.path, DataPath::Quic { .. }) {
            SessionPath::Direct
        } else {
            SessionPath::Relay
        };
        let mut report = ConsumerReport {
            owner: self.owner,
            session: (&self.grant).into(),
            secret: self.grant.secret,
            path,
            seq: 0,
            diagnostics: self.diagnostics,
        };
        report.diagnostics.session_opened(path);
        report.active();
        let forwarding = async {
            match self.path {
                DataPath::Quic {
                    connection: _connection,
                    send,
                    recv,
                    framing_key,
                    _network,
                } => forward_quic(local, send, recv, framing_key, None).await,
                DataPath::Relay { mut socket, codec } => {
                    pb_mapper_protocol::with_relay_stream!(&mut socket, socket, {
                        let (read, write) = local.split();
                        let (remote_read, remote_write) = socket.split();
                        L::forward_local_to_remote_with_codec(
                            codec,
                            *credential.key(),
                            read,
                            write,
                            remote_read,
                            remote_write,
                        )
                        .await
                        .map_err(io_error)
                    })
                }
            }
        };
        tokio::pin!(forwarding);
        let mut ticker = tokio::time::interval(Duration::from_secs(15));
        ticker.tick().await;
        loop {
            tokio::select! { result = &mut forwarding => return result, _ = ticker.tick() => report.active() }
        }
    }
}
async fn forward_quic<L: NetworkStream + StreamForward>(
    mut local: L,
    mut send: quinn::SendStream,
    recv: quinn::RecvStream,
    framing: [u8; 32],
    idle: Option<Duration>,
) -> io::Result<()> {
    let (read, write) = local.split();
    let forward =
        L::forward_local_to_remote_with_codec(None, framing, read, write, recv, &mut send);
    if let Some(idle) = idle {
        pb_mapper_protocol::forward::with_tunnel_idle_timeout(idle, forward)
            .await
            .map_err(io_error)?;
    } else {
        forward.await.map_err(io_error)?;
    }
    // FIN queues bytes but does not acknowledge delivery. Keep the connection
    // alive until Quinn confirms the final response; local stop still cancels us.
    let _ = send.finish();
    let _ = send.stopped().await;
    Ok(())
}

struct ConsumerReport {
    owner: Arc<Consumer>,
    session: SessionSummary,
    secret: SessionSecret,
    path: SessionPath,
    seq: u64,
    diagnostics: Diagnostics,
}
impl ConsumerReport {
    fn update(&mut self, state: SessionState) {
        self.seq += 1;
        self.owner.reports.put(SessionUpdate {
            session: self.session.clone(),
            secret: self.secret,
            report: SessionReport {
                id: self.session.id,
                seq: self.seq,
                path: self.path,
                state,
                reason: None,
            },
        });
        self.owner.report_wake.notify_one();
    }
    fn active(&mut self) {
        self.update(SessionState::Active);
    }
}
impl Drop for ConsumerReport {
    fn drop(&mut self) {
        self.diagnostics.session_closed(self.path);
        self.update(SessionState::Closed);
    }
}
async fn report_loop(
    remote: RelayEndpoint,
    credential: Credential,
    namespace: Option<u64>,
    control: Arc<Mutex<Option<Arc<Control>>>>,
    reports: Arc<reports::Latest>,
    wake: Arc<tokio::sync::Notify>,
) {
    let mut delay = Duration::from_secs(15);
    let mut retry = Duration::from_secs(1);
    loop {
        tokio::select! { () = wake.notified() => {}, () = tokio::time::sleep(delay) => {} }
        let snapshot = reports.snapshot();
        if snapshot.is_empty() {
            continue;
        }
        let attempt = async {
            let current = {
                let mut slot = control.lock().await;
                if slot.as_ref().is_none_or(|c| !c.alive()) {
                    let _permit = remote.control_permit().await;
                    *slot = Some(Control::connect(&remote, credential, namespace).await?);
                }
                slot.clone()
            };
            if let Some(control) = current {
                for report in snapshot {
                    // Backpressure belongs to this independent reporter. A
                    // snapshot larger than the channel must not repeatedly lose
                    // the same tail, or hold the setup mutex while it drains.
                    control.report(report).await?;
                }
            }
            Ok::<_, io::Error>(())
        };
        match tokio::time::timeout(Duration::from_secs(3), attempt).await {
            Ok(Ok(())) => {
                delay = Duration::from_secs(15);
                retry = Duration::from_secs(1);
            }
            _ => {
                // A failed reconnect must not cost another full reporting
                // interval. Retry independently of data with bounded backoff.
                delay = retry;
                retry = (retry * 2).min(Duration::from_secs(15));
            }
        }
    }
}

#[cfg(test)]
mod tests;
