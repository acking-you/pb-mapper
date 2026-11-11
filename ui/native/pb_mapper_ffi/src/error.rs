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

use pb_mapper_client::sdk::{Error as SdkError, QUIC_UNAVAILABLE};
use serde::{Deserialize, Serialize};

/// What kind of failure this is, for a caller that has to branch on it.
///
/// Consumers must treat an unrecognised code as [`Internal`](Self::Internal):
/// variants are added as the surface grows, and that is not a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The relay answered that it offers no QUIC, which this computer is set
    /// to reach it over: no retry reaches it, choosing TCP or giving the
    /// relay a QUIC port does.
    QuicUnavailable,
    /// A local port was already taken.
    AddressInUse,
    /// The server did not answer in time.
    Timeout,
    /// The server answered with something this version did not expect, or
    /// stored data cannot be read as what it should be: the remembered
    /// computers, or the remembered sharing grant.
    Protocol,
    /// Reading or writing local configuration failed.
    Io,
    /// The remote computer speaks another remote desktop protocol: update
    /// pb-mapper on both computers.
    VersionMismatch,
    /// The remote computer's certificate is not the one its invitation pins.
    IdentityChanged,
    /// The remote computer is already serving another viewer.
    HostBusy,
    /// The remote computer refused the invitation.
    AccessDenied,
    /// A pb-mapper setup string was given where a desktop invitation belongs.
    SetupString,
    /// The text is not a desktop invitation.
    InvalidInvitation,
    /// Input needs a freshly presented picture of the current desktop.
    NotFresh,
    /// Input was sent without control of the remote computer.
    NotControlling,
    /// Control was revoked because input backed up on the remote computer.
    InputCongestion,
    /// The remote computer offers no display modes: it cannot change its
    /// resolution, or this session cannot reach it.
    DisplayModeNotOffered,
    /// None of the remote display's modes fits this computer's display.
    DisplayModeNoFit,
    /// The remote display does not offer that resolution.
    DisplayModeNotListed,
    /// The remembered computers are locked or cannot be read.
    VaultUnavailable,
    /// Resuming would move this computer's host to the relay or namespace
    /// the settings name now, which changes its invitation: ask first.
    RelayChanged,
    /// The Windows desktop service is older than this app: set it up again.
    HelperOutdated,
    /// The Windows desktop service is not installed.
    HelperMissing,
    /// The Windows desktop service is installed but not running; it may be
    /// restarting.
    HelperStopped,
    /// The Windows desktop service serves another installed copy of the app
    /// for this user, so this one cannot use or ask it: set it up again from
    /// this one.
    HelperForeignApp,
    /// The Windows desktop service belongs to another Windows user, or its
    /// installation cannot be read: this app cannot set it up again.
    HelperOtherOwner,
    /// The running Windows desktop service cannot be shown to be the
    /// protected helper.
    HelperUnverified,
    /// Anything with no more specific answer.
    ///
    /// Also where a code this build does not recognise lands, so an older CLI
    /// talking to a newer UI degrades to a generic failure rather than failing
    /// to parse the response at all.
    #[serde(other)]
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
    vault_unavailable => VaultUnavailable,
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

/// Why nothing can reach this relay over QUIC, in the words the window shows.
pub(crate) fn no_quic() -> CtlError {
    CtlError::new(
        ErrorCode::QuicUnavailable,
        "The relay does not offer QUIC. Choose TCP in Settings, or give the relay a QUIC port",
    )
}

/// What an SDK refusal means to the window, as a coded sentence.
pub(crate) fn sdk_error(error: SdkError) -> CtlError {
    match error {
        SdkError::NotAdministrator => CtlError::invalid_argument(
            "This computer's credential is not the relay's administrator key",
        ),
        SdkError::InvalidConfig { message } => CtlError::invalid_argument(message),
        SdkError::Address { .. } => CtlError::invalid_address(error.to_string()),
        SdkError::Connect { .. } => CtlError::server_unreachable(error.to_string()),
        SdkError::TimedOut { .. } => CtlError::timeout(error.to_string()),
        SdkError::Remote { code, .. } if code == QUIC_UNAVAILABLE => no_quic(),
        SdkError::Remote { code, message, .. } => {
            CtlError::protocol(format!("The relay refused: {message} ({code})"))
        }
        SdkError::Protocol { .. }
        | SdkError::ReadyTimeout { .. }
        | SdkError::TunnelFailed { .. }
        | SdkError::Stopped
        | SdkError::Status { .. }
        | SdkError::Io { .. }
        | SdkError::AuthFile { .. }
        | SdkError::RootRotationUncertain { .. } => CtlError::protocol(error.to_string()),
    }
}

/// What the SDK said when it refused a tunnel before starting it.
pub(crate) fn sdk_refusal(error: SdkError) -> CtlError {
    CtlError::invalid_argument(error.to_string())
}
