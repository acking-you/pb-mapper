use snafu::Snafu;

use crate::common::{self};

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum Error {
    #[snafu(display("send register request error"))]
    SendRegisterReq { source: common::error::Error },
    #[snafu(display("encode register request error"))]
    EncodeRegisterReq { source: common::error::Error },
    #[snafu(display("read register response error"))]
    ReadRegisterResp { source: common::error::Error },
    #[snafu(display("decode register response error"))]
    DecodeRegisterResp { source: common::error::Error },
    #[snafu(display("register response not `PbConnResponse::Register`"))]
    RegisterRespNotMatch,
    #[snafu(display("read stream request error"))]
    ReadStreamReq { source: common::error::Error },
    #[snafu(display("decode stream request error"))]
    DecodeStreamReq { source: common::error::Error },
    #[snafu(display("encode pb connect stream request error"))]
    EncodePbConnStreamReq { source: common::error::Error },
    #[snafu(display("connect local stream error"))]
    ConnectLocalStream { source: common::error::Error },
    #[snafu(display("connect remote stream error"))]
    ConnectRemoteStream { source: std::io::Error },
    #[snafu(display("write pb conn stream request error"))]
    WritePbConnStreamReq { source: common::error::Error },
    #[snafu(display("read pb conn stream response error"))]
    ReadPbConnStreamResp { source: common::error::Error },
    #[snafu(display("decode pb conn stream response error"))]
    DecodePbConnStreamResp { source: common::error::Error },
    #[snafu(display("we expected `PbConnResponse::Stream`,but actual response is `{resp}`"))]
    PbConnStreamRespNotMatch {
        // Structured representation of response
        resp: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
