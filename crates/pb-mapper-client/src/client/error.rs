use std::time::Duration;

use pb_mapper_core::error::Error as CoreError;
use snafu::Snafu;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum Error {
    // The text names its source: a tunnel that fails on it reports this
    // Display as its failure reason.
    #[snafu(display("direct setup failed: {source}"))]
    DirectSetup { source: std::io::Error },
    #[snafu(display("bind local listener error"))]
    BindLocalListener { source: std::io::Error },
    #[snafu(display("accept local listener error"))]
    AcceptLocalStream { source: std::io::Error },
    #[snafu(display("connect remote stream error"))]
    ConnectRemoteStream { source: std::io::Error },
    #[snafu(display("encode subcribe request error"))]
    EncodeSubcribeReq { source: CoreError },
    #[snafu(display("encode status request error"))]
    EncodeStatusReq { source: CoreError },
    #[snafu(display("write status request error"))]
    WriteStatusReq { source: CoreError },
    #[snafu(display("decode status response error"))]
    DecodeStatusResp { source: CoreError },
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
    WriteSubcribeReq { source: CoreError },
    #[snafu(display("decode subcribe response error"))]
    DecodeSubcribeResp { source: CoreError },
    #[snafu(display("we expected `PbConnResponse::Subcribe`,but actual response is `{resp}`"))]
    SubcribeRespNotMatch {
        // Structured representation of response
        resp: String,
    },
    #[snafu(display("relay refused subscription: {code}: {message}"))]
    SubscribeRemoteError {
        code: String,
        message: String,
        retryable: bool,
    },
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

impl Error {
    /// The relay's verdict on whether the same request could ever succeed, or
    /// `None` when this failure did not come from the relay.
    ///
    /// Transport and framing failures are `None`: nothing about a dropped
    /// connection says the relay's answer is final.
    pub fn remote_retryable(&self) -> Option<bool> {
        match self {
            Self::StatusRemoteError { retryable, .. }
            | Self::SubscribeRemoteError { retryable, .. } => Some(*retryable),
            Self::DirectSetup { .. }
            | Self::BindLocalListener { .. }
            | Self::AcceptLocalStream { .. }
            | Self::ConnectRemoteStream { .. }
            | Self::EncodeSubcribeReq { .. }
            | Self::EncodeStatusReq { .. }
            | Self::WriteStatusReq { .. }
            | Self::DecodeStatusResp { .. }
            | Self::StatusRespNotMatch { .. }
            | Self::WriteSubcribeReq { .. }
            | Self::DecodeSubcribeResp { .. }
            | Self::SubcribeRespNotMatch { .. }
            | Self::CreateHeaderTool { .. }
            | Self::ControlIoTimeout { .. } => None,
        }
    }

    /// Whether a failed subscription attempt is worth another before the
    /// stream's setup deadline: the relay could not be reached or the request
    /// not delivered, or the relay itself called its refusal retryable.
    /// Anything else would fail the same way again.
    pub(crate) fn subscribe_retryable(&self) -> bool {
        match self {
            Self::ConnectRemoteStream { .. } | Self::WriteSubcribeReq { .. } => true,
            Self::SubscribeRemoteError { retryable, .. } => *retryable,
            Self::DirectSetup { .. }
            | Self::BindLocalListener { .. }
            | Self::AcceptLocalStream { .. }
            | Self::EncodeSubcribeReq { .. }
            | Self::EncodeStatusReq { .. }
            | Self::WriteStatusReq { .. }
            | Self::DecodeStatusResp { .. }
            | Self::StatusRespNotMatch { .. }
            | Self::StatusRemoteError { .. }
            | Self::DecodeSubcribeResp { .. }
            | Self::SubcribeRespNotMatch { .. }
            | Self::CreateHeaderTool { .. }
            | Self::ControlIoTimeout { .. } => false,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why [`fetch_status_scoped`](super::fetch_status_scoped) got no status.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum FetchStatusError {
    /// The relay address named no address.
    #[snafu(display("{source}"))]
    Resolve { source: std::io::Error },
    /// No stream to the relay could be opened.
    #[snafu(display("get status stream: {source}"))]
    Connect { source: std::io::Error },
    /// The relay was reached, but the exchange failed or the relay refused it.
    ///
    /// Its text carries the source's causes, as the other variants' do: the
    /// source names only the step that failed, and the `status` command
    /// prints the text alone.
    #[snafu(display("{}", with_causes(source)))]
    Status { source: Error },
}

/// `error`'s text, then each of its causes', as `error: cause: …`, leaving
/// out a cause whose text the one before already ends with: for an error
/// that reaches its caller only as text, which would otherwise name the step
/// that failed and not why.
pub(crate) fn with_causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(current) = cause {
        let more = current.to_string();
        if !text.ends_with(&more) {
            text.push_str(": ");
            text.push_str(&more);
        }
        cause = current.source();
    }
    text
}
