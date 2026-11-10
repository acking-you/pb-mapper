use std::sync::Arc;
use std::time::{Duration, Instant};

use snafu::ResultExt;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use super::error::{
    self, ControlIoTimeoutSnafu, DecodeStreamReqSnafu, EncodePingMsgSnafu, EncodeStreamAckMsgSnafu,
    WritePingMsgSnafu, WriteStreamAckMsgSnafu,
};
use super::stream::{StreamConnect, handle_stream_on_endpoint};
use crate::diagnostics::Diagnostics;
use crate::endpoint::RelayEndpoint;
use pb_mapper_core::config::control_io_timeout;
use pb_mapper_core::snafu_error_handle;
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{
    CONTROL_PROTOCOL_V2, LocalServer, MessageSerializer, PbServerRequest,
};
use pb_mapper_protocol::forward::StreamForward;
use uni_stream::stream::StreamProvider;

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

#[derive(Debug, Clone, Copy)]
pub(super) struct ControlRegistration {
    pub(super) conn_id: u32,
    pub(super) generation: u64,
    pub(super) protocol_version: u16,
    pub(super) lease_ttl_ms: u64,
}

#[derive(Debug)]
pub(super) struct ControlLeaseState {
    last_rx_at: Instant,
    last_pong_at: Option<Instant>,
    /// The latest heartbeat written, by sequence number, and when.
    ping: Option<(u64, Instant)>,
}

impl ControlLeaseState {
    pub(super) fn new() -> Self {
        Self {
            last_rx_at: Instant::now(),
            last_pong_at: None,
            ping: None,
        }
    }

    pub(super) fn record_rx(&mut self) {
        self.last_rx_at = Instant::now();
    }

    fn record_pong(&mut self) {
        let now = Instant::now();
        self.last_rx_at = now;
        self.last_pong_at = Some(now);
    }

    pub(super) fn ping_sent(&mut self, seq: u64) {
        self.ping = Some((seq, Instant::now()));
    }

    /// The pong to heartbeat `seq`: its round trip, when it answers the
    /// latest heartbeat written.
    fn record_pong_to(&mut self, seq: u64) -> Option<Duration> {
        self.record_pong();
        let (_, at) = self.ping.take_if(|(sent, _)| *sent == seq)?;
        Some(at.elapsed())
    }

    pub(super) fn last_rx_age(&self) -> Duration {
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
pub(super) async fn handle_request<LocalStream: StreamProvider>(
    msg: &[u8],
    target: StreamConnect,
    relay_endpoint: RelayEndpoint,
    diagnostics: &Diagnostics,
    key: Arc<str>,
    conn_id: u32,
    write_tx: &tokio::sync::mpsc::Sender<LocalControlWrite>,
    lease_state: Arc<tokio::sync::Mutex<ControlLeaseState>>,
    stream_tasks: &mut JoinSet<()>,
    shutdown: &CancellationToken,
    setup_slots: &Arc<tokio::sync::Semaphore>,
    #[cfg(feature = "p2p")] direct: Option<&Arc<crate::direct::Provider>>,
) -> error::Result<()>
where
    LocalStream::Item: StreamForward,
{
    let req = LocalServer::decode(msg).context(DecodeStreamReqSnafu)?;

    match req {
        LocalServer::Direct(event) => {
            #[cfg(feature = "p2p")]
            if let Some(direct) = direct {
                let mut event = event;
                // `remote_addr` is the relay address this registration is
                // connected to: the one to probe when the relay advertises
                // only its STUN port.
                if let (
                    pb_mapper_protocol::direct::DirectEvent::Offer { stun_addr, .. },
                    Some(relay),
                ) = (&mut *event, target.remote_addr.as_slice().first())
                {
                    *stun_addr = pb_mapper_protocol::direct::stun_target(*stun_addr, relay.ip());
                }
                direct.event(*event, write_tx.clone());
            }
            #[cfg(not(feature = "p2p"))]
            let _ = event;
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
            let key = key.clone();
            let stream_shutdown = shutdown.clone();
            let diagnostics = diagnostics.clone();
            // Tracked rather than detached: a cancelled registration must take its
            // in-flight forwarded sessions with it, so `stop()` returning means no
            // stream of this tunnel is still moving bytes.
            stream_tasks.spawn(async move {
                let forward = handle_stream_on_endpoint::<LocalStream>(
                    key,
                    client_id,
                    server_generation,
                    target,
                    permit,
                    relay_endpoint,
                    diagnostics,
                );
                tokio::select! {
                    () = stream_shutdown.cancelled() => {}
                    result = forward => snafu_error_handle!(result),
                }
            });
        }
        // got pong response
        LocalServer::Pong => {
            lease_state.lock().await.record_pong();
            tracing::debug!(
                event = "local_server_pong_received",
                key = %key,
                server_conn_id = %conn_id,
                "local server received pong"
            );
        }
        LocalServer::PongV2 { seq } => {
            let rtt = lease_state.lock().await.record_pong_to(seq);
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
