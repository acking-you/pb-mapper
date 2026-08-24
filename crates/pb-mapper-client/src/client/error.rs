use std::time::Duration;

use snafu::Snafu;

// The `common::error::Error` spellings below are the source type on nearly every
// variant; aliasing keeps them as they were.
use pb_mapper_core as common;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum Error {
    #[snafu(display("bind local listener error"))]
    BindLocalListener { source: std::io::Error },
    #[snafu(display("accept local listener error"))]
    AcceptLocalStream { source: std::io::Error },
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
    /// The relay answered a status request with a structured refusal.
    ///
    /// `retryable` is the relay's own verdict and the reason this is a variant of
    /// its own: a caller that loops on every status failure would spin forever on
    /// a refusal that reconnecting cannot fix, such as a namespace the credential
    /// does not own.
    #[snafu(display("relay refused the status request: {code}: {message}"))]
    StatusRemoteError {
        code: String,
        message: String,
        retryable: bool,
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
    #[snafu(display("`{action}` failed!"))]
    GetCodec {
        // Must be `client/server-reader/writer-decode/encode`
        action: &'static str,
        source: common::error::Error,
    },
    #[snafu(display("header msg tool:`{action}` create failed!"))]
    CreateHeaderTool {
        // Must be `reader/writer`
        action: &'static str,
        source: common::error::Error,
    },
    #[snafu(display("control io `{action}` timed out after {timeout:?}"))]
    ControlIoTimeout {
        action: &'static str,
        timeout: Duration,
    },
}

impl Error {
    /// The relay's verdict on whether the same request could ever succeed, or
    /// `None` when this failure did not come from the relay.
    ///
    /// Transport and framing failures are `None`: nothing about a dropped
    /// connection says the relay's answer is final.
    pub fn remote_retryable(&self) -> Option<bool> {
        match self {
            Self::StatusRemoteError { retryable, .. } => Some(*retryable),
            _ => None,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
