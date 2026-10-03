use crate::diagnostics::{Diagnostics, RecoveryFailure};
use crate::endpoint::RelayEndpoint;
use std::sync::Arc;

use crate::recovery::RecoveryTiming;
use snafu::ResultExt;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, OwnedSemaphorePermit, mpsc};
use tokio::time::Instant;
use tracing::{Instrument, info_span, instrument};

use super::error::{
    ConnectRemoteStreamSnafu, DecodeSubcribeRespSnafu, EncodeSubcribeReqSnafu, Result,
    SubcribeRespNotMatchSnafu, WriteSubcribeReqSnafu,
};
use crate::client::error::CreateHeaderToolSnafu;
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::control_io_timeout;
use pb_mapper_core::snafu_error_handle;
use pb_mapper_protocol::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use pb_mapper_protocol::data::{DATA_PROTOCOL_V2, DataCodec};
use pb_mapper_protocol::forward::StreamForward;
use pb_mapper_protocol::secure::ClientHeaderSession;
use uni_stream::stream::{NetworkStream, set_tcp_keep_alive, set_tcp_nodelay};

pub(super) struct StreamSetup {
    pub permit: OwnedSemaphorePermit,
    pub timing: Arc<Mutex<RecoveryTiming>>,
    pub events: mpsc::Sender<bool>,
    pub diagnostics: Diagnostics,
}

#[instrument(skip(local_stream, credential, setup))]
pub(super) async fn handle_local_stream<LocalStream: NetworkStream + StreamForward>(
    mut local_stream: LocalStream,
    key: Arc<str>,
    remote_addr: RelayEndpoint,
    keep_alive: bool,
    namespace: Option<u64>,
    credential: Credential,
    setup: StreamSetup,
) -> Result<()> {
    let started = Instant::now();
    // Retry only setup: no application byte is read until a subscription has
    // succeeded. A fresh socket/session avoids replaying payload or crypto state.
    let budget = std::time::Duration::from_secs(5).min(control_io_timeout());
    let deadline = started + budget;
    let global_permit = tokio::time::timeout_at(deadline, remote_addr.data_slots().acquire_owned())
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .ok_or(super::error::Error::ControlIoTimeout {
            action: "setup admission",
            timeout: budget,
        })?;
    let mut wake = remote_addr.wake();
    let mut retry = pb_mapper_core::timeout::RetryBackoff::default();
    let ((mut remote_stream, codec_key, client_id, server_id), setup_latency) = loop {
        let attempt_started = Instant::now();
        let attempt_budget = setup
            .timing
            .lock()
            .await
            .timeout()
            .max(std::time::Duration::from_secs(3));
        let attempt_deadline = (Instant::now() + attempt_budget).min(deadline);
        wake.acknowledge();
        let result = tokio::select! {
            () = wake.network_changed(), if Instant::now() < deadline => continue,
            result = tokio::time::timeout_at(
            attempt_deadline,
            subscribe(&key, remote_addr.clone(), keep_alive, namespace, credential, LocalStream::supports_data_v2()),
        ) => result,
        };
        match result {
            Ok(Ok(ready)) => break (ready, attempt_started.elapsed()),
            Ok(Err(error)) => {
                let retryable = matches!(
                    &error,
                    super::error::Error::ConnectRemoteStream { .. }
                        | super::error::Error::WriteSubcribeReq { .. }
                        | super::error::Error::SubscribeRemoteError {
                            retryable: true,
                            ..
                        }
                );
                if error.remote_retryable().is_some() {
                    remote_addr.protocol_succeeded();
                } else {
                    remote_addr.transport_failed();
                }
                if !retryable || Instant::now() >= deadline {
                    return Err(error);
                }
            }
            Err(_) => {
                remote_addr.transport_failed();
                setup
                    .diagnostics
                    .failed(RecoveryFailure::Timeout, std::time::Duration::ZERO);
                setup.timing.lock().await.timed_out();
                if Instant::now() >= deadline {
                    return super::error::ControlIoTimeoutSnafu {
                        action: "subscribe",
                        timeout: budget,
                    }
                    .fail();
                }
            }
        }
        tokio::select! {
            () = tokio::time::sleep_until((Instant::now() + crate::recovery::jitter(retry.next_delay())).min(deadline)) => {},
            () = wake.changed() => retry.reset(),
        }
    };
    drop(global_permit);
    remote_addr.protocol_succeeded();
    setup.diagnostics.succeeded(setup_latency);
    setup.timing.lock().await.record(setup_latency);
    let _ = setup.events.try_send(true);
    drop(setup.permit);
    let span = info_span!("forward", "client:{client_id} <-> server_id:{server_id}");
    let (client_reader, client_writer) = local_stream.split();
    let (server_reader, server_writer) = remote_stream.split();
    snafu_error_handle!(
        <LocalStream as StreamForward>::forward_local_to_remote_with_codec(
            codec_key,
            *credential.key(),
            client_reader,
            client_writer,
            server_reader,
            server_writer,
        )
        .instrument(span)
        .await
    );
    Ok(())
}

async fn subscribe(
    key: &str,
    remote_addr: RelayEndpoint,
    keep_alive: bool,
    namespace: Option<u64>,
    credential: Credential,
    offered_v2: bool,
) -> Result<(TcpStream, Option<DataCodec>, u32, u32)> {
    let mut remote_stream = remote_addr
        .connect()
        .await
        .context(ConnectRemoteStreamSnafu)?;

    if keep_alive {
        snafu_error_handle!(
            set_tcp_keep_alive(&remote_stream),
            "remote stream set keepalive"
        );
    }
    snafu_error_handle!(set_tcp_nodelay(&remote_stream), "remote stream set nodelay");

    // start subcribe
    let (codec_key, client_id, server_id) = {
        let timeout = control_io_timeout();
        // handle request
        let request = match namespace {
            Some(namespace) => PbConnRequest::SubcribeScoped {
                data_protocol: offered_v2.then_some(DATA_PROTOCOL_V2),
                key: key.to_string(),
                namespace,
            },
            None => PbConnRequest::Subcribe {
                data_protocol: offered_v2.then_some(DATA_PROTOCOL_V2),
                key: key.to_string(),
            },
        };
        let msg = request.encode().context(EncodeSubcribeReqSnafu)?;
        let session = ClientHeaderSession::new_v2(&credential)
            .context(CreateHeaderToolSnafu { action: "session" })?;
        let response = session
            .exchange(&mut remote_stream, &msg, timeout)
            .await
            .context(WriteSubcribeReqSnafu)?;
        let resp = PbConnResponse::decode(&response).context(DecodeSubcribeRespSnafu)?;
        match resp {
            PbConnResponse::Subcribe {
                data_protocol,
                codec_key,
                client_id,
                server_id,
            } => (
                DataCodec::from_response(codec_key, data_protocol, offered_v2).context(
                    CreateHeaderToolSnafu {
                        action: "data codec",
                    },
                )?,
                client_id,
                server_id,
            ),
            PbConnResponse::Error(error) => super::error::SubscribeRemoteSnafu {
                code: error.code,
                message: error.message,
                retryable: error.retryable,
            }
            .fail()?,
            resp => SubcribeRespNotMatchSnafu {
                resp: format!("{resp:?}"),
            }
            .fail()?,
        }
    };
    Ok((remote_stream, codec_key, client_id, server_id))
}
