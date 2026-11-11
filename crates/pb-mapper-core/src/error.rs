use std::env::VarError;
use std::net::AddrParseError;

use snafu::Snafu;

use crate::DataLenType;
use crate::checksum::ChecksumType;

#[derive(Debug, Snafu)]
// The generated context selectors are constructed by the protocol, client, and
// server crates, so they have to be reachable from outside this one.
#[snafu(visibility(pub))]
pub enum Error {
    /// Error handling for message
    #[snafu(display("read `checksum` from network error"))]
    MsgNetworkReadCheckSum { source: std::io::Error },
    #[snafu(display("read `buffered_raw_data` from network error"))]
    MsgNetworkReadBufferdRawData { source: std::io::Error },
    #[snafu(display("read `msg_body` from network error"))]
    MsgNetworkReadBody { source: std::io::Error },
    #[snafu(display("write `checksum` to network error"))]
    MsgNetworkWriteCheckSum { source: std::io::Error },
    #[snafu(display("write `datalen` to network error"))]
    MsgNetworkWriteDatalen { source: std::io::Error },
    #[snafu(display("write `msg_body` to network error"))]
    MsgNetworkWriteBody { source: std::io::Error },
    #[snafu(display("write `codec_msg` to network error"))]
    MsgNetworkWriteCodecMsg { source: std::io::Error },
    #[snafu(display("write `codec_tag` to network error"))]
    MsgNetworkWriteCodecTag { source: std::io::Error },
    #[snafu(display(
        "`datalen` exceeded! the length must be less than {max}, but the actual length is {actual}"
    ))]
    MsgDatalenExceeded {
        actual: DataLenType,
        max: DataLenType,
    },
    #[snafu(display(
        "`datalen` not valid,`datalen:{datalen}` doesn't pass the `checksum:{checksum}`"
    ))]
    MsgDatalenValidate {
        datalen: DataLenType,
        checksum: ChecksumType,
    },
    #[snafu(display("{action} `{struct_name}` error with content:{content}"))]
    MsgSerialize {
        // must be "encode" or "decode"
        action: &'static str,
        // must be the name of the structure to be serialized,such as `PbConnRequest`
        struct_name: &'static str,
        // a redacted description of the payload, never its bytes
        content: String,
        source: serde_json::Error,
    },
    #[snafu(display("`{action}` failed with the specific error: `{detail}`"))]
    MsgCodec {
        // must be "encrypt" or "decrypt" or "create encodec" or "create decodec"
        action: &'static str,
        // specific error explanation
        detail: String,
    },
    #[snafu(display("protocol-v2 error: {detail}"))]
    MsgProtocol { detail: String },
    #[snafu(display("`{action}` forward message failed: {source}"))]
    MsgForward {
        // must be "read" or "write"
        action: &'static str,
        source: std::io::Error,
    },
    /// Error for forward
    #[snafu(display("failed to forward message to write in normal text"))]
    FwdNetworkWriteWithNormal { source: std::io::Error },
    /// Error for config
    #[snafu(display("parse socket address from string:`{string}` error"))]
    CfgParseSockAddr {
        string: String,
        source: AddrParseError,
    },
    /// A well-formed `host:port` whose host did not resolve. The reason is in
    /// the message too: the CLI and the app show this one only by its message.
    #[snafu(display("failed to resolve `{string}`: {source}"))]
    CfgResolveSockAddr {
        string: String,
        source: std::io::Error,
    },
    #[snafu(display("`PB_MAPPER_SERVER` env not set"))]
    CfgPbServerEnvNotExist { source: VarError },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn is_expected_disconnect(&self) -> bool {
        use std::io::ErrorKind;

        let is_expected = |kind: ErrorKind| {
            matches!(
                kind,
                ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::BrokenPipe
                    | ErrorKind::NotConnected
                    | ErrorKind::TimedOut
            )
        };

        match self {
            Error::MsgNetworkReadCheckSum { source }
            | Error::MsgNetworkReadBufferdRawData { source }
            | Error::MsgNetworkReadBody { source }
            | Error::MsgNetworkWriteCheckSum { source }
            | Error::MsgNetworkWriteDatalen { source }
            | Error::MsgNetworkWriteBody { source }
            | Error::MsgNetworkWriteCodecMsg { source }
            | Error::MsgNetworkWriteCodecTag { source }
            | Error::FwdNetworkWriteWithNormal { source }
            | Error::MsgForward { source, .. } => is_expected(source.kind()),
            Error::MsgDatalenExceeded { .. }
            | Error::MsgDatalenValidate { .. }
            | Error::MsgSerialize { .. }
            | Error::MsgCodec { .. }
            | Error::MsgProtocol { .. }
            | Error::CfgParseSockAddr { .. }
            | Error::CfgResolveSockAddr { .. }
            | Error::CfgPbServerEnvNotExist { .. } => false,
        }
    }
}

#[macro_export]
macro_rules! snafu_error_handle {
    ($func_call:expr) => {
        if let Err(e) = $func_call {
            tracing::error!("{}", snafu::Report::from_error(e));
        }
    };
    ($func_call:expr, $msg:expr) => {
        if let Err(e) = $func_call {
            tracing::error!("{},detail:{}", $msg, snafu::Report::from_error(e));
        }
    };
}

#[macro_export]
macro_rules! snafu_error_get_or_continue {
    ($func_call:expr) => {
        match $func_call {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("{}", snafu::Report::from_error(e));
                continue;
            }
        }
    };
    ($func_call:expr, $msg:expr) => {
        match $func_call {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("{},detail:{}", $msg, snafu::Report::from_error(e));
                continue;
            }
        }
    };
}

#[macro_export]
macro_rules! snafu_error_get_or_return {
    ($func_call:expr) => {
        match $func_call {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("{}", snafu::Report::from_error(e));
                return;
            }
        }
    };
    ($func_call:expr, $msg:expr) => {
        match $func_call {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("{},detail:{}", $msg, snafu::Report::from_error(e));
                return;
            }
        }
    };
    ($func_call:expr, $msg:expr, $ret_val:expr) => {
        match $func_call {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("{},detail:{}", $msg, snafu::Report::from_error(e));
                return $ret_val;
            }
        }
    };
}

#[macro_export]
macro_rules! snafu_error_get_or_return_ok {
    ($func_call:expr) => {
        match $func_call {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("{}", snafu::Report::from_error(e));
                return Ok(());
            }
        }
    };
    ($func_call:expr, $msg:expr) => {
        match $func_call {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("{},detail:{}", $msg, snafu::Report::from_error(e));
                return Ok(());
            }
        }
    };
}
