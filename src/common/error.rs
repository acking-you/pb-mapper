use std::env::VarError;
use std::net::AddrParseError;

use snafu::Snafu;

use super::checksum::ChecksumType;
use super::message::DataLenType;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum Error {
    /// Error handling for message
    #[snafu(display("read `checksum` from network error"))]
    MsgNetworkReadCheckSum { source: std::io::Error },
    #[snafu(display("read `datalen` from network error"))]
    MsgNetworkReadDatalen { source: std::io::Error },
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
        // must be structures that need to be serialized or messages that need to be deserialized
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
    #[snafu(display("`{action}` forward message failed with detail:`{detail}`"))]
    MsgForward {
        // must be "read" or "write"
        action: &'static str,
        detail: String,
    },
    /// Error for manager
    #[snafu(display("`TaskManager` fails while waiting for a task"))]
    MngWaitForTask { source: kanal::ReceiveError },
    /// Error for forward
    #[snafu(display("failed to forward message to write in normal text"))]
    FwdNetworkWriteWithNormal { source: std::io::Error },
    /// Error for stream
    #[snafu(display("failed to connect stream, type:`{stream_type}`"))]
    StmConnectStream {
        // must be "UDP" or "TCP"
        stream_type: &'static str,
        source: std::io::Error,
    },
    #[snafu(display("failed to got one addr from iter"))]
    StmGotOneAddrFromIter,
    #[snafu(display("failed to got one addr when parsing address"))]
    StmGotOneAddr { source: std::io::Error },
    /// Error for listener
    #[snafu(display("listener failed to bind addr, type:`{listener_type}`"))]
    LsnListenerBind {
        // must be "UDP" or "TCP"
        listener_type: &'static str,
        source: std::io::Error,
    },
    #[snafu(display("listener failed to accept stream, type:`{listener_type}`"))]
    LsnListenerAccept {
        // must be "UDP" or "TCP"
        listener_type: &'static str,
        source: std::io::Error,
    },
    /// Error for config
    #[snafu(display("parse socket address from string:`{string}` error"))]
    CfgParseSockAddr {
        string: String,
        source: AddrParseError,
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
            | Error::MsgNetworkReadDatalen { source }
            | Error::MsgNetworkReadBufferdRawData { source }
            | Error::MsgNetworkReadBody { source }
            | Error::MsgNetworkWriteCheckSum { source }
            | Error::MsgNetworkWriteDatalen { source }
            | Error::MsgNetworkWriteBody { source }
            | Error::MsgNetworkWriteCodecMsg { source }
            | Error::MsgNetworkWriteCodecTag { source }
            | Error::FwdNetworkWriteWithNormal { source } => is_expected(source.kind()),
            Error::MsgForward { detail, .. } => {
                let detail_lower = detail.to_ascii_lowercase();
                detail_lower.contains("unexpected end of file")
                    || detail_lower.contains("connection reset")
                    || detail_lower.contains("connection aborted")
                    || detail_lower.contains("broken pipe")
                    || detail_lower.contains("not connected")
                    || detail_lower.contains("forcibly closed")
                    || detail.contains("远程主机强迫关闭了一个现有的连接")
            }
            _ => false,
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
