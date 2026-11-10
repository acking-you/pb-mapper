//! One direct coordination connection's socket loop.
use super::DirectTask;
use crate::ManagerTask;
use crate::guard::{DeregisterGuard, Deregistration};
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::direct::*;
use std::time::Duration;
use tokio::sync::mpsc;

/// How long a control connection may stay silent; clients ping well inside it.
const CONTROL_IDLE: Duration = Duration::from_secs(45);
/// How long one event may take to write before the connection is dropped.
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Events the manager may queue for one control connection.
const CONTROL_EVENT_QUEUE: usize = 64;

/// A direct control connection: its consumer state, then its ID.
///
/// Cleanup owns no sockets and is fenced by the connection's scope, so it logs
/// nothing when the manager is gone or busy.
struct ControlDeregistration {
    connection: RemoteConnId,
}

impl Deregistration for ControlDeregistration {
    fn release(&self) -> Option<ManagerTask> {
        Some(ManagerTask::Direct(DirectTask::Disconnect {
            connection: self.connection,
        }))
    }

    fn task(&self) -> ManagerTask {
        ManagerTask::DeRegisterClientConn {
            server_id: None,
            client_id: self.connection,
        }
    }

    fn manager_closed(&self) {}

    fn dropped_manager_closed(&self) {}

    fn dropped_queue_full(&self) {}

    fn deferred_manager_closed() {}

    fn no_runtime() {
        tracing::warn!(
            "cannot defer direct control deregister because no Tokio runtime is available"
        );
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
    // Causes stay typed inside the `io::Error`; their text is what it displays.
    let io = std::io::Error::other::<pb_mapper_core::error::Error>;
    let auth = session.context().map_err(io)?.clone();
    let cancellation = auth.cancellation_token().map_err(std::io::Error::other)?;
    let (events, mut rx) = mpsc::channel(CONTROL_EVENT_QUEUE);
    // Dropped, never awaited: whichever way this returns, the drop queues the
    // cleanup without waiting on the manager.
    let _guard = DeregisterGuard::new(ControlDeregistration { connection }, sender.clone());
    sender
        .send(ManagerTask::Direct(DirectTask::Connect {
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
            let msg = tokio::time::timeout(CONTROL_IDLE, reader.read_msg())
                .await
                .map_err(|_| std::io::Error::other("direct control idle"))?
                .map_err(io)?;
            let message = DirectClientMessage::decode(msg).map_err(io)?;
            sender
                .send(ManagerTask::Direct(DirectTask::Client {
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
            tokio::time::timeout(CONTROL_WRITE_TIMEOUT, writer.write_msg(&bytes))
                .await
                .map_err(|_| std::io::Error::other("direct control write timed out"))?
                .map_err(io)?;
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::select! { result = read => result, result = write => result, () = cancellation.cancelled() => Ok(()) }
}
