//! The work the routing manager is sent, and the work it sends each connection.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use pb_mapper_auth::AuthContext;
use pb_mapper_core::conn_id::RemoteConnId;
use pb_mapper_protocol::command::{
    AdminConnectionPage, AdminServicePage, PbConnResponse, PbConnStatusReq,
};
use pb_mapper_protocol::secure::ServerHeaderSession;
use pb_mapper_protocol::transport::RelayStream;
use tokio_util::sync::CancellationToken;

use crate::guard::ConnLease;
use crate::manager::{self, ReceiverChan, SenderChan};
use crate::routing::ImutableKey;
use crate::{ServerStatusInfo, direct};

pub enum ManagerTask {
    Direct(direct::DirectTask),
    Accept {
        stream: RelayStream,
        peer_addr: SocketAddr,
    },
    Register {
        key: ImutableKey,
        conn_id: RemoteConnId,
        need_codec: bool,
        is_datagram: bool,
        protocol_version: u16,
        conn_sender: ConnTaskSender,
        /// Cancelled to tear the control connection's socket down.
        ///
        /// Separate from `conn_sender` because the two fail independently: a
        /// registration that has stopped reading leaves the writer blocked inside
        /// a single `write_msg`, and a `ConnTask` queued behind it is not observed
        /// until that write returns — which, for a half-open socket, may be never.
        /// Retirement has to reach the socket without going through the work queue.
        retire_token: CancellationToken,
        direct: Option<AuthContext>,
    },
    ServerConnActivity {
        key: ImutableKey,
        conn_id: RemoteConnId,
    },
    Subcribe {
        key: ImutableKey,
        conn_id: RemoteConnId,
        conn_sender: ConnTaskSender,
        excluded_server_conns: Vec<(RemoteConnId, u64)>,
        selected: Option<(RemoteConnId, u64)>,
    },
    Stream {
        data_protocol: Option<u16>,
        key: ImutableKey,
        stream: RelayStream,
        session: ServerHeaderSession,
        /// The provider stream socket's ID, which goes where the stream goes.
        server: ConnLease,
        client_id: RemoteConnId,
        server_generation: u64,
    },
    StreamAck {
        server_id: RemoteConnId,
        client_id: RemoteConnId,
        server_generation: u64,
    },
    Status {
        response_sender: tokio::sync::oneshot::Sender<PbConnResponse>,
        status: PbConnStatusReq,
        namespace: u64,
        conn_id: RemoteConnId,
    },
    StatusQuery {
        response_sender: tokio::sync::oneshot::Sender<ServerStatusInfo>,
    },
    AdminServiceList {
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
        response_sender: tokio::sync::oneshot::Sender<AdminServicePage>,
    },
    AdminConnectionList {
        key_id: Option<u64>,
        page: u32,
        page_size: u16,
        response_sender: tokio::sync::oneshot::Sender<AdminConnectionPage>,
    },
    AdminConnectionRetire {
        key: ImutableKey,
        /// Absent retires every connection registered under `key`.
        conn_id: Option<RemoteConnId>,
        response_sender: tokio::sync::oneshot::Sender<u32>,
    },
    DeRegisterServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
    },
    RetireServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
        reason: String,
    },
    /// A single slow stream requests confirmation, not immediate deregistration.
    SuspectServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
        generation: u64,
    },
    /// Retire only if no authenticated activity followed the suspicion.
    ConfirmServerConn {
        key: ImutableKey,
        conn_id: RemoteConnId,
        generation: u64,
        observed_at: Instant,
    },
    /// Look for registrations that stopped renewing their lease, and retire them.
    ///
    /// Sent by a ticker task rather than driven by a timer inside the manager loop:
    /// that loop's single await is a channel receive, and racing it against a tick
    /// in `tokio::select!` would drop a task the receive had already taken.
    SweepServerLeases,
    /// A connection that registered no service ended: a subscriber, a direct
    /// control connection, or one that registered nothing under its ID.
    DeRegisterClientConn {
        /// The provider stream socket that served this subscriber, once one has:
        /// the connection that sent `Stream`, not the registered control
        /// connection the subscribe was routed to.
        server_id: Option<RemoteConnId>,
        client_id: RemoteConnId,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum ConnTask {
    /// Boxed: rare and large, it must not widen every relay mailbox slot.
    Direct(Box<pb_mapper_protocol::direct::DirectEvent>),
    RegisterResp {
        generation: u64,
        protocol_version: u16,
        lease_ttl_ms: u64,
        direct: Option<pb_mapper_protocol::direct::DirectCapability>,
    },
    RegisterFailed {
        code: String,
        reason: String,
        retryable: bool,
    },
    SubcribeResp {
        ack_timeout: Duration,
        server_conn_id: RemoteConnId,
        server_generation: u64,
        need_codec: bool,
        is_datagram: bool,
    },
    SubcribeFailed {
        code: String,
        reason: String,
        retryable: bool,
    },
    SubcribeRetry {
        reason: String,
    },
    StreamReq {
        client_id: RemoteConnId,
        server_generation: u64,
    },
    Retire {
        reason: String,
    },
    StreamAck {
        server_id: RemoteConnId,
        server_generation: u64,
    },
    StreamResp {
        data_protocol: Option<u16>,
        /// The ID of the provider stream socket handed over, not of its
        /// control connection; returned if the stream is dropped on the way.
        server: ConnLease,
        server_generation: u64,
        stream: RelayStream,
        session: ServerHeaderSession,
    },
}

impl ManagerTask {
    /// Whether this task ends a connection, releasing its reusable routing ID.
    pub(crate) fn ends_connection(&self) -> bool {
        matches!(
            self,
            Self::DeRegisterServerConn { .. } | Self::DeRegisterClientConn { .. }
        )
    }
}

pub(crate) type ManagerTaskSender = manager::TaskSender;
pub(crate) type ConnTaskSender = SenderChan<ConnTask>;
pub(crate) type ConnTaskReceiver = ReceiverChan<ConnTask>;

#[cfg(test)]
mod task_tests {
    use super::*;

    /// A mailbox slot is as wide as the widest task, and every registered
    /// connection preallocates its slots. Handing over a stream is the one large
    /// task routing needs; anything wider is boxed, as `ConnTask::Direct` is.
    #[test]
    fn no_conn_task_is_wider_than_a_stream_hand_off() {
        let stream_hand_off = size_of::<RelayStream>()
            + size_of::<ServerHeaderSession>()
            + size_of::<(Option<u16>, ConnLease, u64)>();
        assert!(
            size_of::<ConnTask>() <= stream_hand_off,
            "ConnTask is {} bytes, a stream hand-off {stream_hand_off}",
            size_of::<ConnTask>()
        );
    }
}
