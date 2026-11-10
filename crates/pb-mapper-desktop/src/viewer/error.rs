//! Why a viewer could not connect, or could not send input.
use std::io;

#[derive(Debug)]
pub enum ConnectError {
    /// The host's certificate is not the invitation's: trust was revoked, or
    /// this is not that computer.
    IdentityChanged,
    /// The host speaks another desktop protocol version.
    VersionMismatch,
    /// Another viewer is connected.
    Busy,
    /// The invitation's token was refused.
    Denied,
    /// The codecs, limits or invitation are invalid, here or at the host.
    InvalidRequest,
    /// The embedding app abandoned the attempt.
    Cancelled,
    Transport(io::Error),
    Protocol(String),
}
impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentityChanged => f.write_str(
                "the remote computer's identity changed; ask its owner for a new invitation",
            ),
            Self::VersionMismatch => f.write_str(
                "The other computer runs a different pb-mapper version. Update pb-mapper on both computers.",
            ),
            Self::Busy => f.write_str("the remote computer is already being viewed"),
            Self::Denied => f.write_str("the remote computer refused this invitation"),
            Self::InvalidRequest => f.write_str("invalid desktop invitation or video settings"),
            Self::Cancelled => f.write_str("desktop connection cancelled"),
            Self::Transport(error) => write!(f, "{error}"),
            Self::Protocol(message) => write!(f, "desktop protocol error: {message}"),
        }
    }
}
impl std::error::Error for ConnectError {}

#[derive(Debug)]
pub enum InputError {
    /// No current frame was presented recently, the viewer is paused, or the
    /// generation is not the current one.
    NotFresh,
    NotControlling,
    /// The input queue overflowed; control was revoked.
    Congestion,
    Closed,
    Invalid(String),
}
impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFresh => f.write_str("input needs a current, recently presented picture"),
            Self::NotControlling => f.write_str("control of the remote computer is not granted"),
            Self::Congestion => {
                f.write_str("input congestion; control released while video recovers")
            }
            Self::Closed => f.write_str("the desktop session is closed"),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for InputError {}
