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
    /// Error for manager
    #[snafu(display("`TaskManager` fails while waiting for a task"))]
    MngWaitForTask { source: flume::RecvError },
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
