use snafu::ResultExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::error::{
    DecodeStatusRespSnafu, EncodeStatusReqSnafu, ReadStatusRespSnafu, StatusRespNotMatchSnafu,
    WriteStatusReqSnafu,
};
use crate::common::message::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
};
use crate::common::message::{
    MessageReader, MessageWriter, NormalMessageReader, NormalMessageWriter,
};

pub async fn get_status<S: AsyncReadExt + AsyncWriteExt + Send + Unpin>(
    remote_stream: &mut S,
    req: PbConnStatusReq,
) -> super::error::Result<PbConnStatusResp> {
    let msg = PbConnRequest::Status(req)
        .encode()
        .context(EncodeStatusReqSnafu)?;

    // send status request
    {
        let mut msg_writer = NormalMessageWriter::new(remote_stream);
        msg_writer
            .write_msg(&msg)
            .await
            .context(WriteStatusReqSnafu)?;
    }

    // get status
    let mut msg_reader = NormalMessageReader::new(remote_stream);
    let msg = msg_reader.read_msg().await.context(ReadStatusRespSnafu)?;
    let resp = PbConnResponse::decode(msg).context(DecodeStatusRespSnafu)?;
    match resp {
        PbConnResponse::Status(status) => Ok(status),
        _ => StatusRespNotMatchSnafu {
            resp: String::from_utf8_lossy(msg),
        }
        .fail(),
    }
}
