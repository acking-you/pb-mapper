use snafu::Snafu;

use crate::common;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum Error {
    #[snafu(display("bind local listener error"))]
    BindLocalListener { source: common::error::Error },
    #[snafu(display("accept local listener error"))]
    AcceptLocalStream { source: common::error::Error },
    #[snafu(display("connect remote stream error"))]
    ConnectRemoteStream { source: std::io::Error },
    #[snafu(display("encode subcribe request error"))]
    EncodeSubcribeReq { source: common::error::Error },
    #[snafu(display("encode status request error"))]
    EncodeStatusReq { source: common::error::Error },
    #[snafu(display("write status request error"))]
    WriteStatusReq { source: common::error::Error },
    #[snafu(display("read status response error"))]
    ReadStatusResp { source: common::error::Error },
    #[snafu(display("decode status response error"))]
    DecodeStatusResp { source: common::error::Error },
    #[snafu(display("we expected `PbConnResponse::Status`,but actual response is `{resp}`"))]
    StatusRespNotMatch {
        // Structured representation of response
        resp: String,
    },
    #[snafu(display("write subcribe request error"))]
    WriteSubcribeReq { source: common::error::Error },
    #[snafu(display("read subcribe response error"))]
    ReadSubcribeResp { source: common::error::Error },
    #[snafu(display("decode subcribe response error"))]
    DecodeSubcribeResp { source: common::error::Error },
    #[snafu(display("we expected `PbConnResponse::Subcribe`,but actual response is `{resp}`"))]
    SubcribeRespNotMatch {
        // Structured representation of response
        resp: String,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
