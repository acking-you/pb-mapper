use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use snafu::ResultExt;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use super::error::{
    self, ControlIoTimeoutSnafu, DecodeStreamReqSnafu, EncodePingMsgSnafu, EncodeStreamAckMsgSnafu,
    WritePingMsgSnafu, WriteStreamAckMsgSnafu,
};
use super::stream::{SpawnStream, StreamConnect, StreamJob};
use crate::diagnostics::Diagnostics;
use crate::endpoint::RelayEndpoint;
use pb_mapper_core::config::control_io_timeout;
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{
    CONTROL_PROTOCOL_V2, LocalServer, MessageSerializer, PbServerRequest,
};
use pb_mapper_protocol::direct::DirectEvent;

fn get_ping_message(protocol_version: u16, seq: u64) -> error::Result<Vec<u8>> {
    if protocol_version >= CONTROL_PROTOCOL_V2 {
        PbServerRequest::PingV2 { seq }
            .encode()
            .context(EncodePingMsgSnafu)
    } else {
        PbServerRequest::Ping.encode().context(EncodePingMsgSnafu)
    }
}

pub(crate) enum LocalControlWrite {
    /// Boxed so it does not widen every ping and stream acknowledgement.
    #[cfg(feature = "p2p")]
    Direct(Box<pb_mapper_protocol::direct::DirectProviderMessage>),
    Ping {
        seq: u64,
    },
    StreamAck {
        client_id: u32,
        server_generation: u64,
    },
}

/// The registration's direct-session provider, as its control plane sees it:
/// one, or none when the registration refuses direct sessions.
///
/// Without `p2p` there is never one: this is then empty and every call does
/// nothing, so the worker and its attempts are written once for both builds.
#[derive(Clone, Default)]
pub(super) struct DirectProvider(
    #[cfg(feature = "p2p")] pub(super) Option<Arc<crate::direct::Provider>>,
);

#[cfg(feature = "p2p")]
impl DirectProvider {
    /// The direct protocol a registration offers the relay: only with a
    /// provider to serve the sessions it would be offered.
    pub(super) fn version(&self) -> Option<u16> {
        self.0
            .as_ref()
            .map(|_| pb_mapper_protocol::direct::DIRECT_VERSION)
    }

    pub(super) fn has_active(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|provider| provider.has_active())
    }

    /// A control connection registered; the provider replies over it.
    pub(super) fn connected(&self, write_tx: &mpsc::Sender<LocalControlWrite>) {
        if let Some(provider) = &self.0 {
            provider.connected(write_tx.clone());
        }
    }

    /// Hand the relay's direct event to the provider. `relay` is the relay
    /// address this registration is connected to: the one to probe when the
    /// relay advertises only its STUN port.
    pub(super) fn event(
        &self,
        mut event: Box<DirectEvent>,
        relay: Option<&SocketAddr>,
        write_tx: &mpsc::Sender<LocalControlWrite>,
    ) {
        if let Some(provider) = &self.0 {
            if let (DirectEvent::Offer { stun_addr, .. }, Some(relay)) = (&mut *event, relay) {
                *stun_addr = pb_mapper_protocol::direct::stun_target(*stun_addr, relay.ip());
            }
            provider.event(*event, write_tx.clone());
        }
    }

    pub(super) async fn stop(&self) {
        if let Some(provider) = &self.0 {
            provider.stop().await;
        }
    }
}

#[cfg(not(feature = "p2p"))]
impl DirectProvider {
    pub(super) fn version(&self) -> Option<u16> {
        None
    }

    pub(super) fn has_active(&self) -> bool {
        false
    }

    pub(super) fn connected(&self, _write_tx: &mpsc::Sender<LocalControlWrite>) {}

    pub(super) fn event(
        &self,
        _event: Box<DirectEvent>,
        _relay: Option<&SocketAddr>,
        _write_tx: &mpsc::Sender<LocalControlWrite>,
    ) {
    }

    pub(super) async fn stop(&self) {}
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ControlRegistration {
    pub(super) conn_id: u32,
    pub(super) generation: u64,
    pub(super) protocol_version: u16,
    pub(super) lease_ttl_ms: u64,
}

/// When the relay was last heard on a control connection, and the heartbeat
/// awaiting its pong: shared by the control loop and its writer.
///
/// Every call locks for the call alone, so no lock is held across an
/// `.await`.
#[derive(Debug)]
pub(super) struct ControlLeaseState(Mutex<LeaseTimes>);

#[derive(Debug)]
struct LeaseTimes {
    last_rx_at: Instant,
    last_pong_at: Option<Instant>,
    /// The latest heartbeat written, by sequence number, and when.
    ping: Option<(u64, Instant)>,
}

impl ControlLeaseState {
    pub(super) fn new() -> Self {
        Self(Mutex::new(LeaseTimes {
            last_rx_at: Instant::now(),
            last_pong_at: None,
            ping: None,
        }))
    }

    fn times(&self) -> MutexGuard<'_, LeaseTimes> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn record_rx(&self) {
        self.times().record_rx();
    }

    fn record_pong(&self) {
        self.times().record_pong();
    }

    pub(super) fn ping_sent(&self, seq: u64) {
        self.times().ping_sent(seq);
    }

    /// The pong to heartbeat `seq`: its round trip, when it answers the
    /// latest heartbeat written.
    fn record_pong_to(&self, seq: u64) -> Option<Duration> {
        self.times().record_pong_to(seq)
    }

    pub(super) fn last_rx_age(&self) -> Duration {
        self.times().last_rx_age()
    }
}

impl LeaseTimes {
    fn record_rx(&mut self) {
        self.last_rx_at = Instant::now();
    }

    fn record_pong(&mut self) {
        let now = Instant::now();
        self.last_rx_at = now;
        self.last_pong_at = Some(now);
    }

    fn ping_sent(&mut self, seq: u64) {
        self.ping = Some((seq, Instant::now()));
    }

    fn record_pong_to(&mut self, seq: u64) -> Option<Duration> {
        self.record_pong();
        let (_, at) = self.ping.take_if(|(sent, _)| *sent == seq)?;
        Some(at.elapsed())
    }

    fn last_rx_age(&self) -> Duration {
        self.last_rx_at.elapsed()
    }
}

#[instrument(skip(writer))]
pub(super) async fn handle_ping_interval<T: MessageWriter>(
    writer: &mut T,
    _key: Arc<str>,
    registration: ControlRegistration,
    seq: u64,
) -> error::Result<()> {
    let ping_msg = get_ping_message(registration.protocol_version, seq)?;
    let timeout = control_io_timeout();
    match tokio::time::timeout(timeout, writer.write_msg(&ping_msg)).await {
        Ok(result) => result.context(WritePingMsgSnafu),
        Err(_) => ControlIoTimeoutSnafu {
            action: "write ping message",
            timeout,
        }
        .fail(),
    }
}

#[instrument(skip_all, fields(%key, conn_id))]
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_request(
    msg: &[u8],
    target: StreamConnect,
    relay_endpoint: RelayEndpoint,
    diagnostics: &Diagnostics,
    key: Arc<str>,
    conn_id: u32,
    write_tx: &mpsc::Sender<LocalControlWrite>,
    lease_state: &ControlLeaseState,
    stream_tasks: &mut JoinSet<()>,
    spawn: SpawnStream,
    shutdown: &CancellationToken,
    setup_slots: &Arc<tokio::sync::Semaphore>,
    direct: &DirectProvider,
) -> error::Result<()> {
    let req = LocalServer::decode(msg).context(DecodeStreamReqSnafu)?;

    match req {
        LocalServer::Direct(event) => {
            direct.event(event, target.remote_addr.as_slice().first(), write_tx);
        }
        LocalServer::Stream {
            client_id,
            server_generation,
        } => {
            tracing::debug!(
                event = "local_server_stream_request_received",
                key = %key,
                server_conn_id = %conn_id,
                client_conn_id = client_id,
                server_generation,
                "local server received stream request"
            );
            let Ok(permit) = setup_slots.clone().try_acquire_owned() else {
                tracing::warn!(event = "local_server_setup_saturated", %key, client_id, "stream setup capacity reached; relay can select another worker");
                return Ok(());
            };
            write_tx
                .try_send(LocalControlWrite::StreamAck {
                    client_id,
                    server_generation,
                })
                .map_err(|_| error::Error::ControlWriterClosed {
                    action: "stream ack message",
                })?;
            spawn(
                stream_tasks,
                StreamJob {
                    key,
                    client_id,
                    server_generation,
                    target,
                    permit,
                    endpoint: relay_endpoint,
                    diagnostics: diagnostics.clone(),
                    shutdown: shutdown.clone(),
                },
            );
        }
        // got pong response
        LocalServer::Pong => {
            lease_state.record_pong();
            tracing::debug!(
                event = "local_server_pong_received",
                key = %key,
                server_conn_id = %conn_id,
                "local server received pong"
            );
        }
        LocalServer::PongV2 { seq } => {
            let rtt = lease_state.record_pong_to(seq);
            if let Some(rtt) = rtt {
                diagnostics.relay_rtt(&relay_endpoint, rtt);
            }
            tracing::debug!(
                event = "local_server_pong_received",
                key = %key,
                server_conn_id = %conn_id,
                seq,
                "local server received pong v2"
            );
        }
        LocalServer::Retire {
            reason,
            conn_id: retired_conn_id,
            server_generation,
        } => {
            tracing::warn!(
                event = "local_server_control_retired",
                key = %key,
                server_conn_id = %conn_id,
                retired_conn_id,
                server_generation,
                reason = %reason,
                "remote server retired this local control connection"
            );
        }
    }
    Ok(())
}

pub(super) async fn write_stream_ack<T: MessageWriter>(
    writer: &mut T,
    client_id: u32,
    server_generation: u64,
) -> error::Result<()> {
    let ack = PbServerRequest::StreamAck {
        client_id,
        server_generation,
    }
    .encode()
    .context(EncodeStreamAckMsgSnafu)?;
    let timeout = control_io_timeout();
    match tokio::time::timeout(timeout, writer.write_msg(&ack)).await {
        Ok(result) => result.context(WriteStreamAckMsgSnafu),
        Err(_) => ControlIoTimeoutSnafu {
            action: "write stream ack message",
            timeout,
        }
        .fail(),
    }
}
