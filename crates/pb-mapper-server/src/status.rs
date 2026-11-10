use pb_mapper_protocol::transport::RelayStream;
use snafu::ResultExt;
use tracing::{Instrument, info_span};

use super::error::{
    Result, StatusCreateHeaderToolSnafu, StatusEncodeRespSnafu, StatusRecvRespSnafu,
    StatusSendManagerTaskSnafu, StatusWriteRespSnafu,
};
use super::{ManagerTask, ManagerTaskSender};
use crate::guard::{DeregisterGuard, Deregistration};
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{MessageSerializer, PbConnStatusReq};
use pb_mapper_protocol::secure::ServerHeaderSession;

/// A status connection's ID, returned to the manager when the request ends.
struct StatusDeregistration {
    conn_id: RemoteConnId,
}

impl Deregistration for StatusDeregistration {
    fn task(&self) -> ManagerTask {
        ManagerTask::DeRegisterClientConn {
            server_id: None,
            client_id: self.conn_id,
        }
    }

    fn manager_closed(&self) {
        tracing::debug!("skip async status deregister because manager channel is closed");
    }

    fn dropped_manager_closed(&self) {
        tracing::debug!("skip status deregister because manager channel is closed");
    }

    fn dropped_queue_full(&self) {
        tracing::warn!("manager queue is full; defer status deregister");
    }

    fn deferred_manager_closed() {
        tracing::debug!("skip deferred status deregister because manager channel is closed");
    }

    fn no_runtime() {
        tracing::warn!("cannot defer status deregister because no Tokio runtime is available");
    }
}

pub async fn handle_show_status(
    status: PbConnStatusReq,
    namespace: u64,
    manager_sender: ManagerTaskSender,
    conn_id: RemoteConnId,
    mut conn: RelayStream,
    session: ServerHeaderSession,
) -> Result<()> {
    let info_span = info_span!("show status", "{status:?},{conn_id:?}");
    let mut guard = DeregisterGuard::new(StatusDeregistration { conn_id }, manager_sender.clone());
    let result = async {
        let (response_sender, response) = tokio::sync::oneshot::channel();
        let req = ManagerTask::Status {
            response_sender,
            status,
            namespace,
            conn_id,
        };
        manager_sender
            .send(req)
            .await
            .context(StatusSendManagerTaskSnafu)?;

        let resp = response.await.context(StatusRecvRespSnafu)?;
        let msg = resp.encode().context(StatusEncodeRespSnafu)?;
        let mut msg_writer = session
            .response_writer(&mut conn)
            .context(StatusCreateHeaderToolSnafu { tool: "writer" })?;
        msg_writer
            .write_msg(&msg)
            .await
            .context(StatusWriteRespSnafu)
    }
    .instrument(info_span)
    .await;
    guard.deregister().await;
    result
}
