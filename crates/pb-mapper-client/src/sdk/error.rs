use std::time::Duration;

use snafu::Snafu;

use pb_mapper_protocol::command::PbErrorResponse;

/// Errors returned by the client SDK.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("{message}"))]
    InvalidConfig { message: String },
    #[snafu(display("not an administrator credential"))]
    NotAdministrator,
    #[snafu(display("invalid address `{addr}`: {source}"))]
    Address {
        addr: String,
        source: pb_mapper_core::error::Error,
    },
    #[snafu(display("connect to `{addr}` failed: {source}"))]
    Connect {
        addr: String,
        source: std::io::Error,
    },
    #[snafu(display("{code}: {message}"))]
    Remote {
        code: String,
        message: String,
        retryable: bool,
    },
    #[snafu(display("{message}"))]
    Protocol { message: String },
    #[snafu(display("timed out waiting for the tunnel to become ready after {timeout:?}"))]
    ReadyTimeout { timeout: Duration },
    #[snafu(display("administrator request timed out after {timeout:?}"))]
    TimedOut { timeout: Duration },
    #[snafu(display("tunnel failed: {reason}"))]
    TunnelFailed { reason: String },
    #[snafu(display("tunnel stopped before becoming ready"))]
    Stopped,
    #[snafu(display("{source}"))]
    Status { source: crate::client::error::Error },
    #[snafu(display("{source}"))]
    Io { source: std::io::Error },
    #[snafu(display("{message}"))]
    AuthFile { message: String },
    /// A root rotation whose outcome the caller could not learn, carrying the
    /// candidate the SDK generated on their behalf.
    ///
    /// The rotation is not idempotent: if the relay committed it and the
    /// response was lost, the candidate is the relay's active administrator key
    /// and the only one that still authenticates. It is repeated in the display
    /// text deliberately — the Node binding flattens errors to a string, and a
    /// key that only lives in a structured field would be lost there, locking
    /// the operator out of their own relay.
    #[snafu(display(
        "root rotation did not report success, and the relay may already have installed the \
         generated administrator key `{candidate}`; treat it as the active key: {message}"
    ))]
    RootRotationUncertain { candidate: String, message: String },
}

impl Error {
    pub(crate) fn invalid_config(message: impl Into<String>) -> Self {
        Self::InvalidConfig {
            message: message.into(),
        }
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol {
            message: message.into(),
        }
    }

    pub(crate) fn from_remote(error: PbErrorResponse) -> Self {
        Self::Remote {
            code: error.code,
            message: error.message,
            retryable: error.retryable,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
