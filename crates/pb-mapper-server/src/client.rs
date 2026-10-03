use std::sync::Arc;
use std::time::Duration;

use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout};
use tracing::instrument;

use super::error::{
    ClientConnEncodeSubcribeRespSnafu, ClientConnRecvStreamSnafu, ClientConnRecvSubcribeRespSnafu,
    ClientConnRecvSubcribeRespTimeoutSnafu, ClientConnSendSubcribeSnafu,
    ClientConnStreamRespNotMatchSnafu, ClientConnSubcribeFailedSnafu,
    ClientConnSubcribeRespNotMatchSnafu, ClientConnWriteSubcribeRespSnafu,
};
use super::{ConnTask, ImutableKey, ManagerTask, ManagerTaskSender, Result};
use crate::error::{
    ClientConnCreateHeaderToolSnafu, ClientConnEncodeStreamRespSnafu,
    ClientConnWriteStreamRespSnafu,
};
use pb_mapper_core::checksum::{AesKeyType, gen_random_key};
use pb_mapper_core::config::{stream_ack_timeout, stream_ready_timeout, stream_recovery_timeout};
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{MessageSerializer, PbConnResponse};
use pb_mapper_protocol::data::DataCodec;
use pb_mapper_protocol::forward::{
    CodecDatagramReader, CodecDatagramWriter, CodecForwardReader, CodecForwardWriter,
    NormalDatagramReader, NormalDatagramWriter, NormalForwardReader, NormalForwardWriter,
    start_datagram_forward, start_forward,
};
use pb_mapper_protocol::secure::ServerHeaderSession;

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
#[instrument(skip(task_sender, conn, session))]
pub async fn handle_client_conn(
    key: ImutableKey,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
    mut conn: TcpStream,
    session: ServerHeaderSession,
    data_protocol: Option<u16>,
) -> Result<()> {
    DataCodec::validate_offer(data_protocol, session.protocol()).context(
        ClientConnCreateHeaderToolSnafu {
            tool: "data protocol",
        },
    )?;
    let prev_time = Instant::now();
    let mut guard = ClientConnGuard::new(conn_id, None, task_sender.clone(), key.clone());
    let setup_timeout = pb_mapper_core::config::control_io_timeout().min(Duration::from_secs(5));
    let setup_result = timeout(
        setup_timeout,
        get_server_stream(
            &mut conn,
            &session,
            key.clone(),
            conn_id,
            task_sender.clone(),
        ),
    )
    .await
    .unwrap_or_else(|_| {
        Err(super::error::Error::ClientConnSetupTimeout {
            key: key.clone(),
            conn_id,
            timeout: setup_timeout,
        })
    });
    let ReadyStream {
        mut server_stream,
        server_session,
        server_id,
        codec_key,
        is_datagram,
        data_protocol: server_data_protocol,
    } = match setup_result {
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
    DataCodec::validate_offer(server_data_protocol, server_session.protocol()).context(
        ClientConnCreateHeaderToolSnafu {
            tool: "server data protocol",
        },
    )?;
    let client_codec = codec_key
        .map(|key| DataCodec::negotiate(key, data_protocol, session.protocol()))
        .transpose()
        .context(ClientConnCreateHeaderToolSnafu {
            tool: "client data codec",
        })?;
    // Each leg gets independent random material, even when one or both peers
    // need the legacy format. Re-encryption must never repeat the other leg's key.
    let server_codec = codec_key
        .map(|_| {
            DataCodec::negotiate(
                gen_random_key(),
                server_data_protocol,
                server_session.protocol(),
            )
        })
        .transpose()
        .context(ClientConnCreateHeaderToolSnafu {
            tool: "server data codec",
        })?;
    let server_cancellation = server_session
        .context()
        .map_err(|error| super::error::Error::ClientConnAuthInactive {
            detail: error.to_string(),
        })?
        .cancellation_token()
        .map_err(|error| super::error::Error::ClientConnAuthInactive {
            detail: error.to_string(),
        })?;

    let forwarding =
        async {
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

            timeout(setup_timeout.saturating_sub(prev_time.elapsed()), async {
                write_subscribe_response(
                    &mut conn,
                    &session,
                    &key,
                    conn_id,
                    server_id,
                    client_codec,
                    is_datagram,
                )
                .await?;

                // response message to server to indicate that stream handling has finished
                {
                    let mut msg_writer = server_session
                        .response_writer(&mut server_stream)
                        .context(ClientConnCreateHeaderToolSnafu { tool: "writer" })?;
                    let msg = PbConnResponse::Stream {
                        codec_key: server_codec.map(DataCodec::key),
                        data_protocol: server_codec.and_then(DataCodec::protocol),
                    }
                    .encode()
                    .context(ClientConnEncodeStreamRespSnafu {
                        key: key.clone(),
                        conn_id,
                    })?;
                    msg_writer
                        .write_msg(&msg)
                        .await
                        .context(ClientConnWriteStreamRespSnafu {
                            key: key.clone(),
                            conn_id,
                        })?;
                }

                Ok(())
            })
            .await
            .unwrap_or_else(|_| {
                Err(super::error::Error::ClientConnSetupTimeout {
                    key: key.clone(),
                    conn_id,
                    timeout: setup_timeout,
                })
            })?;
            let (mut client_reader, mut client_writer) = conn.split();
            let (mut server_reader, mut server_writer) = server_stream.split();

            let client_framing = session.framing_key();
            let server_framing = server_session.framing_key();
            if is_datagram {
                match client_codec.zip(server_codec) {
                    Some((client_codec, server_codec)) => {
                        let (client_decoder, client_encoder) = client_codec
                            .relay_codecs()
                            .context(ClientConnCreateHeaderToolSnafu {
                                tool: "client codecs",
                            })?;
                        let (server_decoder, server_encoder) = server_codec
                            .relay_codecs()
                            .context(ClientConnCreateHeaderToolSnafu {
                                tool: "server codecs",
                            })?;
                        start_datagram_forward(
                            CodecDatagramReader::new(&mut client_reader, client_decoder)
                                .with_checksum_key(client_framing),
                            CodecDatagramWriter::new(&mut client_writer, client_encoder)
                                .with_checksum_key(client_framing),
                            CodecDatagramReader::new(&mut server_reader, server_decoder)
                                .with_checksum_key(server_framing),
                            CodecDatagramWriter::new(&mut server_writer, server_encoder)
                                .with_checksum_key(server_framing),
                        )
                        .await;
                    }
                    None => {
                        start_datagram_forward(
                            NormalDatagramReader::new(&mut client_reader)
                                .with_checksum_key(client_framing),
                            NormalDatagramWriter::new(&mut client_writer)
                                .with_checksum_key(client_framing),
                            NormalDatagramReader::new(&mut server_reader)
                                .with_checksum_key(server_framing),
                            NormalDatagramWriter::new(&mut server_writer)
                                .with_checksum_key(server_framing),
                        )
                        .await;
                    }
                }
            } else {
                match client_codec.zip(server_codec) {
                    Some((client_codec, server_codec)) => {
                        let (client_decoder, client_encoder) = client_codec
                            .relay_codecs()
                            .context(ClientConnCreateHeaderToolSnafu {
                                tool: "client codecs",
                            })?;
                        let (server_decoder, server_encoder) = server_codec
                            .relay_codecs()
                            .context(ClientConnCreateHeaderToolSnafu {
                                tool: "server codecs",
                            })?;
                        start_forward(
                            CodecForwardReader::new(&mut client_reader, client_decoder)
                                .with_checksum_key(client_framing),
                            CodecForwardWriter::new(&mut client_writer, client_encoder)
                                .with_checksum_key(client_framing),
                            CodecForwardReader::new(&mut server_reader, server_decoder)
                                .with_checksum_key(server_framing),
                            CodecForwardWriter::new(&mut server_writer, server_encoder)
                                .with_checksum_key(server_framing),
                        )
                        .await;
                    }
                    None => {
                        start_forward(
                            NormalForwardReader::new(&mut client_reader),
                            NormalForwardWriter::new(&mut client_writer),
                            NormalForwardReader::new(&mut server_reader),
                            NormalForwardWriter::new(&mut server_writer),
                        )
                        .await;
                    }
                }
            }

            Ok(())
        };
    let result = tokio::select! {
            result = forwarding => result,
            _ = server_cancellation.cancelled() => {
                tracing::info!(
                    event = "connection_auth_expired",
                    key = %key,
                    client_conn_id = %conn_id,
                    server_conn_id = %server_id,
                    "closing active data stream because its registering credential expired or was revoked"
                );
                Ok(())
            }
    };
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

async fn suspect_server_conn(
    task_sender: &ManagerTaskSender,
    key: ImutableKey,
    conn_id: RemoteConnId,
    generation: u64,
) {
    if task_sender
        .send(ManagerTask::SuspectServerConn {
            key,
            conn_id,
            generation,
        })
        .await
        .is_err()
    {
        tracing::debug!(
            event = "server_suspect_check_skipped",
            conn_id = %conn_id,
            "skip server suspect check because manager channel is closed"
        );
    }
}

async fn write_subscribe_response(
    conn: &mut TcpStream,
    session: &ServerHeaderSession,
    key: &ImutableKey,
    conn_id: RemoteConnId,
    server_id: RemoteConnId,
    codec: Option<DataCodec>,
    is_datagram: bool,
) -> Result<()> {
    let mut msg_writer = session
        .response_writer(conn)
        .context(ClientConnCreateHeaderToolSnafu { tool: "writer" })?;
    let msg = PbConnResponse::Subcribe {
        codec_key: codec.map(DataCodec::key),
        data_protocol: codec.and_then(DataCodec::protocol),
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
        codec_enabled = codec.is_some(),
        data_protocol = codec.and_then(DataCodec::protocol),
        "subscribe response written to client"
    );
    Ok(())
}

struct ReadyStream {
    server_stream: TcpStream,
    server_session: ServerHeaderSession,
    server_id: RemoteConnId,
    codec_key: Option<AesKeyType>,
    is_datagram: bool,
    data_protocol: Option<u16>,
}

async fn get_server_stream(
    conn: &mut TcpStream,
    session: &ServerHeaderSession,
    key: ImutableKey,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
) -> Result<ReadyStream> {
    let (tx, rx) = kanal::bounded_async(DEFAULT_CLIENT_CHAN_CAP);
    let ready_timeout = stream_ready_timeout();
    let recovery_timeout = stream_recovery_timeout().max(
        pb_mapper_core::config::control_suspect_grace()
            + stream_ack_timeout()
            + Duration::from_millis(200),
    );
    let recovery_deadline = Instant::now() + recovery_timeout;
    let mut excluded_server_conns = Vec::new();

    'attempt: loop {
        task_sender
            .send(ManagerTask::Subcribe {
                key: key.clone(),
                conn_id,
                conn_sender: tx.clone(),
                excluded_server_conns: excluded_server_conns.clone(),
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
            excluded_server_conns = ?excluded_server_conns,
            "subscribe task sent to manager"
        );

        let response_deadline = tokio::time::Instant::now() + CLIENT_CONN_CONTROL_TIMEOUT;
        let resp = loop {
            let resp = match tokio::time::timeout_at(response_deadline, rx.recv()).await {
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
            // An ACK/data socket from the previous candidate can arrive while
            // the next selection is queued. It cannot become that selection's
            // response; dropping it preserves the generation boundary.
            if matches!(
                resp,
                ConnTask::StreamAck { .. } | ConnTask::StreamResp { .. }
            ) {
                continue;
            }
            break resp;
        };

        let (codec_key, is_datagram, server_conn_id, server_generation, ack_timeout) = match resp {
            ConnTask::SubcribeResp {
                ack_timeout,
                need_codec,
                is_datagram,
                server_conn_id,
                server_generation,
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
                    server_generation,
                    need_codec,
                    is_datagram,
                    codec_enabled = codec_key.is_some(),
                    "subscribe response received from manager"
                );
                (
                    codec_key,
                    is_datagram,
                    server_conn_id,
                    server_generation,
                    ack_timeout,
                )
            }
            ConnTask::SubcribeFailed {
                code,
                reason,
                retryable,
            } => {
                tracing::warn!(
                    event = "subscribe_failed",
                    key = %key,
                    client_conn_id = %conn_id,
                    reason = %reason,
                    "subscribe failed before stream forwarding"
                );
                write_subscribe_error(conn, session, &code, &reason, retryable).await?;
                ClientConnSubcribeFailedSnafu {
                    key: key.clone(),
                    conn_id,
                    reason,
                }
                .fail()?
            }
            ConnTask::SubcribeRetry { reason } => {
                if Instant::now() >= recovery_deadline {
                    tracing::warn!(
                        event = "subscribe_recovery_timeout",
                        key = %key,
                        client_conn_id = %conn_id,
                        reason = %reason,
                        recovery_timeout = ?recovery_timeout,
                        "subscribe recovery timed out"
                    );
                    ClientConnSubcribeFailedSnafu {
                        key: key.clone(),
                        conn_id,
                        reason: reason.clone(),
                    }
                    .fail()?
                }
                tracing::info!(
                    event = "subscribe_waiting_for_replacement",
                    key = %key,
                    client_conn_id = %conn_id,
                    reason = %reason,
                    excluded_server_conns = ?excluded_server_conns,
                    recovery_timeout = ?recovery_timeout,
                    "waiting for replacement server control connection"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue 'attempt;
            }
            _ => ClientConnSubcribeRespNotMatchSnafu {
                key: key.clone(),
                conn_id,
            }
            .fail()?,
        };

        let resp = match timeout(ack_timeout, rx.recv()).await {
            Ok(resp) => resp.context(ClientConnRecvStreamSnafu {
                key: key.clone(),
                conn_id,
            })?,
            Err(_) => {
                tracing::warn!(
                    event = "server_stream_ack_timeout",
                    key = %key,
                    client_conn_id = %conn_id,
                    server_conn_id = %server_conn_id,
                    server_generation,
                    timeout = ?ack_timeout,
                    "timed out waiting for server stream ack"
                );
                suspect_server_conn(&task_sender, key.clone(), server_conn_id, server_generation)
                    .await;
                excluded_server_conns.push((server_conn_id, server_generation));
                continue 'attempt;
            }
        };

        match resp {
            ConnTask::StreamResp {
                data_protocol,
                server_id,
                server_generation: response_generation,
                stream,
                session: stream_session,
            } if response_generation == server_generation => {
                return Ok(ReadyStream {
                    server_stream: stream,
                    server_session: stream_session,
                    server_id,
                    codec_key,
                    is_datagram,
                    data_protocol,
                });
            }
            ConnTask::StreamAck {
                server_id,
                server_generation: response_generation,
            } if server_id == server_conn_id && response_generation == server_generation => {
                tracing::debug!(
                    event = "server_stream_ack_received",
                    key = %key,
                    client_conn_id = %conn_id,
                    server_conn_id = %server_id,
                    server_generation,
                    "server stream ack received"
                );
            }
            _ => {
                tracing::warn!(
                    event = "server_stream_ack_unexpected_task",
                    key = %key,
                    client_conn_id = %conn_id,
                    server_conn_id = %server_conn_id,
                    server_generation,
                    "unexpected task while waiting for server stream ack"
                );
                excluded_server_conns.push((server_conn_id, server_generation));
                continue 'attempt;
            }
        }

        let ready_timeout = ready_timeout.max(ack_timeout.saturating_mul(2));
        let resp = match timeout(ready_timeout, rx.recv()).await {
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
                    server_generation,
                    timeout = ?ready_timeout,
                    "timed out waiting for server stream after ack"
                );
                suspect_server_conn(&task_sender, key.clone(), server_conn_id, server_generation)
                    .await;
                excluded_server_conns.push((server_conn_id, server_generation));
                continue 'attempt;
            }
        };

        if let ConnTask::StreamResp {
            data_protocol,
            server_id,
            server_generation: response_generation,
            stream,
            session: stream_session,
        } = resp
        {
            if response_generation == server_generation {
                return Ok(ReadyStream {
                    server_stream: stream,
                    server_session: stream_session,
                    server_id,
                    codec_key,
                    is_datagram,
                    data_protocol,
                });
            }
            tracing::warn!(
                event = "server_stream_generation_mismatch",
                key = %key,
                client_conn_id = %conn_id,
                expected_server_conn_id = %server_conn_id,
                expected_generation = server_generation,
                server_conn_id = %server_id,
                response_generation,
                "ignoring stale server stream response"
            );
            excluded_server_conns.push((server_conn_id, server_generation));
            continue 'attempt;
        }

        ClientConnStreamRespNotMatchSnafu {
            key: key.clone(),
            conn_id,
        }
        .fail()?
    }
}

async fn write_subscribe_error(
    conn: &mut TcpStream,
    session: &ServerHeaderSession,
    code: &str,
    reason: &str,
    retryable: bool,
) -> Result<()> {
    let message = PbConnResponse::error(code, reason, retryable)
        .encode()
        .context(ClientConnEncodeSubcribeRespSnafu {
            key: Arc::from("<rejected>"),
            conn_id: RemoteConnId::default(),
        })?;
    let mut writer = session
        .response_writer(conn)
        .context(ClientConnCreateHeaderToolSnafu { tool: "writer" })?;
    writer
        .write_msg(&message)
        .await
        .context(ClientConnWriteSubcribeRespSnafu {
            key: Arc::from("<rejected>"),
            conn_id: RemoteConnId::default(),
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod cancellation_audit {
    use super::*;
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    #[tokio::test]
    async fn cancelled_pending_deregister_delivers_exactly_one_cleanup() {
        let (sender, mut receiver) = crate::manager::task_channel(1);
        sender.send(ManagerTask::SweepServerLeases).await.unwrap();
        let mut guard = ClientConnGuard::new(7_u32.into(), None, sender.clone(), "audit".into());
        let mut deregister = Box::pin(guard.deregister());
        poll_fn(|cx| {
            assert!(deregister.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(matches!(
            receiver.recv().await.unwrap(),
            ManagerTask::SweepServerLeases
        ));
        assert!(
            receiver.is_empty(),
            "a pending send must not deliver before it is polled"
        );
        drop(deregister);
        drop(guard);
        let delivered = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        drop(delivered);
        let duplicate =
            tokio::time::timeout(std::time::Duration::from_millis(20), receiver.recv()).await;
        assert!(
            duplicate.is_err(),
            "cancelling cleanup after channel delivery emitted a second deregistration"
        );
    }
}
