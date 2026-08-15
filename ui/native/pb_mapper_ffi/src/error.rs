//! The error type crossing the FFI boundary.
//!
//! Every failure carries a [`ErrorCode`] alongside its sentence. The sentence is
//! for a person and is the same text the UI has always shown; the code is for a
//! script, which should not have to match on prose that translation or a
//! reworded message could change underneath it.
//!
//! There is deliberately **no** `From<String>`. It would make every existing
//! `format!` site compile untouched by defaulting to [`ErrorCode::Internal`],
//! and then nothing would ever be classified — the compiler's refusal is what
//! makes each site pick a code.

use std::fmt;

use serde::Serialize;

/// What kind of failure this is, for a caller that has to branch on it.
///
/// Consumers must treat an unrecognised code as [`Internal`](Self::Internal):
/// variants are added as the surface grows, and that is not a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// No such service, connection, or running server.
    NotFound,
    /// Something with this name is already there.
    AlreadyExists,
    /// Someone else is setting this key up right now. See `KeyClaim`.
    AlreadyInProgress,
    /// An address failed to parse or resolve.
    InvalidAddress,
    /// An argument was rejected before anything was attempted.
    InvalidArgument,
    /// The pb-mapper server could not be reached.
    ServerUnreachable,
    /// A local port was already taken.
    AddressInUse,
    /// The server did not answer in time.
    Timeout,
    /// The server answered with something this version did not expect.
    Protocol,
    /// Reading or writing local configuration failed.
    Io,
    /// Anything with no more specific answer.
    Internal,
}

/// An error with a machine-readable code and a human sentence.
#[derive(Debug, Clone)]
pub struct CtlError {
    code: ErrorCode,
    message: String,
}

impl CtlError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }
}

/// One constructor per code, so a call site reads as the kind of failure it is
/// rather than as a two-argument `new`.
macro_rules! constructors {
    ($($name:ident => $code:ident),* $(,)?) => {
        impl CtlError {
            $(
                pub fn $name(message: impl Into<String>) -> Self {
                    Self::new(ErrorCode::$code, message)
                }
            )*
        }
    };
}

constructors! {
    not_found => NotFound,
    already_exists => AlreadyExists,
    already_in_progress => AlreadyInProgress,
    invalid_address => InvalidAddress,
    invalid_argument => InvalidArgument,
    server_unreachable => ServerUnreachable,
    address_in_use => AddressInUse,
    timeout => Timeout,
    protocol => Protocol,
    io => Io,
    internal => Internal,
}

impl fmt::Display for CtlError {
    /// Just the sentence. The code travels in the JSON envelope beside it, so
    /// putting it here too would double it up in every log line and toast.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CtlError {}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_message_is_what_a_person_reads() {
        let err = CtlError::address_in_use("Failed to bind local address 127.0.0.1:80");
        assert_eq!(
            err.to_string(),
            "Failed to bind local address 127.0.0.1:80",
            "Display must stay the plain sentence the UI already shows"
        );
        assert_eq!(err.code(), ErrorCode::AddressInUse);
    }

    #[test]
    fn codes_serialise_as_screaming_snake_case() {
        let json = serde_json::to_string(&ErrorCode::ServerUnreachable).unwrap();
        assert_eq!(json, "\"SERVER_UNREACHABLE\"");
    }
}
