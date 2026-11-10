//! The SDK's error type.
//!
//! One flat enum, deliberately: a caller has to be able to match on what went
//! wrong without importing the error type of every layer below.

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
    /// Its text carries the source's causes: a status error names the step
    /// that failed, and a Node or app caller gets only the text.
    #[snafu(display("{}", with_causes(source)))]
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
    pub(crate) fn from_connect(error: crate::relay::ConnectError, addr: String) -> Self {
        match error {
            crate::relay::ConnectError::Transport { source } => Self::Connect { addr, source },
            crate::relay::ConnectError::Rejected { response } => Self::from_remote(response),
        }
    }

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

    /// The relay answered with something other than the `expected` response.
    pub(crate) fn unexpected(expected: &str, actual: &impl std::fmt::Debug) -> Self {
        Self::protocol(format!("expected {expected}, got {actual:?}"))
    }

    pub(crate) fn from_remote(error: PbErrorResponse) -> Self {
        Self::Remote {
            code: error.code,
            message: error.message,
            retryable: error.retryable,
        }
    }

    /// Whether this says the carrier failed, rather than that the relay
    /// answered with a refusal.
    pub(crate) fn is_carrier_failure(&self) -> bool {
        match self {
            Self::Remote { .. } => false,
            Self::Status { source } => source.remote_retryable().is_none(),
            Self::InvalidConfig { .. }
            | Self::NotAdministrator
            | Self::Address { .. }
            | Self::Connect { .. }
            | Self::Protocol { .. }
            | Self::ReadyTimeout { .. }
            | Self::TimedOut { .. }
            | Self::TunnelFailed { .. }
            | Self::Stopped
            | Self::Io { .. }
            | Self::AuthFile { .. }
            | Self::RootRotationUncertain { .. } => true,
        }
    }

    /// Whether a request that failed before any of it left this process may
    /// be sent again: anything but the relay refusing it for good.
    pub(crate) fn may_resend_unsent(&self) -> bool {
        match self {
            Self::Remote { retryable, .. } => *retryable,
            Self::InvalidConfig { .. }
            | Self::NotAdministrator
            | Self::Address { .. }
            | Self::Connect { .. }
            | Self::Protocol { .. }
            | Self::ReadyTimeout { .. }
            | Self::TimedOut { .. }
            | Self::TunnelFailed { .. }
            | Self::Stopped
            | Self::Status { .. }
            | Self::Io { .. }
            | Self::AuthFile { .. }
            | Self::RootRotationUncertain { .. } => true,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A refusal proves the carrier works, and only one the relay calls final
    /// stops a request that never left this process from being sent again.
    #[test]
    fn a_refusal_is_no_carrier_failure_and_only_a_final_one_stops_a_resend() {
        let refusal = |retryable| Error::Remote {
            code: "refused".into(),
            message: String::new(),
            retryable,
        };
        assert!(!refusal(true).is_carrier_failure());
        assert!(refusal(true).may_resend_unsent());
        assert!(!refusal(false).may_resend_unsent());
        let unreachable = Error::Connect {
            addr: "127.0.0.1:1".into(),
            source: std::io::ErrorKind::ConnectionRefused.into(),
        };
        assert!(unreachable.is_carrier_failure());
        assert!(unreachable.may_resend_unsent());
    }

    /// A status query that failed in transport says why in its text, which
    /// is all a Node or app caller gets of it, and keeps its source.
    #[test]
    fn a_failed_status_query_names_its_cause() {
        let cause = || std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset mid-reply");
        let error = Error::Status {
            source: crate::client::error::Error::WriteStatusReq {
                source: pb_mapper_core::error::Error::MsgNetworkReadBody { source: cause() },
            },
        };
        assert!(error.to_string().contains(&cause().to_string()), "{error}");
        assert!(std::error::Error::source(&error).is_some());
    }
}
