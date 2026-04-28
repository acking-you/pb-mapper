use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::instrument;

use super::error::{
    ServerConnCreateHeaderToolSnafu, ServerConnDecodeStreamRequestSnafu,
    ServerConnEncodeRegisterRespSnafu, ServerConnRecvServerRegisteredRespSnafu,
    ServerConnRegisteredRespNotMatchSnafu, ServerConnSendRegisterSnafu,
    ServerConnSendStreamAckSnafu, ServerConnWritePongRespSnafu, ServerConnWriteRegisteredOkSnafu,
    ServerConnWriteStreamRequestSnafu,
};
use super::{ConnTask, ImutableKey, ManagerTask, ManagerTaskSender, Result};
use crate::common::conn_id::RemoteConnId;
use crate::common::message::command::{
    LocalServer, MessageSerializer, PbConnResponse, PbServerRequest,
};
use crate::common::message::{
    get_header_msg_reader, get_header_msg_writer, MessageReader, MessageWriter,
};
use crate::{snafu_error_get_or_continue, snafu_error_get_or_return_ok};

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
// Must be greater than the local server ping interval (5 minutes). Equal values race
// under scheduler/network jitter and can drop a healthy registration.
const SERVER_TIMEOUT: Duration = Duration::from_secs(60 * 11);

/// Maintaining a connection to the server.
/// This connection is used to send channel request
#[instrument(skip(task_sender))]
pub async fn handle_server_conn(
    key: ImutableKey,
    need_codec: bool,
    is_datagram: bool,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
    mut conn: TcpStream,
) -> Result<()> {
    let (tx, rx) = kanal::bounded_async(DEFAULT_SERVER_CHAN_CAP);

    // register metadate
    task_sender
        .send(ManagerTask::Register {
            key: key.clone(),
            conn_id,
            need_codec,
            is_datagram,
            conn_sender: tx,
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

        if !matches!(response, ConnTask::RegisterResp) {
            ServerConnRegisteredRespNotMatchSnafu {
                key: key.clone(),
                conn_id,
            }
            .fail()?
        }
        tracing::debug!(
            event = "server_register_ack_received",
            key = %key,
            conn_id = %conn_id,
            "server register ack received from manager"
        );

        let (mut reader, mut writer) = conn.split();
        let mut msg_writer = get_header_msg_writer(&mut writer)
            .context(ServerConnCreateHeaderToolSnafu { tool: "writer" })?;
        let mut msg_reader = get_header_msg_reader(&mut reader)
            .context(ServerConnCreateHeaderToolSnafu { tool: "reader" })?;
        // response msg to local server to indicate that register handling has finished
        {
            let msg = PbConnResponse::Register(conn_id.into()).encode().context(
                ServerConnEncodeRegisterRespSnafu {
                    key: key.clone(),
                    conn_id,
                },
            )?;
            msg_writer
                .write_msg(&msg)
                .await
                .context(ServerConnWriteRegisteredOkSnafu {
                    key: key.clone(),
                    conn_id,
                })?;
            tracing::info!(
                event = "server_register_response_written",
                key = %key,
                conn_id = %conn_id,
                need_codec,
                is_datagram,
                "server register response written to local server"
            );
        }
        let (task_tx, mut task_rx) = tokio::sync::mpsc::unbounded_channel();
        let forward_handle = tokio::spawn(async move {
            while let Ok(task) = rx.recv().await {
                if task_tx.send(task).is_err() {
                    break;
                }
            }
        });

        let result = loop {
            tokio::select! {
                // handle stream request
                req = task_rx.recv() => {
                    let Some(req) = req else {
                        break Ok(());
                    };
                    snafu_error_get_or_continue!(
                        handle_stream_req(
                            req,
                            &mut msg_writer,
                            key.clone(),
                            conn_id
                        ).await
                    );
                }
                // handle ping pong check
                ret = msg_reader.read_msg() =>{
                    snafu_error_get_or_return_ok!(
                        handle_control_message(
                            snafu_error_get_or_return_ok!(ret),
                            &mut msg_writer,
                            task_sender.clone(),
                            key.clone(),
                            conn_id
                        ).await
                    );
                }
                // handle timeout
                _ = tokio::time::sleep(SERVER_TIMEOUT) =>{
                    tracing::error!(
                        event = "server_conn_idle_timeout",
                        key = %key,
                        conn_id = %conn_id,
                        timeout = ?SERVER_TIMEOUT,
                        "server connection idle timeout triggered"
                    );
                    break Ok(());
                }
            }
        };
        forward_handle.abort();
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

#[instrument(skip(msg, writer))]
async fn handle_control_message<T: MessageWriter>(
    msg: &[u8],
    writer: &mut T,
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
            writer
                .write_msg(&resp)
                .await
                .context(ServerConnWritePongRespSnafu { key, conn_id })
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

#[instrument(skip(req, writer))]
async fn handle_stream_req<T: MessageWriter>(
    req: ConnTask,
    writer: &mut T,
    key: ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    // TODO: handle stop task
    // FIXME: Maybe it can be parallelized here?
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

    use tokio::time::Instant;

    use crate::common::conn_id::RemoteConnId;
    use crate::pb_server::ManagerTask;

    use super::{ServerConnGuard, SERVER_TIMEOUT};

    #[test]
    fn server_timeout_has_slack_over_local_server_ping_interval() {
        assert!(SERVER_TIMEOUT > Duration::from_secs(5 * 60));
    }

    #[tokio::test]
    async fn test_sleep() {
        let expired_time = Instant::now();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut cnt = 0;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(expired_time)=>{
                    println!("end");
                    return;
                }
                _ = tokio::time::sleep(Duration::from_secs(3)) =>{
                    cnt += 1;
                    if cnt > 3 {
                        break;
                    }
                    println!("never print this: {cnt}");
                }
            }
        }
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
