use std::time::Duration;

use pb_mapper_protocol::transport::RelayStream;
use snafu::ResultExt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::time::{Instant, timeout, timeout_at};
use tracing::instrument;

use super::error::{
    ClientConnAuthContextSnafu, ClientConnAuthInactiveSnafu, ClientConnEncodeSubcribeRespSnafu,
    ClientConnRecvStreamSnafu, ClientConnRecvSubcribeRespSnafu, ClientConnSendSubcribeSnafu,
    ClientConnStreamRespNotMatchSnafu, ClientConnSubcribeFailedSnafu,
    ClientConnSubcribeRespNotMatchSnafu, ClientConnWriteSubcribeRespSnafu,
};
use super::{ConnTask, ImutableKey, ManagerTask, ManagerTaskSender, Result};
use crate::error::{
    ClientConnCreateHeaderToolSnafu, ClientConnEncodeStreamRespSnafu,
    ClientConnWriteStreamRespSnafu,
};
use crate::guard::{ConnLease, Deregistration};
use crate::routing::SERVICE_NOT_AVAILABLE;
use crate::task::ConnTaskReceiver;
use pb_mapper_core::checksum::{AesKeyType, gen_random_key};
use pb_mapper_core::codec::{Aes256GcmDeCodec, Aes256GcmEnCodec};
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

/// A subscriber connection, returned to the manager when its socket task ends,
/// however it ends.
struct ClientDeregistration {
    key: ImutableKey,
    client_id: RemoteConnId,
    /// The provider stream socket, once one was paired with this subscriber.
    server_id: Option<RemoteConnId>,
}

impl Deregistration for ClientDeregistration {
    fn task(&self) -> ManagerTask {
        ManagerTask::DeRegisterClientConn {
            server_id: self.server_id,
            client_id: self.client_id,
        }
    }

    fn manager_closed(&self) {
        tracing::debug!(
            "skip async client deregister because manager channel is closed: key:{} client:{}",
            self.key,
            self.client_id
        );
    }

    fn dropped_manager_closed(&self) {
        tracing::debug!(
            "skip client deregister on drop because manager channel is closed: key:{} client:{}",
            self.key,
            self.client_id
        );
    }

    fn dropped_queue_full(&self) {
        tracing::warn!(
            "manager queue is full; defer client deregister: key:{} client:{}",
            self.key,
            self.client_id
        );
    }

    fn deferred_manager_closed() {
        tracing::debug!("skip deferred client deregister because manager channel is closed");
    }

    fn no_runtime() {
        tracing::warn!("cannot defer client deregister because no Tokio runtime is available");
    }
}

const DEFAULT_CLIENT_CHAN_CAP: usize = 32;

/// 1. Request server stream
/// 2. Forward the traffic between client stream and server stream
///
/// `selected` pins the registration and generation a direct setup's relay
/// fallback was granted; a plain subscription leaves the choice to the manager.
/// A pinned one waits for no replacement: the manager refuses it once its pin
/// cannot serve it. The function's name is its span's name in the logs.
#[instrument(skip(lease, task_sender, conn, session), fields(conn_id = %lease.conn_id()))]
pub(super) async fn handle_client_conn_selected(
    key: ImutableKey,
    lease: ConnLease,
    task_sender: ManagerTaskSender,
    mut conn: RelayStream,
    session: ServerHeaderSession,
    data_protocol: Option<u16>,
    selected: Option<(RemoteConnId, u64)>,
) -> Result<()> {
    let conn_id = lease.conn_id();
    let mut guard = lease.hand_to(ClientDeregistration {
        key: key.clone(),
        client_id: conn_id,
        server_id: None,
    });
    DataCodec::validate_offer(data_protocol, session.protocol()).context(
        ClientConnCreateHeaderToolSnafu {
            tool: "data protocol",
        },
    )?;
    let prev_time = Instant::now();
    // Bounds the whole wait for a stream, every wait for the manager included,
    // and on its own the refusal, if one is written after it.
    let setup_timeout = pb_mapper_core::config::control_io_timeout().min(Duration::from_secs(5));
    let mut subscriber = Subscriber {
        conn: &mut conn,
        left: None,
    };
    let setup = timeout(
        setup_timeout,
        get_server_stream(
            key.clone(),
            conn_id,
            task_sender.clone(),
            selected,
            &mut subscriber,
        ),
    )
    .await;
    let (setup_result, refusal) = match (subscriber.left, setup) {
        // A subscriber that gave up, as a client does when its own attempt
        // runs out, is answered nothing, whatever the wait came to: a stream
        // that came for it is dropped.
        (Some(source), _) | (None, Ok(Ok(Setup::Abandoned(source)))) => (
            Err(super::error::Error::ClientConnLeft {
                key: key.clone(),
                conn_id,
                source,
            }),
            None,
        ),
        (None, Ok(Ok(Setup::Ready(ready)))) => (Ok(ready), None),
        (None, Ok(Ok(Setup::Refused { refusal, reason }))) => (
            ClientConnSubcribeFailedSnafu {
                key: key.clone(),
                conn_id,
                reason,
            }
            .fail(),
            Some(refusal),
        ),
        (None, Ok(Err(error))) => (Err(error), None),
        // One still waiting, as far as the relay knows, is told to try again,
        // as one is that no registration can serve, rather than left to take
        // a closed connection for a failed transport.
        (None, Err(_)) => (
            Err(super::error::Error::ClientConnSetupTimeout {
                key: key.clone(),
                conn_id,
                timeout: setup_timeout,
            }),
            Some(PbConnResponse::error(
                SERVICE_NOT_AVAILABLE,
                format!("no registration of `{key}` served a stream within {setup_timeout:?}"),
                true,
            )),
        ),
    };
    if let Some(refusal) = refusal {
        // The subscriber's one answer, written once: a second would reuse its
        // session's first response nonce. A small first frame waits on its
        // peer only if the peer withholds even that much room.
        let written = timeout(
            setup_timeout,
            write_subscribe_error(subscriber.conn, &session, refusal, &key, conn_id),
        )
        .await;
        if !matches!(written, Ok(Ok(()))) {
            tracing::debug!(
                event = "subscribe_refusal_unsent",
                key = %key,
                client_conn_id = %conn_id,
                timed_out = written.is_err(),
                error = ?written.ok().and_then(std::result::Result::err),
                "the subscriber's refusal could not be written"
            );
        }
    }
    let ReadyStream {
        mut server_stream,
        server_session,
        server,
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
    // This connection's deregistration returns the stream's ID from here on.
    let server_id = server.transfer();
    guard.what_mut().server_id = Some(server_id);
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
        .context(ClientConnAuthContextSnafu)?
        .cancellation_token()
        .context(ClientConnAuthInactiveSnafu)?;

    let forwarding = async {
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
        let codecs = match client_codec.zip(server_codec) {
            Some((client_codec, server_codec)) => Some(RelayCodecs {
                client: client_codec
                    .relay_codecs()
                    .context(ClientConnCreateHeaderToolSnafu {
                        tool: "client codecs",
                    })?,
                server: server_codec
                    .relay_codecs()
                    .context(ClientConnCreateHeaderToolSnafu {
                        tool: "server codecs",
                    })?,
            }),
            None => None,
        };
        let framing = (session.framing_key(), server_session.framing_key());
        pb_mapper_protocol::with_relay_stream!(&mut conn, conn, {
            pb_mapper_protocol::with_relay_stream!(&mut server_stream, server_stream, {
                let (mut client_reader, mut client_writer) = conn.split();
                let (mut server_reader, mut server_writer) = server_stream.split();
                relay_legs(
                    (&mut client_reader, &mut client_writer),
                    (&mut server_reader, &mut server_writer),
                    framing,
                    is_datagram,
                    codecs,
                )
                .await;
            })
        });

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

/// Each leg's data codecs from the relay's side: a decoder for what the leg
/// sends, an encoder for what it receives.
struct RelayCodecs {
    client: (Aes256GcmDeCodec, Aes256GcmEnCodec),
    server: (Aes256GcmDeCodec, Aes256GcmEnCodec),
}

/// Forward between the subscriber's leg and the provider's until forwarding
/// ends. Generic, so the carrier pair the caller's `with_relay_stream!` picks
/// stays statically dispatched while the body is written once. Each leg's
/// frames are checksummed with its own session's framing key; a stream without
/// codecs is not framed at all.
async fn relay_legs<CR, CW, SR, SW>(
    (client_reader, client_writer): (&mut CR, &mut CW),
    (server_reader, server_writer): (&mut SR, &mut SW),
    (client_framing, server_framing): (AesKeyType, AesKeyType),
    is_datagram: bool,
    codecs: Option<RelayCodecs>,
) where
    CR: AsyncRead + Unpin,
    CW: AsyncWrite + Unpin,
    SR: AsyncRead + Unpin,
    SW: AsyncWrite + Unpin,
{
    match (is_datagram, codecs) {
        (
            true,
            Some(RelayCodecs {
                client: (client_decoder, client_encoder),
                server: (server_decoder, server_encoder),
            }),
        ) => {
            start_datagram_forward(
                CodecDatagramReader::new(client_reader, client_decoder)
                    .with_checksum_key(client_framing),
                CodecDatagramWriter::new(client_writer, client_encoder)
                    .with_checksum_key(client_framing),
                CodecDatagramReader::new(server_reader, server_decoder)
                    .with_checksum_key(server_framing),
                CodecDatagramWriter::new(server_writer, server_encoder)
                    .with_checksum_key(server_framing),
            )
            .await
        }
        (true, None) => {
            start_datagram_forward(
                NormalDatagramReader::new(client_reader).with_checksum_key(client_framing),
                NormalDatagramWriter::new(client_writer).with_checksum_key(client_framing),
                NormalDatagramReader::new(server_reader).with_checksum_key(server_framing),
                NormalDatagramWriter::new(server_writer).with_checksum_key(server_framing),
            )
            .await
        }
        (
            false,
            Some(RelayCodecs {
                client: (client_decoder, client_encoder),
                server: (server_decoder, server_encoder),
            }),
        ) => {
            start_forward(
                CodecForwardReader::new(client_reader, client_decoder)
                    .with_checksum_key(client_framing),
                CodecForwardWriter::new(client_writer, client_encoder)
                    .with_checksum_key(client_framing),
                CodecForwardReader::new(server_reader, server_decoder)
                    .with_checksum_key(server_framing),
                CodecForwardWriter::new(server_writer, server_encoder)
                    .with_checksum_key(server_framing),
            )
            .await
        }
        (false, None) => {
            start_forward(
                NormalForwardReader::new(client_reader),
                NormalForwardWriter::new(client_writer),
                NormalForwardReader::new(server_reader),
                NormalForwardWriter::new(server_writer),
            )
            .await
        }
    }
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
    conn: &mut RelayStream,
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
    server_stream: RelayStream,
    server_session: ServerHeaderSession,
    server: ConnLease,
    codec_key: Option<AesKeyType>,
    is_datagram: bool,
    data_protocol: Option<u16>,
}

/// How a subscribe's wait for a stream ended, short of an error.
enum Setup {
    Ready(ReadyStream),
    /// The manager refused the subscribe, with `refusal` for the subscriber.
    Refused {
        refusal: PbConnResponse,
        reason: String,
    },
    /// The subscriber left, for this reason, and the relay has finished what
    /// the registration it asked owed it.
    Abandoned(std::io::Error),
}

/// The subscriber's connection while the relay finds it a stream, and why
/// the subscriber left, once it has.
struct Subscriber<'a> {
    conn: &'a mut RelayStream,
    left: Option<std::io::Error>,
}

impl Subscriber<'_> {
    /// `work`'s result, noting on the way whether the subscriber leaves.
    /// Leaving does not cut `work` short: what the relay waits for on a
    /// subscriber's behalf, a registration owes it, and the relay's verdict on
    /// that registration is the same whether or not anyone still waits.
    async fn meanwhile<T>(&mut self, work: impl Future<Output = T>) -> T {
        let mut work = std::pin::pin!(work);
        if self.left.is_none() {
            tokio::select! {
                biased;
                source = subscriber_gone(self.conn) => self.left = Some(source),
                done = &mut work => return done,
            }
        }
        work.await
    }

    /// `work`'s result, unless the subscriber has left or leaves first, which
    /// drops `work` and answers why it left: for asking on the subscriber's
    /// behalf, which nothing does once it has gone.
    async fn unless_gone<T>(
        &mut self,
        work: impl Future<Output = T>,
    ) -> std::result::Result<T, std::io::Error> {
        if let Some(source) = self.left.take() {
            return Err(source);
        }
        tokio::select! {
            biased;
            source = subscriber_gone(self.conn) => Err(source),
            done = work => Ok(done),
        }
    }
}

/// Why the subscriber is gone, once its connection ends, fails or carries
/// data. A subscriber sends nothing before its subscribe answer, so any of the
/// three means it is no longer waiting for one. Dropped before it resolves, it
/// has read nothing.
async fn subscriber_gone(conn: &mut RelayStream) -> std::io::Error {
    let mut byte = [0_u8; 1];
    match conn.read(&mut byte).await {
        Ok(0) => std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "the subscriber closed its connection",
        ),
        Ok(_) => std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the subscriber sent data before its subscribe answer",
        ),
        Err(error) => error,
    }
}

/// How long after a suspected connection is retired a subscribe still waits
/// for the replacement to register.
const REPLACEMENT_MARGIN: Duration = Duration::from_millis(200);

/// Until when a subscribe that has just suspected a connection, whose
/// acknowledgement it waited `ack_timeout` for, waits for its replacement.
///
/// The relay retires that connection one grace from now, the grace being the
/// suspect grace or the connection's doubled acknowledgement budget (see
/// `on_suspect`), and a replacement registers only after that. Counted from
/// the subscribe's start instead, the wait ran out before the retirement for
/// any provider the relay had learned to wait more than 500 ms for, and for
/// any whose acknowledgement timer fired late.
///
/// The setup timeout still ends the wait, which cuts it short for a budget
/// past about 1.6 s, and so does the subscriber leaving: this can outlast a
/// client's own attempt, after which the client closes its connection and
/// tries again.
fn replacement_deadline(ack_timeout: Duration) -> Instant {
    let grace = pb_mapper_core::config::control_suspect_grace().max(ack_timeout.saturating_mul(2));
    Instant::now() + grace + REPLACEMENT_MARGIN
}

/// Find the subscriber a stream: ask the routing manager for a registration,
/// wait for its acknowledgement and then its stream, and move on to another
/// registration, or wait for a replacement, when one falls silent.
///
/// A subscriber that leaves is asked for no more: its subscribe ends with
/// [`Setup::Abandoned`] once the registration already asked has given the
/// relay what it owes, to the same deadlines and with the same verdicts as
/// for a subscriber still waiting (see [`abandon`]).
async fn get_server_stream(
    key: ImutableKey,
    conn_id: RemoteConnId,
    task_sender: ManagerTaskSender,
    selected: Option<(RemoteConnId, u64)>,
    subscriber: &mut Subscriber<'_>,
) -> Result<Setup> {
    let (sender, receiver) = kanal::bounded_async(DEFAULT_CLIENT_CHAN_CAP);
    let ready_timeout = stream_ready_timeout();
    let recovery_timeout = stream_recovery_timeout().max(
        pb_mapper_core::config::control_suspect_grace() + stream_ack_timeout() + REPLACEMENT_MARGIN,
    );
    let mut recovery_deadline = Instant::now() + recovery_timeout;
    let mut excluded_server_conns = Vec::new();
    // The generation of the registration last asked for a stream for this
    // subscribe, whose answers the routing manager passes on to it until
    // another is asked.
    let mut asked = None;

    'attempt: loop {
        let request = ManagerTask::Subcribe {
            key: key.clone(),
            conn_id,
            conn_sender: sender.clone(),
            excluded_server_conns: excluded_server_conns.clone(),
            selected,
        };
        let sent = match subscriber.unless_gone(task_sender.send(request)).await {
            Ok(sent) => sent,
            Err(left) => return Ok(abandon(&receiver, asked, recovery_deadline, left).await),
        };
        sent.context(ClientConnSendSubcribeSnafu {
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

        // Bounded by the caller's setup timeout, which covers this whole exchange.
        let resp = loop {
            let resp = subscriber.meanwhile(receiver.recv()).await.context(
                ClientConnRecvSubcribeRespSnafu {
                    key: key.clone(),
                    conn_id,
                },
            )?;
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
                asked = Some(server_generation);
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
                let refusal = PbConnResponse::error(code, &reason, retryable);
                return Ok(Setup::Refused { refusal, reason });
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
                    // What a subscribe that may not wait for a replacement
                    // is told at once.
                    let refusal = PbConnResponse::error(SERVICE_NOT_AVAILABLE, &reason, true);
                    return Ok(Setup::Refused { refusal, reason });
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
                if let Err(left) = subscriber
                    .unless_gone(tokio::time::sleep(Duration::from_millis(50)))
                    .await
                {
                    return Ok(abandon(&receiver, asked, recovery_deadline, left).await);
                }
                continue 'attempt;
            }
            _ => ClientConnSubcribeRespNotMatchSnafu {
                key: key.clone(),
                conn_id,
            }
            .fail()?,
        };

        // As a rule the acknowledgement, or the stream, which implies it.
        let ack_due = Instant::now() + ack_timeout;
        let Ok(resp) = subscriber
            .meanwhile(timeout_at(ack_due, receiver.recv()))
            .await
        else {
            tracing::warn!(
                event = "server_stream_ack_timeout",
                key = %key,
                client_conn_id = %conn_id,
                server_conn_id = %server_conn_id,
                server_generation,
                timeout = ?ack_timeout,
                "timed out waiting for server stream ack"
            );
            // Out of rotation at once, for its silence.
            suspect_server_conn(&task_sender, key.clone(), server_conn_id, server_generation).await;
            excluded_server_conns.push((server_conn_id, server_generation));
            recovery_deadline = recovery_deadline.max(replacement_deadline(ack_timeout));
            continue 'attempt;
        };
        let resp = resp.context(ClientConnRecvStreamSnafu {
            key: key.clone(),
            conn_id,
        })?;

        match resp {
            ConnTask::StreamResp {
                data_protocol,
                server,
                server_generation: response_generation,
                stream,
                session: stream_session,
            } if response_generation == server_generation => {
                return Ok(Setup::Ready(ReadyStream {
                    server_stream: stream,
                    server_session: stream_session,
                    server,
                    codec_key,
                    is_datagram,
                    data_protocol,
                }));
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
        let resp = match subscriber
            .meanwhile(timeout(ready_timeout, receiver.recv()))
            .await
        {
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
                recovery_deadline = recovery_deadline.max(replacement_deadline(ack_timeout));
                continue 'attempt;
            }
        };

        if let ConnTask::StreamResp {
            data_protocol,
            server,
            server_generation: response_generation,
            stream,
            session: stream_session,
        } = resp
        {
            if response_generation == server_generation {
                return Ok(Setup::Ready(ReadyStream {
                    server_stream: stream,
                    server_session: stream_session,
                    server,
                    codec_key,
                    is_datagram,
                    data_protocol,
                }));
            }
            tracing::warn!(
                event = "server_stream_generation_mismatch",
                key = %key,
                client_conn_id = %conn_id,
                expected_server_conn_id = %server_conn_id,
                expected_generation = server_generation,
                server_conn_id = %server.conn_id(),
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

/// End a subscribe whose subscriber has left, for the reason `left`.
///
/// Nothing is asked on its behalf any more, but the registration asked last,
/// of generation `asked`, still gets the verdict it would with the subscriber
/// waiting: until `until`, the end of the subscribe's recovery, the subscribe
/// keeps its place, so that the routing manager still takes that
/// registration's late answer for it. An acknowledgement teaches the
/// registration's budget, and either answer readmits it. The answer ends the
/// wait, and a stream is dropped.
async fn abandon(
    receiver: &ConnTaskReceiver,
    asked: Option<u64>,
    until: Instant,
    left: std::io::Error,
) -> Setup {
    if let Some(generation) = asked {
        let answered = async {
            while let Ok(answer) = receiver.recv().await {
                if let ConnTask::StreamAck {
                    server_generation, ..
                }
                | ConnTask::StreamResp {
                    server_generation, ..
                } = answer
                    && server_generation == generation
                {
                    break;
                }
            }
        };
        let _ = timeout_at(until, answered).await;
    }
    Setup::Abandoned(left)
}

async fn write_subscribe_error(
    conn: &mut RelayStream,
    session: &ServerHeaderSession,
    refusal: PbConnResponse,
    key: &ImutableKey,
    conn_id: RemoteConnId,
) -> Result<()> {
    let message = refusal
        .encode()
        .context(ClientConnEncodeSubcribeRespSnafu {
            key: key.clone(),
            conn_id,
        })?;
    let mut writer = session
        .response_writer(conn)
        .context(ClientConnCreateHeaderToolSnafu { tool: "writer" })?;
    writer
        .write_msg(&message)
        .await
        .context(ClientConnWriteSubcribeRespSnafu {
            key: key.clone(),
            conn_id,
        })
}
