use snafu::ResultExt;
use tokio::net::TcpStream;
use tracing::{Instrument, info_span};

use super::error::{
    Result, StatusConnTaskNotMatchSnafu, StatusCreateHeaderToolSnafu, StatusEncodeRespSnafu,
    StatusRecvConnTaskSnafu, StatusSendManagerTaskSnafu, StatusWriteRespSnafu,
};
use super::{ConnTask, ManagerTask, ManagerTaskSender};
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{MessageSerializer, PbConnStatusReq};
use pb_mapper_protocol::secure::ServerHeaderSession;

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
    namespace: u64,
    manager_sender: ManagerTaskSender,
    conn_id: RemoteConnId,
    mut conn: TcpStream,
    session: ServerHeaderSession,
) -> Result<()> {
    let info_span = info_span!("show status", "{status:?},{conn_id:?}");
    let mut guard = StatusConnGuard::new(conn_id, manager_sender.clone());
    let result = async {
        let (tx, rx) = kanal::bounded_async(5);
        let req = ManagerTask::Status {
            conn_sender: tx,
            status,
            namespace,
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
            let mut msg_writer = session
                .response_writer(&mut conn)
                .context(StatusCreateHeaderToolSnafu { tool: "writer" })?;
            msg_writer
                .write_msg(&msg)
                .await
                .context(StatusWriteRespSnafu)
        } else {
            StatusConnTaskNotMatchSnafu {}.fail()
        }
    }
    .instrument(info_span)
    .await;
    guard.deregister().await;
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod cancellation_audit {
    use super::*;
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    #[tokio::test]
    async fn cancelled_pending_deregister_delivers_exactly_one_cleanup() {
        let (sender, mut receiver) = crate::manager::task_channel(1);
        sender.send(ManagerTask::SweepServerLeases).await.unwrap();
        let mut guard = StatusConnGuard::new(7_u32.into(), sender.clone());
        let mut deregister = Box::pin(guard.deregister());
        poll_fn(|cx| {
            assert!(deregister.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(matches!(
            receiver.recv().await.unwrap(),
            ManagerTask::SweepServerLeases
        ));
        assert!(
            receiver.is_empty(),
            "a pending send must not deliver before it is polled"
        );
        drop(deregister);
        drop(guard);
        let delivered = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        drop(delivered);
        let duplicate =
            tokio::time::timeout(std::time::Duration::from_millis(20), receiver.recv()).await;
        assert!(
            duplicate.is_err(),
            "cancelling cleanup after channel delivery emitted a second deregistration"
        );
    }
}
