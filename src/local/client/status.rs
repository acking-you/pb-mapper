use snafu::ResultExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::error::{
    ControlIoTimeoutSnafu, CreateHeaderToolSnafu, DecodeStatusRespSnafu, EncodeStatusReqSnafu,
    ReadStatusRespSnafu, StatusRespNotMatchSnafu, WriteStatusReqSnafu,
};
use crate::common::config::control_io_timeout;
use crate::common::message::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
};
use crate::common::message::{
    get_header_msg_reader, get_header_msg_writer, MessageReader, MessageWriter,
};

pub async fn get_status<S: AsyncReadExt + AsyncWriteExt + Send + Unpin>(
    remote_stream: &mut S,
    req: PbConnStatusReq,
) -> super::error::Result<PbConnStatusResp> {
    let timeout = control_io_timeout();
    let msg = PbConnRequest::Status(req)
        .encode()
        .context(EncodeStatusReqSnafu)?;

    // send status request
    {
        let mut msg_writer = get_header_msg_writer(remote_stream)
            .context(CreateHeaderToolSnafu { action: "writer" })?;
        match tokio::time::timeout(timeout, msg_writer.write_msg(&msg)).await {
            Ok(result) => result.context(WriteStatusReqSnafu)?,
            Err(_) => ControlIoTimeoutSnafu {
                action: "write status request",
                timeout,
            }
            .fail()?,
        }
    }

    // get status
    let mut msg_reader =
        get_header_msg_reader(remote_stream).context(CreateHeaderToolSnafu { action: "reader" })?;
    let msg = match tokio::time::timeout(timeout, msg_reader.read_msg()).await {
        Ok(result) => result.context(ReadStatusRespSnafu)?,
        Err(_) => ControlIoTimeoutSnafu {
            action: "read status response",
            timeout,
        }
        .fail()?,
    };
    let resp = PbConnResponse::decode(msg).context(DecodeStatusRespSnafu)?;
    match resp {
        PbConnResponse::Status(status) => Ok(status),
        _ => StatusRespNotMatchSnafu {
            resp: String::from_utf8_lossy(msg),
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
