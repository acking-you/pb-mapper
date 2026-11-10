use std::time::Duration;

use pb_mapper_protocol::transport::RelayStream;
use snafu::ResultExt;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use super::error::{
    ServerConnCreateHeaderToolSnafu, ServerConnEncodeRegisterRespSnafu,
    ServerConnEncodeStreamRequestSnafu, ServerConnRecvConnTaskSnafu,
    ServerConnRecvServerRegisteredRespSnafu, ServerConnRegisteredRespNotMatchSnafu,
    ServerConnSendControlTaskSnafu, ServerConnSendRegisterSnafu, ServerConnWritePongRespSnafu,
    ServerConnWriteRegisterRespSnafu, ServerConnWriteStreamRequestSnafu,
};
use super::{ConnTask, ConnTaskReceiver, ImutableKey, ManagerTask, ManagerTaskSender, Result};
use crate::guard::{DeregisterGuard, Deregistration};
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::command::{
    CONTROL_PROTOCOL_V2, LocalServer, MessageSerializer, PbConnResponse, PbServerRequest,
};
use pb_mapper_protocol::direct::DirectCapability;
use pb_mapper_protocol::secure::ServerHeaderSession;
use pb_mapper_protocol::{MessageReader, MessageWriter};

/// A registered control connection, returned to the manager when its socket
/// task ends, however it ends.
struct ServerDeregistration {
    key: ImutableKey,
    conn_id: RemoteConnId,
}

impl Deregistration for ServerDeregistration {
    fn task(&self) -> ManagerTask {
        ManagerTask::DeRegisterServerConn {
            key: self.key.clone(),
            conn_id: self.conn_id,
        }
    }

    fn sent(&self) {
        tracing::info!(
            "Server conn deregistered! key:{} conn_id:{}",
            self.key,
            self.conn_id
        );
    }

    fn manager_closed(&self) {
        tracing::debug!(
            "skip async deregister because manager channel is closed: key:{} conn_id:{}",
            self.key,
            self.conn_id
        );
    }

    fn dropped_sent(&self) {
        tracing::info!(
            "Server conn drop! key:{} conn_id:{}",
            self.key,
            self.conn_id
        );
    }

    fn dropped_manager_closed(&self) {
        tracing::debug!(
            "skip deregister on drop because manager channel is closed: key:{} conn_id:{}",
            self.key,
            self.conn_id
        );
    }

    fn dropped_queue_full(&self) {
        tracing::warn!(
            "manager queue is full; defer server deregister: key:{} conn_id:{}",
            self.key,
            self.conn_id
        );
    }

    fn deferred_manager_closed() {
        tracing::debug!("skip deferred deregister because manager channel is closed");
    }

    fn no_runtime() {
        tracing::warn!("cannot defer deregister because no Tokio runtime is available");
    }
}

const DEFAULT_SERVER_CHAN_CAP: usize = 32 * 4;
/// Idle threshold for a protocol-v1 registration, which has no lease to renew.
pub(super) const LEGACY_SERVER_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 11);
/// How often a protocol-v1 local server pings its registration.
const LEGACY_PING_INTERVAL: Duration = Duration::from_secs(5 * 60);

const _: () = assert!(
    LEGACY_SERVER_IDLE_TIMEOUT.as_secs() > LEGACY_PING_INTERVAL.as_secs(),
    "equal values race under scheduler and network jitter and drop a healthy registration"
);

/// Pongs the reader may queue for the writer before a registration is dropped.
const CONTROL_WRITE_QUEUE: usize = 64;

pub struct ServerRegistration {
    pub key: ImutableKey,
    pub need_codec: bool,
    pub is_datagram: bool,
    pub protocol_version: u16,
    pub conn_id: RemoteConnId,
    pub direct: Option<pb_mapper_auth::AuthContext>,
}

/// Serve one registered service's control connection: register it with the
/// manager, answer with what the manager granted, then carry control frames
/// both ways until the connection ends or is retired.
#[instrument(skip(registration, task_sender, conn, session))]
pub async fn handle_server_conn(
    registration: ServerRegistration,
    task_sender: ManagerTaskSender,
    mut conn: RelayStream,
    session: ServerHeaderSession,
) -> Result<()> {
    let ServerRegistration {
        key,
        need_codec,
        is_datagram,
        protocol_version,
        conn_id,
        direct,
    } = registration;
    let (tx, rx) = kanal::bounded_async(DEFAULT_SERVER_CHAN_CAP);
    // Handed to the manager so a retirement can cancel this connection's socket
    // directly. Queuing `ConnTask::Retire` is not enough on its own: the writer
    // observes that only between writes, and a registration that has stopped
    // reading leaves it blocked inside one.
    let retire_token = CancellationToken::new();

    // Tell the manager what this connection serves.
    task_sender
        .send(ManagerTask::Register {
            key: key.clone(),
            conn_id,
            need_codec,
            is_datagram,
            protocol_version,
            conn_sender: tx,
            retire_token: retire_token.clone(),
            direct,
        })
        .await
        .context(ServerConnSendRegisterSnafu {
            key: key.clone(),
            conn_id,
        })?;
    tracing::debug!(
        event = "server_register_task_sent",
        key = %key,
        conn_id = %conn_id,
        need_codec,
        is_datagram,
        "server register task sent to manager"
    );

    let mut guard = DeregisterGuard::new(
        ServerDeregistration {
            key: key.clone(),
            conn_id,
        },
        task_sender.clone(),
    );
    let result = async {
        let response = rx
            .recv()
            .await
            .context(ServerConnRecvServerRegisteredRespSnafu {
                key: key.clone(),
                conn_id,
            })?;
        let registered = match response {
            ConnTask::RegisterResp {
                generation,
                protocol_version,
                lease_ttl_ms,
                direct,
            } => Registered {
                key: key.clone(),
                conn_id,
                need_codec,
                is_datagram,
                generation,
                protocol_version,
                lease_ttl_ms,
                direct,
            },
            ConnTask::RegisterFailed {
                code,
                reason,
                retryable,
            } => {
                let refusal = PbConnResponse::error(code, reason, retryable);
                return write_register_refusal(&mut conn, &session, refusal, &key, conn_id).await;
            }
            _ => {
                return ServerConnRegisteredRespNotMatchSnafu {
                    key: key.clone(),
                    conn_id,
                }
                .fail();
            }
        };
        serve_registered(registered, conn, session, rx, retire_token, &task_sender).await
    }
    .await;
    match &result {
        Ok(()) => tracing::info!(
            event = "server_conn_finished",
            key = %key,
            conn_id = %conn_id,
            "server connection handler finished"
        ),
        Err(e) => tracing::warn!(
            event = "server_conn_failed",
            key = %key,
            conn_id = %conn_id,
            error = %e,
            "server connection handler finished with error"
        ),
    }
    guard.deregister().await;
    result
}

/// Tell the local server why the manager refused its registration.
async fn write_register_refusal(
    conn: &mut RelayStream,
    session: &ServerHeaderSession,
    refusal: PbConnResponse,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    let refusal = refusal
        .encode()
        .context(ServerConnEncodeRegisterRespSnafu {
            key: key.clone(),
            conn_id,
        })?;
    let mut writer = session
        .response_writer(conn)
        .context(ServerConnCreateHeaderToolSnafu { tool: "writer" })?;
    writer
        .write_msg(&refusal)
        .await
        .context(ServerConnWriteRegisterRespSnafu {
            key: key.clone(),
            conn_id,
        })
}

/// A registration as the manager accepted it: what the local server asked
/// for, and what the manager granted.
struct Registered {
    key: ImutableKey,
    conn_id: RemoteConnId,
    need_codec: bool,
    is_datagram: bool,
    generation: u64,
    protocol_version: u16,
    lease_ttl_ms: u64,
    direct: Option<DirectCapability>,
}

/// Answer an accepted registration, then read its control frames here while a
/// task of its own writes to it, until either side ends.
async fn serve_registered(
    registered: Registered,
    conn: RelayStream,
    session: ServerHeaderSession,
    rx: ConnTaskReceiver,
    retire_token: CancellationToken,
    task_sender: &ManagerTaskSender,
) -> Result<()> {
    let Registered {
        key,
        conn_id,
        need_codec,
        is_datagram,
        generation,
        protocol_version,
        lease_ttl_ms,
        direct,
    } = registered;
    tracing::debug!(
        event = "server_register_ack_received",
        key = %key,
        conn_id = %conn_id,
        generation,
        protocol_version,
        "server register ack received from manager"
    );

    let (mut reader, mut writer) = conn.into_split();
    let mut msg_reader = session
        .continuation_reader(&mut reader)
        .context(ServerConnCreateHeaderToolSnafu { tool: "reader" })?;
    // Keep one header writer for the register response and all later control frames. The
    // encrypted header codec is stateful; recreating it between frames breaks peer decoding.
    let register_response = if protocol_version >= CONTROL_PROTOCOL_V2 {
        PbConnResponse::RegisterV2 {
            conn_id: conn_id.into(),
            generation,
            lease_ttl_ms,
            direct,
        }
    } else {
        PbConnResponse::Register(conn_id.into())
    }
    .encode()
    .context(ServerConnEncodeRegisterRespSnafu {
        key: key.clone(),
        conn_id,
    })?;
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel(CONTROL_WRITE_QUEUE);
    let writer_key = key.clone();
    // Aborted when dropped, so the writer never outlives this function.
    let mut writer_handle = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let mut msg_writer = session
            .response_writer(&mut writer)
            .context(ServerConnCreateHeaderToolSnafu { tool: "writer" })?;
        msg_writer.write_msg(&register_response).await.context(
            ServerConnWriteRegisterRespSnafu {
                key: writer_key.clone(),
                conn_id,
            },
        )?;
        tracing::info!(
            event = "server_register_response_written",
            key = %writer_key,
            conn_id = %conn_id,
            need_codec,
            is_datagram,
            generation,
            protocol_version,
            lease_ttl_ms,
            "server register response written to local server"
        );
        run_control_writer(
            &mut msg_writer,
            &rx,
            &mut write_rx,
            &retire_token,
            writer_key,
            conn_id,
        )
        .await
    }));

    // Read once: the environment it comes from is not written after startup.
    let idle_timeout = crate::server_conn_idle_timeout(protocol_version);
    let reader_result = async {
        loop {
            let msg = match tokio::time::timeout(idle_timeout, msg_reader.read_msg()).await {
                Ok(Ok(msg)) => msg,
                Ok(Err(e)) => {
                    tracing::error!(
                        event = "server_conn_control_read_failed",
                        key = %key,
                        conn_id = %conn_id,
                        error = %snafu::Report::from_error(e),
                        "server connection control read failed"
                    );
                    break Ok(());
                }
                Err(_) => {
                    tracing::error!(
                        event = "server_conn_idle_timeout",
                        key = %key,
                        conn_id = %conn_id,
                        timeout = ?idle_timeout,
                        protocol_version,
                        "server connection idle timeout triggered"
                    );
                    break Ok(());
                }
            };
            handle_control_message(msg, &write_tx, task_sender, &key, conn_id).await?;
        }
    };

    tokio::select! {
        result = reader_result => result,
        result = &mut writer_handle => match result {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(
                    event = "server_conn_writer_join_failed",
                    key = %key,
                    conn_id = %conn_id,
                    error = %e,
                    "server connection writer task failed"
                );
                Ok(())
            }
        },
    }
}

#[instrument(skip(msg, write_tx))]
async fn handle_control_message(
    msg: &[u8],
    write_tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    task_sender: &ManagerTaskSender,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    let req = match PbServerRequest::decode(msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                event = "ping_decode_failed",
                key = %key,
                conn_id = %conn_id,
                error = %e,
                "failed to decode ping request"
            );
            return Ok(());
        }
    };

    match req {
        PbServerRequest::Direct(message) => {
            send_control_task(
                task_sender,
                ManagerTask::Direct(super::direct::DirectTask::Provider {
                    connection: conn_id,
                    key: key.clone(),
                    message,
                }),
                key,
                conn_id,
            )
            .await
        }
        PbServerRequest::Ping => {
            renew_lease(task_sender, key, conn_id).await?;
            let resp = match LocalServer::Pong.encode() {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        event = "pong_encode_failed",
                        key = %key,
                        conn_id = %conn_id,
                        error = %e,
                        "failed to encode pong response"
                    );
                    return Ok(());
                }
            };

            tracing::debug!(
                event = "ping_received",
                key = %key,
                conn_id = %conn_id,
                "received ping from local server"
            );
            queue_pong(write_tx, resp, key, conn_id)
        }
        PbServerRequest::PingV2 { seq } => {
            renew_lease(task_sender, key, conn_id).await?;
            let resp = match (LocalServer::PongV2 { seq }).encode() {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        event = "pong_v2_encode_failed",
                        key = %key,
                        conn_id = %conn_id,
                        seq,
                        error = %e,
                        "failed to encode pong v2 response"
                    );
                    return Ok(());
                }
            };

            tracing::debug!(
                event = "ping_v2_received",
                key = %key,
                conn_id = %conn_id,
                seq,
                "received ping v2 from local server"
            );
            queue_pong(write_tx, resp, key, conn_id)
        }
        PbServerRequest::StreamAck {
            client_id,
            server_generation,
        } => {
            tracing::debug!(
                event = "stream_ack_received",
                key = %key,
                server_conn_id = %conn_id,
                client_conn_id = client_id,
                server_generation,
                "received stream ack from local server"
            );
            send_control_task(
                task_sender,
                ManagerTask::StreamAck {
                    server_id: conn_id,
                    client_id: client_id.into(),
                    server_generation,
                },
                key,
                conn_id,
            )
            .await
        }
    }
}

/// Hand the manager a task a control message asked for.
async fn send_control_task(
    task_sender: &ManagerTaskSender,
    task: ManagerTask,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    task_sender
        .send(task)
        .await
        .with_context(|_| ServerConnSendControlTaskSnafu {
            key: key.clone(),
            conn_id,
        })
}

/// Renew the registration's lease, as every ping does.
async fn renew_lease(
    task_sender: &ManagerTaskSender,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    let activity = ManagerTask::ServerConnActivity {
        key: key.clone(),
        conn_id,
    };
    send_control_task(task_sender, activity, key, conn_id).await
}

/// Queue a pong for the writer. A full queue ends the registration, as a
/// closed one does.
fn queue_pong(
    write_tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    pong: Vec<u8>,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    write_tx
        .try_send(pong)
        .map_err(|_| super::error::Error::ServerConnControlWriterClosed {
            key: key.clone(),
            conn_id,
        })
}

/// Serve one registered control connection's writes until it is told to stop.
///
/// Ends on a retirement, on either channel closing, or on a write failing.
///
/// `retire_token` exists alongside the `ConnTask::Retire` message because the two
/// arrive by different means. The message is queued on `task_rx`, which this loop
/// only looks at between writes: a registration that has stopped reading leaves a
/// write outstanding with no timeout above it, and the message then waits behind a
/// write that may never return. So the token races the loop as a whole rather than
/// sitting in its `select!`, and cancellation abandons the in-flight write.
///
/// Dropping a write half-way is safe here: the borrowed writer is the only handle
/// to the socket's write side, so the caller drops it immediately after, and a
/// truncated control frame on a connection being torn down has no reader left to
/// mislead.
async fn run_control_writer<T: MessageWriter>(
    msg_writer: &mut T,
    task_rx: &ConnTaskReceiver,
    write_rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    retire_token: &CancellationToken,
    key: ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    let writer_loop = async {
        // Keep the kanal receive future alive when a Pong wins select. A send
        // can already have transferred ownership before the receiver is polled.
        let mut request = std::pin::pin!(task_rx.recv());
        loop {
            tokio::select! {
                req = &mut request => {
                    let req = req.context(ServerConnRecvConnTaskSnafu)?;
                    request.set(task_rx.recv());
                    match req {
                        ConnTask::Retire { reason } => {
                            tracing::warn!(
                                event = "server_conn_retire_requested",
                                key = %key,
                                conn_id = %conn_id,
                                reason = %reason,
                                "server control connection writer is closing"
                            );
                            break Ok(());
                        }
                        req => {
                            handle_stream_req(req, msg_writer, key.clone(), conn_id).await?;
                        }
                    }
                }
                pong = write_rx.recv() => {
                    let Some(pong) = pong else {
                        break Ok(());
                    };
                    msg_writer
                        .write_msg(&pong)
                        .await
                        .context(ServerConnWritePongRespSnafu {
                            key: key.clone(),
                            conn_id,
                        })?;
                }
            }
        }
    };
    tokio::select! {
        biased;
        () = retire_token.cancelled() => {
            tracing::warn!(
                event = "server_conn_writer_cancelled",
                key = %key,
                conn_id = %conn_id,
                "server control connection writer cancelled mid-flight"
            );
            Ok(())
        }
        result = writer_loop => result,
    }
}

#[instrument(skip(req, writer))]
async fn handle_stream_req<T: MessageWriter>(
    req: ConnTask,
    writer: &mut T,
    key: ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    match req {
        ConnTask::Direct(event) => {
            let msg = LocalServer::Direct(event).encode().context(
                ServerConnEncodeStreamRequestSnafu {
                    key: key.clone(),
                    conn_id,
                },
            )?;
            writer
                .write_msg(&msg)
                .await
                .context(ServerConnWriteStreamRequestSnafu { key, conn_id })?;
        }
        ConnTask::StreamReq {
            client_id: client_conn_id,
            server_generation,
        } => {
            let msg = LocalServer::Stream {
                client_id: client_conn_id.into(),
                server_generation,
            }
            .encode()
            .context(ServerConnEncodeStreamRequestSnafu {
                key: key.clone(),
                conn_id,
            })?;
            tracing::debug!(
                event = "stream_request_written_to_local_server",
                key = %key,
                server_conn_id = %conn_id,
                client_conn_id = %client_conn_id,
                server_generation,
                "writing stream request to local server"
            );
            writer
                .write_msg(&msg)
                .await
                .context(ServerConnWriteStreamRequestSnafu { key, conn_id })?;
        }
        req => {
            tracing::error!(
                event = "unexpected_server_conn_task",
                key = %key,
                server_conn_id = %conn_id,
                task = ?req,
                "expected stream request task"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::Poll;

    use pb_mapper_core::conn_id::RemoteConnId;
    use pb_mapper_protocol::MessageWriter;
    use tokio_util::sync::CancellationToken;

    use super::{ConnTask, run_control_writer};

    /// A writer whose every write blocks forever.
    ///
    /// Stands in for a registration that has stopped reading: the socket's send
    /// buffer fills, and `write_msg` never returns. Nothing above it bounds the
    /// wait — the server applies no timeout to a control write.
    struct WedgedWriter;

    impl MessageWriter for WedgedWriter {
        async fn write_msg(&mut self, _msg: &[u8]) -> pb_mapper_core::error::Result<()> {
            std::future::pending().await
        }
    }

    /// Retirement must reach a writer that is already blocked inside a write.
    ///
    /// The bug this pins: `ConnTask::Retire` travels on the same queue the writer
    /// only reads between writes, so a writer stuck in one never sees it. The
    /// manager would meanwhile drop the registration and report it retired while
    /// the socket task stayed alive — and since its connection ID is not recycled
    /// until that task's guard deregisters, the ID leaked with it.
    #[tokio::test]
    async fn cancelling_the_retire_token_unwinds_a_writer_blocked_mid_write() {
        let (task_tx, task_rx) = kanal::bounded_async(4);
        let (write_tx, mut write_rx) = tokio::sync::mpsc::channel(super::CONTROL_WRITE_QUEUE);
        let retire_token = CancellationToken::new();
        let key: Arc<str> = Arc::from("wedged-service");
        let conn_id = RemoteConnId::from(3);

        let mut writer = WedgedWriter;
        let mut control = std::pin::pin!(run_control_writer(
            &mut writer,
            &task_rx,
            &mut write_rx,
            &retire_token,
            key,
            conn_id,
        ));

        // Wedge the writer first. Queuing the retirement alongside it would race:
        // `select!` polls its arms in a random order, and could take the retirement
        // before the write ever started. One poll takes the pong and parks the
        // writer inside its write.
        write_tx.try_send(b"pong".to_vec()).unwrap();
        assert!(
            poll_once(&mut control).await.is_pending(),
            "the writer should still be inside its write"
        );

        // Now the retirement, behind a write that will never return.
        task_tx
            .send(ConnTask::Retire {
                reason: "retired by administrator".to_string(),
            })
            .await
            .unwrap();
        assert!(
            poll_once(&mut control).await.is_pending(),
            "a queued retirement was somehow observed during a blocked write"
        );

        retire_token.cancel();
        match poll_once(&mut control).await {
            Poll::Ready(result) => {
                result.expect("an interrupted write should not surface as an error")
            }
            Poll::Pending => {
                panic!("cancelling the retire token did not unwind the blocked writer")
            }
        }
    }

    /// Poll `future` once, without waiting for it to be woken.
    async fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
        poll_fn(|cx| Poll::Ready(Pin::new(&mut *future).poll(cx))).await
    }
}
