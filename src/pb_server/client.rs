use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio::time::{timeout, Instant};
use tracing::instrument;

use super::error::{
    ClientConnEncodeSubcribeRespSnafu, ClientConnRecvStreamSnafu, ClientConnRecvStreamTimeoutSnafu,
    ClientConnRecvSubcribeRespSnafu, ClientConnRecvSubcribeRespTimeoutSnafu,
    ClientConnSendSubcribeSnafu, ClientConnStreamRespNotMatchSnafu, ClientConnSubcribeFailedSnafu,
    ClientConnSubcribeRespNotMatchSnafu, ClientConnWriteSubcribeRespSnafu,
};
use super::{ConnTask, ImutableKey, ManagerTask, ManagerTaskSender, Result};
use crate::common::checksum::{gen_random_key, AesKeyType};
use crate::common::conn_id::RemoteConnId;
use crate::common::message::command::{MessageSerializer, PbConnResponse};
use crate::common::message::forward::{
    start_datagram_forward, start_forward, CodecDatagramReader, CodecDatagramWriter,
    CodecForwardReader, CodecForwardWriter, NormalDatagramReader, NormalDatagramWriter,
    NormalForwardReader, NormalForwardWriter,
};
use crate::common::message::{get_decodec, get_encodec, get_header_msg_writer, MessageWriter};
use crate::pb_server::error::{
    ClientConnCreateHeaderToolSnafu, ClientConnEncodeStreamRespSnafu,
    ClientConnWriteStreamRespSnafu,
};
use crate::{create_component, snafu_error_get_or_return_ok, start_forward_with_codec_key};

/// Ensure that client-side connections are properly deregistered before a normal connection is
/// disconnected or an exception occurs
struct ClientConnGuard {
    client_id: RemoteConnId,
    server_id: Option<RemoteConnId>,
    sender: ManagerTaskSender,
    key: ImutableKey,
    active: bool,
}

impl ClientConnGuard {
    fn new(
        client_id: RemoteConnId,
        server_id: Option<RemoteConnId>,
        sender: ManagerTaskSender,
        key: ImutableKey,
    ) -> Self {
        Self {
            client_id,
            server_id,
            sender,
            key,
            active: true,
        }
    }

    fn set_server_id(&mut self, server_id: RemoteConnId) {
        self.server_id = Some(server_id);
    }

    fn deregister_task(&self) -> ManagerTask {
        ManagerTask::DeRegisterClientConn {
            server_id: self.server_id,
            client_id: self.client_id,
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
            }
            Err(_) => {
                self.active = false;
                tracing::debug!(
                    "skip async client deregister because manager channel is closed: key:{} client:{}",
                    self.key,
                    self.client_id
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
                            "skip deferred client deregister because manager channel is closed"
                        );
                    }
                });
            }
            Err(_) => {
                tracing::warn!(
                    "cannot defer client deregister because no Tokio runtime is available"
                );
            }
        }
    }
}

impl Drop for ClientConnGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let task = self.deregister_task();
        match self.sender.try_send(task) {
            Ok(()) => {}
            Err(kanal::SendTimeoutError::Closed(_)) => {
                tracing::debug!(
                    "skip client deregister on drop because manager channel is closed: key:{} client:{}",
                    self.key,
                    self.client_id
                );
            }
            Err(kanal::SendTimeoutError::Timeout(task)) => {
                tracing::warn!(
                    "manager queue is full; defer client deregister: key:{} client:{}",
                    self.key,
                    self.client_id
                );
                Self::spawn_deregister(self.sender.clone(), task);
            }
        }
    }
}

const DEFAULT_CLIENT_CHAN_CAP: usize = 32;
const CLIENT_CONN_CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

/// 1. Request server stream
/// 2. Forward the traffic between client stream and server stream
#[instrument(skip(task_sender, conn))]
pub async fn handle_client_conn(
    key: ImutableKey,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
    mut conn: TcpStream,
) -> Result<()> {
    let prev_time = Instant::now();
    let mut guard = ClientConnGuard::new(conn_id, None, task_sender.clone(), key.clone());
    let (mut server_stream, server_id, codec_key, is_datagram) =
        match get_server_stream(&mut conn, key.clone(), conn_id, task_sender.clone()).await {
            Ok(res) => res,
            Err(e) => {
                tracing::warn!(
                    event = "client_stream_setup_failed",
                    key = %key,
                    client_conn_id = %conn_id,
                    error = %e,
                    "failed to prepare server stream for client connection"
                );
                guard.deregister().await;
                return Err(e);
            }
        };
    guard.set_server_id(server_id);

    let result = async {
        let duration = Instant::now() - prev_time;

        tracing::info!(
            event = "client_stream_setup_finished",
            key = %key,
            client_conn_id = %conn_id,
            server_conn_id = %server_id,
            setup_elapsed_ms = duration.as_millis(),
            is_datagram,
            codec_enabled = codec_key.is_some(),
            "server stream is ready; start forwarding client traffic"
        );

        let (mut client_reader, mut client_writer) = conn.split();
        let (mut server_reader, mut server_writer) = server_stream.split();

        // response message to server to indicate that stream handling has finished
        {
            let mut msg_writer = get_header_msg_writer(&mut server_writer)
                .context(ClientConnCreateHeaderToolSnafu { tool: "writer" })?;
            let msg = PbConnResponse::Stream { codec_key }.encode().context(
                ClientConnEncodeStreamRespSnafu {
                    key: key.clone(),
                    conn_id,
                },
            )?;
            msg_writer
                .write_msg(&msg)
                .await
                .context(ClientConnWriteStreamRespSnafu {
                    key: key.clone(),
                    conn_id,
                })?;
        }

        if is_datagram {
            match codec_key {
                Some(key) => {
                    start_datagram_forward(
                        CodecDatagramReader::new(
                            &mut client_reader,
                            snafu_error_get_or_return_ok!(get_decodec(&key)),
                        ),
                        CodecDatagramWriter::new(
                            &mut client_writer,
                            snafu_error_get_or_return_ok!(get_encodec(&key)),
                        ),
                        CodecDatagramReader::new(
                            &mut server_reader,
                            snafu_error_get_or_return_ok!(get_decodec(&key)),
                        ),
                        CodecDatagramWriter::new(
                            &mut server_writer,
                            snafu_error_get_or_return_ok!(get_encodec(&key)),
                        ),
                    )
                    .await;
                }
                None => {
                    start_datagram_forward(
                        NormalDatagramReader::new(&mut client_reader),
                        NormalDatagramWriter::new(&mut client_writer),
                        NormalDatagramReader::new(&mut server_reader),
                        NormalDatagramWriter::new(&mut server_writer),
                    )
                    .await;
                }
            }
        } else {
            start_forward_with_codec_key!(
                codec_key,
                &mut client_reader,
                &mut client_writer,
                &mut server_reader,
                &mut server_writer,
                true,
                true,
                true,
                true
            );
        }

        Ok(())
    }
    .await;
    match &result {
        Ok(()) => tracing::info!(
            event = "client_forward_finished",
            key = %key,
            client_conn_id = %conn_id,
            server_conn_id = %server_id,
            "client forward finished"
        ),
        Err(e) => tracing::warn!(
            event = "client_forward_failed",
            key = %key,
            client_conn_id = %conn_id,
            server_conn_id = %server_id,
            error = %e,
            "client forward finished with error"
        ),
    }
    guard.deregister().await;
    result
}

async fn send_server_deregister(
    task_sender: &ManagerTaskSender,
    key: ImutableKey,
    conn_id: RemoteConnId,
) {
    if task_sender
        .send(ManagerTask::DeRegisterServerConn { key, conn_id })
        .await
        .is_err()
    {
        tracing::debug!(
            event = "server_deregister_skipped",
            conn_id = %conn_id,
            "skip server deregister because manager channel is closed"
        );
    }
}

async fn get_server_stream(
    conn: &mut TcpStream,
    key: ImutableKey,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
) -> Result<(TcpStream, RemoteConnId, Option<AesKeyType>, bool)> {
    let (tx, rx) = kanal::bounded_async(DEFAULT_CLIENT_CHAN_CAP);
    task_sender
        .send(ManagerTask::Subcribe {
            key: key.clone(),
            conn_id,
            conn_sender: tx,
        })
        .await
        .map_err(|_| kanal::SendError(()))
        .context(ClientConnSendSubcribeSnafu {
            key: key.clone(),
            conn_id,
        })?;
    tracing::debug!(
        event = "subscribe_task_sent",
        key = %key,
        client_conn_id = %conn_id,
        "subscribe task sent to manager"
    );

    let resp = match timeout(CLIENT_CONN_CONTROL_TIMEOUT, rx.recv()).await {
        Ok(resp) => resp.context(ClientConnRecvSubcribeRespSnafu {
            key: key.clone(),
            conn_id,
        })?,
        Err(_) => ClientConnRecvSubcribeRespTimeoutSnafu {
            key: key.clone(),
            conn_id,
            timeout: CLIENT_CONN_CONTROL_TIMEOUT,
        }
        .fail()?,
    };

    // Note: A key will be generated for encrypting messages that are forwarded, and this key will
    // apply to all endpoints.
    let (codec_key, is_datagram, server_conn_id) = match resp {
        ConnTask::SubcribeResp {
            need_codec,
            is_datagram,
            server_conn_id,
        } => {
            let codec_key = if need_codec {
                Some(gen_random_key())
            } else {
                None
            };
            tracing::debug!(
                event = "subscribe_response_received",
                key = %key,
                client_conn_id = %conn_id,
                server_conn_id = %server_conn_id,
                need_codec,
                is_datagram,
                codec_enabled = codec_key.is_some(),
                "subscribe response received from manager"
            );
            (codec_key, is_datagram, server_conn_id)
        }
        ConnTask::SubcribeFailed { reason } => {
            tracing::warn!(
                event = "subscribe_failed",
                key = %key,
                client_conn_id = %conn_id,
                reason = %reason,
                "subscribe failed before stream forwarding"
            );
            ClientConnSubcribeFailedSnafu {
                key: key.clone(),
                conn_id,
                reason,
            }
            .fail()?
        }
        _ => ClientConnSubcribeRespNotMatchSnafu {
            key: key.clone(),
            conn_id,
        }
        .fail()?,
    };

    let resp = match timeout(CLIENT_CONN_CONTROL_TIMEOUT, rx.recv()).await {
        Ok(resp) => resp.context(ClientConnRecvStreamSnafu {
            key: key.clone(),
            conn_id,
        })?,
        Err(_) => {
            tracing::warn!(
                event = "server_stream_wait_timeout",
                key = %key,
                client_conn_id = %conn_id,
                server_conn_id = %server_conn_id,
                timeout = ?CLIENT_CONN_CONTROL_TIMEOUT,
                "timed out waiting for server stream"
            );
            send_server_deregister(&task_sender, key.clone(), server_conn_id).await;
            ClientConnRecvStreamTimeoutSnafu {
                key: key.clone(),
                conn_id,
                timeout: CLIENT_CONN_CONTROL_TIMEOUT,
            }
            .fail()?
        }
    };

    if let ConnTask::StreamResp { server_id, stream } = resp {
        // response message to client to indicate that subcribe handling has finished
        let mut msg_writer = get_header_msg_writer(conn)
            .context(ClientConnCreateHeaderToolSnafu { tool: "writer" })?;
        let msg = PbConnResponse::Subcribe {
            codec_key,
            client_id: conn_id.into(),
            server_id: server_id.into(),
        }
        .encode()
        .context(ClientConnEncodeSubcribeRespSnafu {
            key: key.clone(),
            conn_id,
        })?;
        msg_writer
            .write_msg(&msg)
            .await
            .context(ClientConnWriteSubcribeRespSnafu {
                key: key.clone(),
                conn_id,
            })?;
        tracing::info!(
            event = "subscribe_response_written",
            key = %key,
            client_conn_id = %conn_id,
            server_conn_id = %server_id,
            is_datagram,
            codec_enabled = codec_key.is_some(),
            "subscribe response written to client"
        );
        Ok((stream, server_id, codec_key, is_datagram))
    } else {
        ClientConnStreamRespNotMatchSnafu {
            key: key.clone(),
            conn_id,
        }
        .fail()
    }
}
