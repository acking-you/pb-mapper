use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use super::error::{
    ServerConnCreateHeaderToolSnafu, ServerConnDecodeStreamRequestSnafu,
    ServerConnEncodeRegisterRespSnafu, ServerConnRecvConnTaskSnafu,
    ServerConnRecvServerRegisteredRespSnafu, ServerConnRegisteredRespNotMatchSnafu,
    ServerConnSendRegisterSnafu, ServerConnSendStreamAckSnafu, ServerConnWritePongRespSnafu,
    ServerConnWriteRegisteredOkSnafu, ServerConnWriteStreamRequestSnafu,
};
use super::{ConnTask, ConnTaskReceiver, ImutableKey, ManagerTask, ManagerTaskSender, Result};
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::command::{
    CONTROL_PROTOCOL_V2, LocalServer, MessageSerializer, PbConnResponse, PbServerRequest,
};
use pb_mapper_protocol::secure::ServerHeaderSession;
use pb_mapper_protocol::{MessageReader, MessageWriter};

/// Ensure that server-side connections are properly deregistered before a normal connection is
/// disconnected or an exception occurs
struct ServerConnGuard {
    key: ImutableKey,
    conn_id: RemoteConnId,
    sender: ManagerTaskSender,
    active: bool,
}

impl ServerConnGuard {
    fn new(key: ImutableKey, conn_id: RemoteConnId, sender: ManagerTaskSender) -> Self {
        Self {
            key,
            conn_id,
            sender,
            active: true,
        }
    }

    fn deregister_task(&self) -> ManagerTask {
        ManagerTask::DeRegisterServerConn {
            key: self.key.clone(),
            conn_id: self.conn_id,
        }
    }

    async fn deregister(&mut self) {
        if !self.active {
            return;
        }
        let task = self.deregister_task();
        match self.sender.send(task).await {
            Ok(()) => {
                self.active = false;
                tracing::info!(
                    "Server conn deregistered! key:{} conn_id:{}",
                    self.key,
                    self.conn_id
                );
            }
            Err(_) => {
                self.active = false;
                tracing::debug!(
                    "skip async deregister because manager channel is closed: key:{} conn_id:{}",
                    self.key,
                    self.conn_id
                );
            }
        }
    }

    fn spawn_deregister(sender: ManagerTaskSender, task: ManagerTask) {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if sender.send(task).await.is_err() {
                        tracing::debug!(
                            "skip deferred deregister because manager channel is closed"
                        );
                    }
                });
            }
            Err(_) => {
                tracing::warn!("cannot defer deregister because no Tokio runtime is available");
            }
        }
    }
}

impl Drop for ServerConnGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let task = self.deregister_task();
        match self.sender.try_send(task) {
            Ok(()) => {
                tracing::info!(
                    "Server conn drop! key:{} conn_id:{}",
                    self.key,
                    self.conn_id
                );
            }
            Err(kanal::SendTimeoutError::Closed(_)) => {
                tracing::debug!(
                    "skip deregister on drop because manager channel is closed: key:{} conn_id:{}",
                    self.key,
                    self.conn_id
                );
            }
            Err(kanal::SendTimeoutError::Timeout(task)) => {
                tracing::warn!(
                    "manager queue is full; defer server deregister: key:{} conn_id:{}",
                    self.key,
                    self.conn_id
                );
                Self::spawn_deregister(self.sender.clone(), task);
            }
        }
    }
}

const DEFAULT_SERVER_CHAN_CAP: usize = 32 * 4;
/// Idle threshold for a protocol-v1 registration, which has no lease to renew.
///
/// Must be greater than the local server ping interval (5 minutes). Equal values race
/// under scheduler/network jitter and can drop a healthy registration.
pub(super) const LEGACY_SERVER_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 11);

enum ServerControlWrite {
    Pong(Vec<u8>),
}

pub struct ServerRegistration {
    pub key: ImutableKey,
    pub need_codec: bool,
    pub is_datagram: bool,
    pub protocol_version: u16,
    pub conn_id: RemoteConnId,
}

/// Maintaining a connection to the server.
/// This connection is used to send channel request
#[instrument(skip(registration, task_sender, session))]
pub async fn handle_server_conn(
    registration: ServerRegistration,
    task_sender: ManagerTaskSender,
    mut conn: TcpStream,
    session: ServerHeaderSession,
) -> Result<()> {
    let ServerRegistration {
        key,
        need_codec,
        is_datagram,
        protocol_version,
        conn_id,
    } = registration;
    let (tx, rx) = kanal::bounded_async(DEFAULT_SERVER_CHAN_CAP);
    // Handed to the manager so a retirement can cancel this connection's socket
    // directly. Queuing `ConnTask::Retire` is not enough on its own: the writer
    // observes that only between writes, and a registration that has stopped
    // reading leaves it blocked inside one.
    let retire_token = CancellationToken::new();

    // register metadate
    task_sender
        .send(ManagerTask::Register {
            key: key.clone(),
            conn_id,
            need_codec,
            is_datagram,
            protocol_version,
            conn_sender: tx,
            retire_token: retire_token.clone(),
        })
        .await
        .map_err(|_| kanal::SendError(()))
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

    let mut guard = ServerConnGuard::new(key.clone(), conn_id, task_sender.clone());
    let result = async {
        let response = rx
            .recv()
            .await
            .context(ServerConnRecvServerRegisteredRespSnafu {
                key: key.clone(),
                conn_id,
            })?;

        let ConnTask::RegisterResp {
            generation,
            protocol_version,
            lease_ttl_ms,
        } = response
        else {
            if let ConnTask::RegisterFailed {
                code,
                reason,
                retryable,
            } = response
            {
                let response = PbConnResponse::error(code, reason, retryable)
                    .encode()
                    .context(ServerConnEncodeRegisterRespSnafu {
                        key: key.clone(),
                        conn_id,
                    })?;
                let mut writer = session
                    .response_writer(&mut conn)
                    .context(ServerConnCreateHeaderToolSnafu { tool: "writer" })?;
                writer
                    .write_msg(&response)
                    .await
                    .context(ServerConnWriteRegisteredOkSnafu {
                        key: key.clone(),
                        conn_id,
                    })?;
                return Ok(());
            }
            ServerConnRegisteredRespNotMatchSnafu {
                key: key.clone(),
                conn_id,
            }
            .fail()?
        };
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
            }
        } else {
            PbConnResponse::Register(conn_id.into())
        }
        .encode()
        .context(ServerConnEncodeRegisterRespSnafu {
            key: key.clone(),
            conn_id,
        })?;
        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<ServerControlWrite>();
        let writer_key = key.clone();
        let writer_retire_token = retire_token.clone();
        let mut writer_handle = tokio::spawn(async move {
            let mut msg_writer = session
                .response_writer(&mut writer)
                .context(ServerConnCreateHeaderToolSnafu { tool: "writer" })?;
            msg_writer.write_msg(&register_response).await.context(
                ServerConnWriteRegisteredOkSnafu {
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
                &writer_retire_token,
                writer_key,
                conn_id,
            )
            .await
        });

        let reader_result = async {
            loop {
                let idle_timeout = crate::server_conn_idle_timeout(protocol_version);
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
                handle_control_message(msg, &write_tx, task_sender.clone(), key.clone(), conn_id)
                    .await?;
            }
        };

        let result = tokio::select! {
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
        };
        if !writer_handle.is_finished() {
            writer_handle.abort();
        }
        result
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

#[instrument(skip(msg, write_tx))]
async fn handle_control_message(
    msg: &[u8],
    write_tx: &tokio::sync::mpsc::UnboundedSender<ServerControlWrite>,
    task_sender: ManagerTaskSender,
    key: ImutableKey,
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
        PbServerRequest::Ping => {
            task_sender
                .send(ManagerTask::ServerConnActivity {
                    key: key.clone(),
                    conn_id,
                })
                .await
                .map_err(|_| kanal::SendError(()))
                .context(ServerConnSendStreamAckSnafu {
                    key: key.clone(),
                    conn_id,
                })?;
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
            write_tx
                .send(ServerControlWrite::Pong(resp))
                .map_err(|_| super::error::Error::ServerConnControlWriterClosed { key, conn_id })
        }
        PbServerRequest::PingV2 { seq } => {
            task_sender
                .send(ManagerTask::ServerConnActivity {
                    key: key.clone(),
                    conn_id,
                })
                .await
                .map_err(|_| kanal::SendError(()))
                .context(ServerConnSendStreamAckSnafu {
                    key: key.clone(),
                    conn_id,
                })?;
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
            write_tx
                .send(ServerControlWrite::Pong(resp))
                .map_err(|_| super::error::Error::ServerConnControlWriterClosed { key, conn_id })
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
            task_sender
                .send(ManagerTask::StreamAck {
                    server_id: conn_id,
                    client_id: client_id.into(),
                    server_generation,
                })
                .await
                .map_err(|_| kanal::SendError(()))
                .context(ServerConnSendStreamAckSnafu { key, conn_id })
        }
    }
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
    write_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerControlWrite>,
    retire_token: &CancellationToken,
    key: ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    let writer_loop = async {
        loop {
            tokio::select! {
                req = task_rx.recv() => {
                    let req = req.context(ServerConnRecvConnTaskSnafu)?;
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
                cmd = write_rx.recv() => {
                    let Some(cmd) = cmd else {
                        break Ok(());
                    };
                    match cmd {
                        ServerControlWrite::Pong(resp) => {
                            msg_writer
                                .write_msg(&resp)
                                .await
                                .context(ServerConnWritePongRespSnafu {
                                    key: key.clone(),
                                    conn_id,
                                })?;
                        }
                    }
                }
            }
        }
    };
    tokio::select! {
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
    if let ConnTask::StreamReq {
        client_id: client_conn_id,
        server_generation,
    } = req
    {
        let msg = LocalServer::Stream {
            client_id: client_conn_id.into(),
            server_generation,
        }
        .encode()
        .context(ServerConnDecodeStreamRequestSnafu {
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
            .context(ServerConnWriteStreamRequestSnafu {
                key: key.clone(),
                conn_id,
            })?
    } else {
        tracing::error!(
            event = "unexpected_server_conn_task",
            key = %key,
            server_conn_id = %conn_id,
            task = ?req,
            "expected stream request task"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::ManagerTask;
    use pb_mapper_core::conn_id::RemoteConnId;
    use pb_mapper_protocol::MessageWriter;
    use tokio_util::sync::CancellationToken;

    use super::{
        ConnTask, LEGACY_SERVER_IDLE_TIMEOUT, ServerConnGuard, ServerControlWrite,
        run_control_writer,
    };

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
        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel();
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
        // before the write ever started.
        write_tx
            .send(ServerControlWrite::Pong(b"pong".to_vec()))
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut control)
                .await
                .is_err(),
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
            tokio::time::timeout(Duration::from_millis(50), &mut control)
                .await
                .is_err(),
            "a queued retirement was somehow observed during a blocked write"
        );

        retire_token.cancel();
        tokio::time::timeout(Duration::from_millis(200), control)
            .await
            .expect("cancelling the retire token did not unwind the blocked writer")
            .expect("an interrupted write should not surface as an error");
    }

    #[test]
    fn server_timeout_has_slack_over_local_server_ping_interval() {
        assert!(LEGACY_SERVER_IDLE_TIMEOUT > Duration::from_secs(5 * 60));
    }

    #[tokio::test]
    async fn server_conn_guard_does_not_drop_deregister_when_manager_queue_is_full() {
        let (sender, receiver) = kanal::bounded_async(1);
        sender.send(ManagerTask::Shutdown).await.unwrap();
        let key: Arc<str> = Arc::from("sf-backend");

        drop(ServerConnGuard::new(
            key,
            RemoteConnId::from(7),
            sender.clone(),
        ));

        assert!(matches!(
            receiver.recv().await.unwrap(),
            ManagerTask::Shutdown
        ));
        let task = tokio::time::timeout(Duration::from_millis(200), receiver.recv())
            .await
            .expect("deregister task was lost when manager queue was full")
            .unwrap();
        assert!(matches!(
            task,
            ManagerTask::DeRegisterServerConn { conn_id, .. } if conn_id == RemoteConnId::from(7)
        ));
    }
}
