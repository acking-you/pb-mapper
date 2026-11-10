use crate::endpoint::RelayEndpoint;
use pb_mapper_core::{checksum::Credential, config::control_io_timeout};
use pb_mapper_protocol::{
    MessageReader, MessageWriter,
    command::{MessageSerializer, PbConnRequest, PbConnResponse},
    direct::*,
    secure::{ClientHeaderSession, HeaderMessageReader, HeaderMessageWriter},
    transport::{RelayReadHalf, RelayStream, RelayWriteHalf},
};
use std::{
    collections::HashMap,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

/// Messages queued for the relay.
const CONTROL_QUEUE: usize = 64;
/// Grants one control connection waits for at once.
const MAX_PENDING_OPENS: usize = 64;
/// How long the relay may stay silent before the connection is dropped.
const CONTROL_IDLE: Duration = Duration::from_secs(45);
/// How often the relay is pinged; it answers each ping.
const CONTROL_PING: Duration = Duration::from_secs(15);
/// How long writing one message may take.
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const _: () = assert!(
    CONTROL_IDLE.as_secs() > CONTROL_PING.as_secs(),
    "a live relay answers a ping before the read times out"
);

type GrantReply = oneshot::Sender<io::Result<Option<SessionGrant>>>;
struct Pending {
    opens: HashMap<u64, GrantReply>,
    relays: HashMap<SessionId, oneshot::Sender<io::Result<()>>>,
}
pub(super) struct Control {
    tx: mpsc::Sender<DirectClientMessage>,
    pending: Arc<Mutex<Pending>>,
    next: AtomicU64,
    alive: Arc<AtomicBool>,
    task: tokio_util::task::AbortOnDropHandle<()>,
    pub capability: DirectCapability,
}
impl Control {
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire) && !self.task.is_finished()
    }
    pub async fn connect(
        remote: &RelayEndpoint,
        credential: Credential,
        namespace: Option<u64>,
    ) -> io::Result<Arc<Self>> {
        let mut socket = remote.connect(credential, &mut None).await?;
        let relay_ip = socket.peer_addr()?.ip();
        let session = ClientHeaderSession::new_v2(&credential).map_err(io::Error::other)?;
        session
            .write_initial(
                &mut socket,
                &PbConnRequest::DirectControl { namespace }
                    .encode()
                    .map_err(io::Error::other)?,
            )
            .await
            .map_err(io::Error::other)?;
        let (tx, mut rx) = mpsc::channel::<DirectClientMessage>(CONTROL_QUEUE);
        let pending = Arc::new(Mutex::new(Pending {
            opens: HashMap::new(),
            relays: HashMap::new(),
        }));
        let alive = Arc::new(AtomicBool::new(true));
        let (ready, ready_rx) = oneshot::channel();
        let pending_task = pending.clone();
        let alive_task = alive.clone();
        let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            let cleanup = ControlCleanup {
                pending: pending_task.clone(),
                alive: alive_task,
            };
            let result = drive(socket, &session, ready, &pending_task, &mut rx).await;
            if let Err(error) = result {
                tracing::debug!(%error, "direct coordination disconnected; established data sessions stay alive");
            }
            drop(cleanup);
        }));
        let mut capability = tokio::time::timeout(control_io_timeout(), ready_rx)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "direct control not ready"))?
            .map_err(|_| io::Error::other("direct control closed before ready"))?;
        capability.stun_addr = stun_target(capability.stun_addr, relay_ip);
        Ok(Arc::new(Self {
            tx,
            pending,
            next: AtomicU64::new(1),
            alive,
            task,
            capability,
        }))
    }
    pub async fn open(
        &self,
        key: String,
        is_datagram: bool,
        peer: PeerInfo,
    ) -> io::Result<Option<SessionGrant>> {
        let request = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.opens.retain(|_, reply| !reply.is_closed());
            if pending.opens.len() >= MAX_PENDING_OPENS {
                return Err(io::Error::other("direct setup capacity reached"));
            }
            pending.opens.insert(request, tx);
        }
        let _guard = PendingGuard {
            pending: self.pending.clone(),
            tx: self.tx.clone(),
            request: Some(request),
            id: None,
        };
        self.tx
            .send(DirectClientMessage::Open {
                request,
                key,
                is_datagram,
                peer,
            })
            .await
            .map_err(|_| io::Error::other("direct control closed"))?;
        rx.await
            .map_err(|_| io::Error::other("direct grant lost"))?
    }
    pub async fn relay(&self, id: SessionId) -> io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .relays
            .insert(id, tx);
        let _guard = PendingGuard {
            pending: self.pending.clone(),
            tx: self.tx.clone(),
            request: None,
            id: Some(id),
        };
        self.tx
            .send(DirectClientMessage::RelaySelect { id })
            .await
            .map_err(|_| io::Error::other("direct control closed"))?;
        rx.await
            .map_err(|_| io::Error::other("relay selection lost"))?
    }
    pub async fn report(&self, report: SessionUpdate) -> io::Result<()> {
        self.tx
            .send(DirectClientMessage::Report(report))
            .await
            .map_err(|_| io::Error::other("direct control closed"))
    }
    pub fn cancel(&self, id: SessionId) {
        let _ = self.tx.try_send(DirectClientMessage::Cancel { id });
    }
}
/// Run a control connection: wait for the relay's `Ready`, hand its
/// capability to `ready`, then deliver what the relay sends to `pending` and
/// write what `rx` queues, until either side ends.
async fn drive(
    socket: RelayStream,
    session: &ClientHeaderSession,
    ready: oneshot::Sender<DirectCapability>,
    pending: &Mutex<Pending>,
    rx: &mut mpsc::Receiver<DirectClientMessage>,
) -> io::Result<()> {
    let (mut read, mut write) = socket.into_split();
    let mut reader = session
        .response_reader(&mut read)
        .map_err(io::Error::other)?;
    let mut writer = session
        .continuation_writer(&mut write)
        .map_err(io::Error::other)?;
    let first = reader.read_msg().await.map_err(io::Error::other)?;
    let PbConnResponse::Direct(DirectEvent::Ready(capability)) =
        PbConnResponse::decode(first).map_err(io::Error::other)?
    else {
        return Err(io::Error::other("relay declined direct control"));
    };
    let _ = ready.send(capability);
    tokio::select! {
        result = read_events(&mut reader, pending) => result,
        result = write_messages(&mut writer, rx) => result,
    }
}

/// Deliver each event the relay sends, until it fails or goes quiet.
async fn read_events(
    reader: &mut HeaderMessageReader<'_, RelayReadHalf>,
    pending: &Mutex<Pending>,
) -> io::Result<()> {
    loop {
        let bytes = tokio::time::timeout(CONTROL_IDLE, reader.read_msg())
            .await
            .map_err(|_| io::Error::other("direct control idle"))?
            .map_err(io::Error::other)?;
        let PbConnResponse::Direct(event) =
            PbConnResponse::decode(bytes).map_err(io::Error::other)?
        else {
            return Err(io::Error::other("unexpected direct control response"));
        };
        deliver(
            &mut pending.lock().unwrap_or_else(|e| e.into_inner()),
            event,
        )?;
    }
}

/// Write each queued message, and a ping when none was queued for a while.
/// Ends when every sender is gone.
async fn write_messages(
    writer: &mut HeaderMessageWriter<'_, RelayWriteHalf>,
    rx: &mut mpsc::Receiver<DirectClientMessage>,
) -> io::Result<()> {
    let mut ping = tokio::time::interval(CONTROL_PING);
    loop {
        let message = tokio::select! {
            message = rx.recv() => {
                let Some(message) = message else { return Ok(()) };
                message
            }
            _ = ping.tick() => DirectClientMessage::Ping,
        };
        let bytes = message.encode().map_err(io::Error::other)?;
        tokio::time::timeout(CONTROL_WRITE_TIMEOUT, writer.write_msg(&bytes))
            .await
            .map_err(|_| io::Error::other("direct control write timeout"))?
            .map_err(io::Error::other)?;
    }
}

/// Answer the setup waiting on `event`. A consumer is sent only answers to
/// what it asked, and pongs; anything else is a broken relay.
fn deliver(pending: &mut Pending, event: DirectEvent) -> io::Result<()> {
    match event {
        DirectEvent::Granted { request, grant } => {
            if let Some(reply) = pending.opens.remove(&request) {
                let _ = reply.send(Ok(Some(grant)));
            }
        }
        DirectEvent::LegacyRelay { request } => {
            if let Some(reply) = pending.opens.remove(&request) {
                let _ = reply.send(Ok(None));
            }
        }
        DirectEvent::Rejected { request, reason } => {
            if let Some(reply) = pending.opens.remove(&request) {
                let _ = reply.send(Err(io::Error::other(reason)));
            }
        }
        DirectEvent::RelaySelected { id } => {
            if let Some(reply) = pending.relays.remove(&id) {
                let _ = reply.send(Ok(()));
            }
        }
        DirectEvent::Pong => {}
        DirectEvent::Ready(_)
        | DirectEvent::Offer { .. }
        | DirectEvent::RelaySelect { .. }
        | DirectEvent::Cancel { .. } => {
            return Err(io::Error::other("invalid consumer coordination event"));
        }
    }
    Ok(())
}

struct PendingGuard {
    tx: mpsc::Sender<DirectClientMessage>,
    pending: Arc<Mutex<Pending>>,
    request: Option<u64>,
    id: Option<SessionId>,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = self.request
            && p.opens.remove(&r).is_some()
        {
            let _ = self
                .tx
                .try_send(DirectClientMessage::CancelOpen { request: r });
        }
        if let Some(id) = self.id {
            p.relays.remove(&id);
        }
    }
}
struct ControlCleanup {
    pending: Arc<Mutex<Pending>>,
    alive: Arc<AtomicBool>,
}
impl Drop for ControlCleanup {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.opens.clear();
        p.relays.clear();
    }
}
