use snafu::ResultExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::error::{
    CreateHeaderToolSnafu, DecodeStatusRespSnafu, EncodeStatusReqSnafu, StatusRespNotMatchSnafu,
    WriteStatusReqSnafu,
};
use crate::common::checksum::Credential;
use crate::common::config::control_io_timeout;
use crate::common::message::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
};
use crate::common::message::secure::ClientHeaderSession;

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
        PbConnResponse::Error(error) => StatusRespNotMatchSnafu {
            resp: format!("{}: {}", error.code, error.message),
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

    #[tokio::test]
    async fn get_status_times_out_when_peer_stalls_after_request() {
        std::env::set_var("PB_MAPPER_CONTROL_IO_TIMEOUT", "20ms");
        let (mut client, _server) = tokio::io::duplex(1024);

        let result = tokio::time::timeout(
            Duration::from_millis(200),
            get_status(&mut client, PbConnStatusReq::Keys),
        )
        .await
        .expect("get_status ignored PB_MAPPER_CONTROL_IO_TIMEOUT");

        std::env::remove_var("PB_MAPPER_CONTROL_IO_TIMEOUT");
        assert!(result.is_err());
    }
}
