use std::sync::Arc;

use snafu::Snafu;

use super::{ConnTask, ManagerTask};
use crate::common::conn_id::RemoteConnId;
use crate::common::{self};

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum Error {
    /// server task center error
    #[snafu(display("read pb conn init request with `conn_id:{conn_id}`"))]
    TaskCenterReadInitRequest {
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display("decode pb conn init request with `conn_id:{conn_id}`"))]
    TaskCenterDecodeInitRequest {
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display("send listener task error"))]
    TaskCenterSendListener {
        source: flume::SendError<ManagerTask>,
    },

    #[snafu(display(
        "client send server stream request error with `key:{key}` `client_id:{conn_id}`"
    ))]
    TaskCenterClientSendStream {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::SendError<ConnTask>,
    },
    #[snafu(display("send server stream response to task manager error, `dst_id:{conn_id}`"))]
    TaskCenterSendStreamRespToManager {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::SendError<ManagerTask>,
    },
    #[snafu(display("send server stream response to client error, `client_id:{conn_id}`"))]
    TaskCenterSendStreamRespToClient {
        conn_id: RemoteConnId,
        source: flume::SendError<ConnTask>,
    },
    #[snafu(display("The target `dst_id:{conn_id}` that stream needs to send does not exist"))]
    TaskCenterStreamTaskConnIdNotExist { conn_id: RemoteConnId },
    #[snafu(display("send server status response error with `conn_id:{conn_id}`"))]
    TaskCenterSendStatusResp {
        conn_id: RemoteConnId,
        source: flume::SendError<ConnTask>,
    },
    #[snafu(display("send server register response error with `key:{key}` `conn_id:{conn_id}`"))]
    TaskCenterSendRegisterResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::SendError<ConnTask>,
    },
    #[snafu(display("failed to process stream task, `dst_id:{conn_id}` does not exist"))]
    TaskCenterStreamConnIdNotExist { conn_id: RemoteConnId },
    #[snafu(display("subcribe server conn key not exist, `key:{key}` `client_id:{conn_id}`"))]
    TaskCenterSubcribeServerConnKeyNotExist {
        key: Arc<str>,
        conn_id: RemoteConnId,
    },
    #[snafu(display("subcribe server conn id not exist, `key:{key}` `server_id:{conn_id}`"))]
    TaskCenterSubcribeServerConnIdNotExist {
        key: Arc<str>,
        conn_id: RemoteConnId,
    },
    #[snafu(display("send client subcriber response error with `key:{key}` `conn_id:{conn_id}`"))]
    TaskCenterSendSubcribeResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::SendError<ConnTask>,
    },
    #[snafu(display("remote stream to set keepalive error,when handle_listener"))]
    TaskCenterSetKeepAlive { source: std::io::Error },
    /// server handler error
    #[snafu(display("Server connection create header tool:`{tool}` fails!"))]
    ServerConnCreateHeaderTool {
        /// Must be `reader` or `writer`
        tool: &'static str,
        source: common::error::Error,
    },
    #[snafu(display(
        "server conn receive registered response error with `key:{key}` `server_id:{conn_id}`"
    ))]
    ServerConnRecvServerRegisteredResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::RecvError,
    },
    #[snafu(display(
        "the first message received after server registration must be the \
         `RegisterResp`,detail:`key:{key}` `conn_id:{conn_id}`"
    ))]
    ServerConnRegisteredRespNotMatch {
        key: Arc<str>,
        conn_id: RemoteConnId,
    },
    #[snafu(display(
        "server conn encode register resp error with `key:{key}` `conn_id:{conn_id}`"
    ))]
    ServerConnEncodeRegisterResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display(
        "server conn write register resp error with `key:{key}` `conn_id:{conn_id}`"
    ))]
    ServerConnWriteRegisteredOk {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display("server conn receive conn task error"))]
    ServerConnRecvConnTask { source: flume::RecvError },
    #[snafu(display(
        "server conn write stream request to network error with `key:{key}` `server_id:{conn_id}`"
    ))]
    ServerConnWriteStreamRequest {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display(
        "server conn write pong response to local server error with `key:{key}` \
         `server_id:{conn_id}`"
    ))]
    ServerConnWritePongResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display(
        "server conn decode stream request error with `key:{key}` `server_id:{conn_id}`"
    ))]
    ServerConnDecodeStreamRequest {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display("send deregister server task error with `key:{key}` `conn_id:{conn_id}`"))]
    ServerConnSendDeregisterServer {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::SendError<ManagerTask>,
    },
    #[snafu(display("send register task error with `key:{key}` `conn_id:{conn_id}`"))]
    ServerConnSendRegister {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::SendError<ManagerTask>,
    },
    /// client handler error
    #[snafu(display("Client connection create header tool:`{tool}` fails!"))]
    ClientConnCreateHeaderTool {
        /// Must be `reader` or `writer`
        tool: &'static str,
        source: common::error::Error,
    },
    #[snafu(display(
        "send deregister client task error with `key:{key}` `server:{server_id:?}` <-> \
         `client:{client_id}`"
    ))]
    ClientConnSendDeregisterClient {
        key: Arc<str>,
        server_id: Option<RemoteConnId>,
        client_id: RemoteConnId,
        source: flume::SendError<ManagerTask>,
    },
    #[snafu(display("send subcribe task error with `key:{key}` `conn_id:{conn_id}`"))]
    ClientConnSendSubcribe {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::SendError<ManagerTask>,
    },
    #[snafu(display("receive subcribe response error with `key:{key}` `client_id:{conn_id}`"))]
    ClientConnRecvSubcribeResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::RecvError,
    },
    #[snafu(display("receive server stream error with `key:{key}` `client_id:{conn_id}`"))]
    ClientConnRecvStream {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: flume::RecvError,
    },
    #[snafu(display(
        "received conn task not match! we expected subcribe response.  `key:{key}` \
         `client_id:{conn_id}`"
    ))]
    ClientConnSubcribeRespNotMatch {
        key: Arc<str>,
        conn_id: RemoteConnId,
    },
    #[snafu(display(
        "received conn task not match! we expected stream response.  `key:{key}` \
         `client_id:{conn_id}`"
    ))]
    ClientConnStreamRespNotMatch {
        key: Arc<str>,
        conn_id: RemoteConnId,
    },
    #[snafu(display(
        "client conn encode subcribe resp error with `key:{key}` `conn_id:{conn_id}`"
    ))]
    ClientConnEncodeSubcribeResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display(
        "client conn encode server stream resp error with `key:{key}` `client_id:{conn_id}`"
    ))]
    ClientConnEncodeStreamResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display(
        "client conn write subcribe resp error with `key:{key}` `conn_id:{conn_id}`"
    ))]
    ClientConnWriteSubcribeResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    #[snafu(display(
        "client conn write server stream resp error with `key:{key}` `conn_id:{conn_id}`"
    ))]
    ClientConnWriteStreamResp {
        key: Arc<str>,
        conn_id: RemoteConnId,
        source: common::error::Error,
    },
    /// status handle error
    #[snafu(display("Status handler create header tool:`{tool}` fails!"))]
    StatusCreateHeaderTool {
        /// Must be `reader` or `writer`
        tool: &'static str,
        source: common::error::Error,
    },
    #[snafu(display("send status manager task error"))]
    StatusSendManagerTask {
        source: flume::SendError<ManagerTask>,
    },
    #[snafu(display("receive `ConnTask::StatusResp` error"))]
    StatusRecvConnTask { source: flume::RecvError },
    #[snafu(display("we expected `ConnTask::StatusResp`"))]
    StatusConnTaskNotMatch,
    #[snafu(display("encode status response error"))]
    StatusEncodeResp { source: common::error::Error },
    #[snafu(display("write status response error"))]
    StatusWriteResp { source: common::error::Error },
    #[snafu(display("send deregister request error with `conn_id:{conn_id}`"))]
    StatusSendDeregister {
        conn_id: RemoteConnId,
        source: flume::SendError<ManagerTask>,
    },
    #[snafu(display("Server listen error"))]
    ServerListen { source: std::io::Error },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
