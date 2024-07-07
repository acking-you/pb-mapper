use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::info_span;

use super::error::{
    Result, StatusConnTaskNotMatchSnafu, StatusEncodeRespSnafu, StatusRecvConnTaskSnafu,
    StatusSendManagerTaskSnafu, StatusWriteRespSnafu,
};
use super::{ConnTask, ManagerTask, ManagerTaskSender};
use crate::common::conn_id::RemoteConnId;
use crate::common::message::command::{MessageSerializer, PbConnStatusReq};
use crate::common::message::{MessageWriter, NormalMessageWriter};
use crate::pb_server::error::StatusSendDeregisterSnafu;
use crate::snafu_error_handle;

struct StatusConnGuard<'a> {
    conn_id: RemoteConnId,
    sender: &'a ManagerTaskSender,
}

impl Drop for StatusConnGuard<'_> {
    fn drop(&mut self) {
        snafu_error_handle!(self
            .sender
            .send(ManagerTask::DeRegisterClientConn {
                server_id: None,
                client_id: self.conn_id
            })
            .context(StatusSendDeregisterSnafu {
                conn_id: self.conn_id
            }));
    }
}

pub async fn handle_show_status(
    status: PbConnStatusReq,
    manager_sender: ManagerTaskSender,
    conn_id: RemoteConnId,
    mut conn: TcpStream,
) -> Result<()> {
    let info_span = info_span!("show status", "{status:?},{conn_id:?}");
    let _guard = StatusConnGuard {
        conn_id,
        sender: &manager_sender,
    };
    let _enter = info_span.enter();
    let (tx, rx) = flume::bounded(5);
    let req = ManagerTask::Status {
        conn_sender: tx,
        status,
        conn_id,
    };
    manager_sender
        .send_async(req)
        .await
        .context(StatusSendManagerTaskSnafu)?;

    let resp = rx.recv_async().await.context(StatusRecvConnTaskSnafu)?;
    if let ConnTask::StatusResp(resp) = resp {
        let msg = resp.encode().context(StatusEncodeRespSnafu)?;
        let mut msg_writer = NormalMessageWriter::new(&mut conn);
        msg_writer
            .write_msg(&msg)
            .await
            .context(StatusWriteRespSnafu)
    } else {
        StatusConnTaskNotMatchSnafu {}.fail()
    }
}
