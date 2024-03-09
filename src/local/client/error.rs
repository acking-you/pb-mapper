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
