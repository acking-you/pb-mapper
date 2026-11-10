use pb_mapper_protocol::transport::RelayStream;
use snafu::ResultExt;
use tracing::{Instrument, info_span};

use super::error::{
    Result, StatusCreateHeaderToolSnafu, StatusEncodeRespSnafu, StatusRecvRespSnafu,
    StatusSendManagerTaskSnafu, StatusWriteRespSnafu,
};
use super::{ManagerTask, ManagerTaskSender};
use crate::guard::ConnLease;
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{MessageSerializer, PbConnStatusReq};
use pb_mapper_protocol::secure::ServerHeaderSession;

pub async fn handle_show_status(
    status: PbConnStatusReq,
    namespace: u64,
    manager_sender: ManagerTaskSender,
    lease: ConnLease,
    mut conn: RelayStream,
    session: ServerHeaderSession,
) -> Result<()> {
    let conn_id = lease.conn_id();
    let info_span = info_span!("show status", "{status:?},{conn_id:?}");
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
    lease.release().await;
    result
}
