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
