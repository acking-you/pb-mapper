//! One forwarded session: from the relay's stream request to the local service.

use std::sync::Arc;

use snafu::ResultExt;
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use super::error::{
    ConnectLocalStreamSnafu, ConnectRemoteStreamSnafu, ControlIoTimeoutSnafu,
    DecodePbConnStreamRespSnafu, EncodePbConnStreamReqSnafu, PbConnStreamRespNotMatchSnafu, Result,
    StreamRemoteSnafu, WritePbConnStreamReqSnafu,
};
use crate::diagnostics::Diagnostics;
use crate::endpoint::RelayEndpoint;
use crate::server::error::CreateHeaderToolSnafu;
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::{ResolvedAddrs, control_io_timeout};
use pb_mapper_core::snafu_error_handle;
use pb_mapper_protocol::command::{MessageSerializer, PbConnRequest, PbConnResponse};
use pb_mapper_protocol::data::{DATA_PROTOCOL_V2, DataCodec};
use pb_mapper_protocol::direct::SessionPath;
use pb_mapper_protocol::forward::StreamForward;
use pb_mapper_protocol::secure::ClientHeaderSession;
use uni_stream::stream::{StreamProvider, StreamSplit};

/// Where one forwarded session dials, both ends resolved.
///
/// Each end is the full candidate list rather than one address, so a session
/// opened long after registration still has every record to try.
#[derive(Clone, Debug)]
pub(super) struct StreamConnect {
    pub(super) local_addr: ResolvedAddrs,
    pub(super) remote_addr: ResolvedAddrs,
    pub(super) keep_alive: bool,
    pub(super) namespace: Option<u64>,
    pub(super) credential: Credential,
    pub(super) idle_timeout: Option<std::time::Duration>,
    /// Whether the local service is UDP.
    pub(super) is_datagram: bool,
}

/// Starts one forwarded session in a worker's set of sessions.
///
/// The only step of a registration that names the local stream type, so the
/// control pool, its workers and their attempts are compiled once rather than
/// once per stream type: they carry [`spawn_stream`] for the type the tunnel
/// was started with, as a plain function pointer called once per session.
pub(super) type SpawnStream = fn(&mut JoinSet<()>, StreamJob);

/// What one forwarded session needs, handed from the control loop that
/// accepted the relay's stream request to the session's own task.
pub(super) struct StreamJob {
    pub(super) key: Arc<str>,
    pub(super) client_id: u32,
    pub(super) server_generation: u64,
    pub(super) target: StreamConnect,
    pub(super) permit: OwnedSemaphorePermit,
    pub(super) endpoint: RelayEndpoint,
    pub(super) diagnostics: Diagnostics,
    pub(super) shutdown: CancellationToken,
}

/// The [`SpawnStream`] for `LocalStream`.
///
/// Tracked rather than detached: a cancelled registration must take its
/// in-flight forwarded sessions with it, so `stop()` returning means no
/// stream of this tunnel is still moving bytes.
pub(super) fn spawn_stream<LocalStream: StreamProvider>(tasks: &mut JoinSet<()>, job: StreamJob)
where
    LocalStream::Item: StreamForward,
{
    let StreamJob {
        key,
        client_id,
        server_generation,
        target,
        permit,
        endpoint,
        diagnostics,
        shutdown,
    } = job;
    tasks.spawn(async move {
        let forward = handle_stream_on_endpoint::<LocalStream>(
            key,
            client_id,
            server_generation,
            target,
            permit,
            endpoint,
            diagnostics,
        );
        tokio::select! {
            () = shutdown.cancelled() => {}
            result = forward => snafu_error_handle!(result),
        }
    });
}

/// Forward a stream using the carrier and relay selected by its registration,
/// counted as one of the tunnel's relay sessions once it starts forwarding.
#[instrument(skip(connect, setup_permit, endpoint, diagnostics), fields(key = %key))]
async fn handle_stream_on_endpoint<LocalStream: StreamProvider>(
    key: Arc<str>,
    client_id: u32,
    server_generation: u64,
    connect: StreamConnect,
    setup_permit: tokio::sync::OwnedSemaphorePermit,
    endpoint: RelayEndpoint,
    diagnostics: Diagnostics,
) -> Result<()>
where
    LocalStream::Item: StreamForward,
{
    let StreamConnect {
        local_addr,
        remote_addr,
        keep_alive,
        namespace,
        credential,
        idle_timeout,
        is_datagram,
    } = connect;

    let offered_v2 = <LocalStream::Item as StreamForward>::supports_data_v2();
    let request = match namespace {
        Some(namespace) => PbConnRequest::StreamScoped {
            data_protocol: offered_v2.then_some(DATA_PROTOCOL_V2),
            key: key.to_string(),
            namespace,
            dst_id: client_id,
            server_generation,
        },
        None => PbConnRequest::Stream {
            data_protocol: offered_v2.then_some(DATA_PROTOCOL_V2),
            key: key.to_string(),
            dst_id: client_id,
            server_generation,
        },
    };
    let msg = request.encode().context(EncodePbConnStreamReqSnafu)?;

    let timeout = control_io_timeout().min(std::time::Duration::from_secs(5));
    let deadline = tokio::time::Instant::now() + timeout;
    let mut remote_stream = match tokio::time::timeout_at(
        deadline,
        endpoint.connect_addresses(&remote_addr, credential, &mut None),
    )
    .await
    {
        Ok(result) => result
            .map_err(std::io::Error::from)
            .context(ConnectRemoteStreamSnafu)?,
        Err(_) => ControlIoTimeoutSnafu {
            action: "connect remote stream",
            timeout,
        }
        .fail()?,
    };
    snafu_error_handle!(remote_stream.configure(keep_alive));

    // Use the credential captured at registration. A later UI/process key
    // change must not move these streams into another namespace.
    let codec_key = {
        let session = ClientHeaderSession::new_v2(&credential)
            .context(CreateHeaderToolSnafu { action: "session" })?;
        let response = session
            .exchange(
                &mut remote_stream,
                &msg,
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            )
            .await
            .context(WritePbConnStreamReqSnafu)?;
        let resp = PbConnResponse::decode(&response).context(DecodePbConnStreamRespSnafu)?;
        match resp {
            PbConnResponse::Stream {
                codec_key,
                data_protocol,
            } => DataCodec::from_response(codec_key, data_protocol, offered_v2).context(
                CreateHeaderToolSnafu {
                    action: "data codec",
                },
            )?,
            PbConnResponse::Error(error) => StreamRemoteSnafu {
                code: error.code,
                message: error.message,
                retryable: error.retryable,
            }
            .fail()?,
            _ => PbConnStreamRespNotMatchSnafu {
                resp: format!("{resp:?}"),
            }
            .fail()?,
        }
    };

    // start forward network traffic
    if is_datagram {
        crate::udp_timeout::publisher_udp_stream();
    }
    let mut local_stream = match tokio::time::timeout_at(
        deadline,
        LocalStream::from_addr(local_addr.as_slice()),
    )
    .await
    {
        Ok(result) => result.context(ConnectLocalStreamSnafu)?,
        Err(_) => {
            return ControlIoTimeoutSnafu {
                action: "connect local service",
                timeout,
            }
            .fail();
        }
    };
    drop(setup_permit);
    let session = diagnostics.session_opened(SessionPath::Relay, None);

    pb_mapper_protocol::with_relay_stream!(&mut remote_stream, remote_stream, {
        let (client_reader, client_writer) = remote_stream.split();
        let (server_reader, server_writer) = local_stream.split();

        // The service is this end's local side: what it writes is sent.
        let forward = <LocalStream::Item as StreamForward>::forward_local_to_remote_metered(
            codec_key,
            *credential.key(),
            server_reader,
            server_writer,
            client_reader,
            client_writer,
            Some(session.meter()),
        );
        let result = match idle_timeout {
            Some(timeout) => {
                pb_mapper_protocol::forward::with_tunnel_idle_timeout(timeout, forward).await
            }
            None => forward.await,
        };
        snafu_error_handle!(result);
    });
    Ok(())
}
