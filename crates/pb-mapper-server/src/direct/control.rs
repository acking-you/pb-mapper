//! One direct coordination connection's socket loop.
use super::DirectTask;
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::direct::*;
use std::time::Duration;
use tokio::sync::mpsc;

struct ControlGuard {
    sender: crate::ManagerTaskSender,
    connection: RemoteConnId,
}
impl Drop for ControlGuard {
    fn drop(&mut self) {
        let sender = self.sender.clone();
        let connection = self.connection;
        // Cleanup owns no sockets and is fenced by the existing connection incarnation.
        tokio::spawn(async move {
            let _ = sender
                .send(crate::ManagerTask::Direct(DirectTask::Disconnect {
                    connection,
                }))
                .await;
            let _ = sender
                .send(crate::ManagerTask::DeRegisterClientConn {
                    server_id: None,
                    client_id: connection,
                })
                .await;
        });
    }
}

pub(crate) async fn handle_control(
    connection: RemoteConnId,
    namespace: u64,
    socket: pb_mapper_protocol::transport::RelayStream,
    session: pb_mapper_protocol::secure::ServerHeaderSession,
    sender: crate::ManagerTaskSender,
) -> std::io::Result<()> {
    use pb_mapper_protocol::{
        MessageReader, MessageWriter,
        command::{MessageSerializer, PbConnResponse},
    };
    let io = |error: pb_mapper_core::error::Error| std::io::Error::other(error.to_string());
    let auth = session.context().map_err(io)?.clone();
    let cancellation = auth
        .cancellation_token()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let (events, mut rx) = mpsc::channel(64);
    let _guard = ControlGuard {
        sender: sender.clone(),
        connection,
    };
    sender
        .send(crate::ManagerTask::Direct(DirectTask::Connect {
            connection,
            namespace,
            auth,
            sender: events,
        }))
        .await
        .map_err(|_| std::io::Error::other("router closed"))?;
    let (mut reader, mut writer) = socket.into_split();
    let mut reader = session.continuation_reader(&mut reader).map_err(io)?;
    let mut writer = session.response_writer(&mut writer).map_err(io)?;
    let read = async {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(45), reader.read_msg())
                .await
                .map_err(|_| std::io::Error::other("direct control idle"))?
                .map_err(io)?;
            let message = DirectClientMessage::decode(msg).map_err(io)?;
            sender
                .send(crate::ManagerTask::Direct(DirectTask::Client {
                    connection,
                    message,
                }))
                .await
                .map_err(|_| std::io::Error::other("router closed"))?;
        }
        #[allow(unreachable_code)]
        Ok::<(), std::io::Error>(())
    };
    let write = async {
        while let Some(event) = rx.recv().await {
            let bytes = PbConnResponse::Direct(event).encode().map_err(io)?;
            tokio::time::timeout(Duration::from_secs(5), writer.write_msg(&bytes))
                .await
                .map_err(|_| std::io::Error::other("direct control write timed out"))?
                .map_err(io)?;
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::select! { result = read => result, result = write => result, () = cancellation.cancelled() => Ok(()) }
}
