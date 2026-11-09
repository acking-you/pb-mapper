//! Why a backend failed, as a kind thread C can act on rather than a text it
//! would have to parse. A kind rides inside the `io::Error` every backend
//! call returns, so `Result` stays `io::Result`.

use crate::VideoCodec;
use serde::{Deserialize, Serialize};
use std::io;

/// Begins the text of every H.265 encoder failure on the helper pipe, which
/// still carries failures as text while the helper protocol is 6.
pub(crate) const H265_ENCODER_FAILED: &str = "h265_encoder_failed";

/// A backend failure of a known kind. Its text is the message alone, so a
/// failure reads as it did before it carried a kind.
#[derive(Debug)]
pub struct BackendError {
    kind: FailureKind,
    message: String,
}
impl BackendError {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    pub fn kind(&self) -> FailureKind {
        self.kind
    }
}
impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for BackendError {}
/// `ErrorKind::Unsupported` for `FailureKind::Unsupported`, else `Other`.
impl From<BackendError> for io::Error {
    fn from(failure: BackendError) -> Self {
        let kind = if failure.kind == FailureKind::Unsupported {
            io::ErrorKind::Unsupported
        } else {
            io::ErrorKind::Other
        };
        io::Error::new(kind, failure)
    }
}

/// The kind `error` carries, if a backend gave it one.
pub fn failure_kind(error: &io::Error) -> Option<FailureKind> {
    error
        .get_ref()?
        .downcast_ref::<BackendError>()
        .map(BackendError::kind)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureKind {
    /// Input belongs to a desktop this backend cannot reach (UAC, the lock
    /// screen) until it returns.
    SecureDesktop,
    /// The selected display is gone.
    DisplayLost,
    CaptureFailed,
    /// The hardware codec itself failed, not the capture around it.
    EncoderFailed {
        codec: VideoCodec,
    },
    Unsupported,
    PermissionMissing,
    ConsentRevoked,
}
