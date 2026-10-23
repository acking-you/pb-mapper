use snafu::ResultExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::error::{
    CreateHeaderToolSnafu, DecodeStatusRespSnafu, EncodeStatusReqSnafu, StatusRemoteSnafu,
    StatusRespNotMatchSnafu, WriteStatusReqSnafu,
};
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::control_io_timeout;
use pb_mapper_protocol::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
};
use pb_mapper_protocol::secure::ClientHeaderSession;

pub async fn get_status<S: AsyncReadExt + AsyncWriteExt + Send + Unpin>(
    remote_stream: &mut S,
    req: PbConnStatusReq,
) -> super::error::Result<PbConnStatusResp> {
    get_status_scoped(remote_stream, req, None).await
}

pub async fn get_status_scoped<S: AsyncReadExt + AsyncWriteExt + Send + Unpin>(
    remote_stream: &mut S,
    req: PbConnStatusReq,
    namespace: Option<u64>,
) -> super::error::Result<PbConnStatusResp> {
    let session =
        ClientHeaderSession::from_process().context(CreateHeaderToolSnafu { action: "session" })?;
    get_status_with_session(remote_stream, req, namespace, session).await
}

pub async fn get_status_with_credential<S: AsyncReadExt + AsyncWriteExt + Send + Unpin>(
    remote_stream: &mut S,
    req: PbConnStatusReq,
    namespace: Option<u64>,
    credential: &Credential,
) -> super::error::Result<PbConnStatusResp> {
    let session = ClientHeaderSession::new_v2(credential)
        .context(CreateHeaderToolSnafu { action: "session" })?;
    get_status_with_session(remote_stream, req, namespace, session).await
}

async fn get_status_with_session<S: AsyncReadExt + AsyncWriteExt + Send + Unpin>(
    remote_stream: &mut S,
    req: PbConnStatusReq,
    namespace: Option<u64>,
    session: ClientHeaderSession,
) -> super::error::Result<PbConnStatusResp> {
    let timeout = control_io_timeout();
    let request = match namespace {
        Some(namespace) => PbConnRequest::StatusScoped {
            status: req,
            namespace,
        },
        None => PbConnRequest::Status(req),
    };
    let msg = request.encode().context(EncodeStatusReqSnafu)?;
    let response = session
        .exchange(remote_stream, &msg, timeout)
        .await
        .context(WriteStatusReqSnafu)?;
    let resp = PbConnResponse::decode(&response).context(DecodeStatusRespSnafu)?;
    match resp {
        PbConnResponse::Status(status) => Ok(status),
        // Kept structured rather than flattened into a message: callers retry on
        // the relay's `retryable` verdict, and a refusal it marks permanent has to
        // end the retry loop instead of feeding it.
        PbConnResponse::Error(error) => StatusRemoteSnafu {
            code: error.code,
            message: error.message,
            retryable: error.retryable,
        }
        .fail(),
        other => StatusRespNotMatchSnafu {
            resp: format!("{other:?}"),
        }
        .fail(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn get_status_times_out_when_peer_stalls_after_request() {
        let (mut client, _server) = tokio::io::duplex(1024);
        let credential = Credential::Admin([17; 32]);
        let budget = control_io_timeout();
        let started = tokio::time::Instant::now();

        let result = tokio::time::timeout(
            budget + Duration::from_secs(1),
            get_status_with_credential(&mut client, PbConnStatusReq::Keys, None, &credential),
        )
        .await
        .expect("status exchange ignored its control I/O timeout");

        assert!(result.is_err());
        assert!(
            started.elapsed() >= budget,
            "must reach the stalled peer timeout"
        );
    }
}
