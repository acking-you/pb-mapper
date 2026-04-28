use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::info_span;

use super::error::{
    Result, StatusConnTaskNotMatchSnafu, StatusCreateHeaderToolSnafu, StatusEncodeRespSnafu,
    StatusRecvConnTaskSnafu, StatusSendManagerTaskSnafu, StatusWriteRespSnafu,
};
use super::{ConnTask, ManagerTask, ManagerTaskSender};
use crate::common::conn_id::RemoteConnId;
use crate::common::message::command::{MessageSerializer, PbConnStatusReq};
use crate::common::message::{get_header_msg_writer, MessageWriter};

struct StatusConnGuard {
    conn_id: RemoteConnId,
    sender: ManagerTaskSender,
    active: bool,
}

impl StatusConnGuard {
    fn new(conn_id: RemoteConnId, sender: ManagerTaskSender) -> Self {
        Self {
            conn_id,
            sender,
            active: true,
        }
    }

    fn deregister_task(&self) -> ManagerTask {
        ManagerTask::DeRegisterClientConn {
            server_id: None,
            client_id: self.conn_id,
        }
    }

    async fn deregister(&mut self) {
        if !self.active {
            return;
        }
        let task = self.deregister_task();
        if self.sender.send(task).await.is_err() {
            tracing::debug!("skip async status deregister because manager channel is closed");
        }
        self.active = false;
    }

    fn spawn_deregister(sender: ManagerTaskSender, task: ManagerTask) {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if sender.send(task).await.is_err() {
                        tracing::debug!(
                            "skip deferred status deregister because manager channel is closed"
                        );
                    }
                });
            }
            Err(_) => {
                tracing::warn!(
                    "cannot defer status deregister because no Tokio runtime is available"
                );
            }
        }
    }
}

impl Drop for StatusConnGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let task = self.deregister_task();
        match self.sender.try_send(task) {
            Ok(()) => {}
            Err(kanal::SendTimeoutError::Closed(_)) => {
                tracing::debug!("skip status deregister because manager channel is closed");
            }
            Err(kanal::SendTimeoutError::Timeout(task)) => {
                tracing::warn!("manager queue is full; defer status deregister");
                Self::spawn_deregister(self.sender.clone(), task);
            }
        }
    }
}

pub async fn handle_show_status(
    status: PbConnStatusReq,
    manager_sender: ManagerTaskSender,
    conn_id: RemoteConnId,
    mut conn: TcpStream,
) -> Result<()> {
    let info_span = info_span!("show status", "{status:?},{conn_id:?}");
    let mut guard = StatusConnGuard::new(conn_id, manager_sender.clone());
    let _enter = info_span.enter();
    let result = async {
        let (tx, rx) = kanal::bounded_async(5);
        let req = ManagerTask::Status {
            conn_sender: tx,
            status,
            conn_id,
        };
        manager_sender
            .send(req)
            .await
            .map_err(|_| kanal::SendError(()))
            .context(StatusSendManagerTaskSnafu)?;

        let resp = rx.recv().await.context(StatusRecvConnTaskSnafu)?;
        if let ConnTask::StatusResp(resp) = resp {
            let msg = resp.encode().context(StatusEncodeRespSnafu)?;
            let mut msg_writer = get_header_msg_writer(&mut conn)
                .context(StatusCreateHeaderToolSnafu { tool: "writer" })?;
            msg_writer
                .write_msg(&msg)
                .await
                .context(StatusWriteRespSnafu)
        } else {
            StatusConnTaskNotMatchSnafu {}.fail()
        }
    }
    .await;
    guard.deregister().await;
    result
}
