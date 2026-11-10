use std::time::Duration;

use pb_mapper_core::error::Error as CoreError;
use snafu::Snafu;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum Error {
    #[snafu(display("send register request error"))]
    SendRegisterReq { source: CoreError },
    #[snafu(display("encode register request error"))]
    EncodeRegisterReq { source: CoreError },
    #[snafu(display("read register response error"))]
    ReadRegisterResp { source: CoreError },
    #[snafu(display("decode register response error"))]
    DecodeRegisterResp { source: CoreError },
    #[snafu(display("register response not `PbConnResponse::Register`"))]
    RegisterRespNotMatch,
    #[snafu(display("read stream request error"))]
    ReadStreamReq { source: CoreError },
    #[snafu(display("decode stream request error"))]
    DecodeStreamReq { source: CoreError },
    #[snafu(display("encode pb connect stream request error"))]
    EncodePbConnStreamReq { source: CoreError },
    #[snafu(display("connect local stream error"))]
    ConnectLocalStream { source: std::io::Error },
    #[snafu(display("connect remote stream error"))]
    ConnectRemoteStream { source: std::io::Error },
    #[snafu(display("write pb conn stream request error"))]
    WritePbConnStreamReq { source: CoreError },
    #[snafu(display("decode pb conn stream response error"))]
    DecodePbConnStreamResp { source: CoreError },
    #[snafu(display("we expected `PbConnResponse::Stream`,but actual response is `{resp}`"))]
    PbConnStreamRespNotMatch {
        // Structured representation of response
        resp: String,
    },
    #[snafu(display("Failed to write ping message"))]
    WritePingMsg { source: CoreError },
    #[snafu(display("Failed to encode ping message"))]
    EncodePingMsg { source: CoreError },
    #[snafu(display("Failed to write stream ack message"))]
    WriteStreamAckMsg { source: CoreError },
    #[snafu(display("Failed to encode stream ack message"))]
    EncodeStreamAckMsg { source: CoreError },
    #[snafu(display("control writer closed while scheduling `{action}`"))]
    ControlWriterClosed { action: &'static str },
    #[snafu(display("header msg tool:`{action}` create failed!"))]
    CreateHeaderTool {
        // What was being created: `session` or `data codec`.
        action: &'static str,
        source: CoreError,
    },
    #[snafu(display("control io `{action}` timed out after {timeout:?}"))]
    ControlIoTimeout {
        action: &'static str,
        timeout: Duration,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
