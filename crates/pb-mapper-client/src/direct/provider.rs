use super::{
    QuicGuard,
    control::io_error,
    endpoint::{self, Network},
};
use crate::{diagnostics::Diagnostics, server::LocalControlWrite};
use pb_mapper_core::config::ResolvedAddrs;
use pb_mapper_protocol::{direct::*, forward::StreamForward};
use std::{
    collections::HashMap,
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncReadExt, sync::mpsc, task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;
use uni_stream::stream::StreamProvider;

type Reply = mpsc::Sender<LocalControlWrite>;
struct Envelope {
    event: Option<DirectEvent>,
    reply: Reply,
}
pub(crate) struct Provider {
    tx: mpsc::Sender<Envelope>,
    active: Arc<AtomicUsize>,
    shutdown: CancellationToken,
    done: CancellationToken,
    task: tokio_util::task::AbortOnDropHandle<()>,
}
struct Incoming {
    connection: QuicGuard,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    tag: [u8; 32],
}
enum SessionEvent {
    Granted,
    Incoming(Incoming),
    Relay,
    Cancel,
}
impl Provider {
    pub fn spawn<L: StreamProvider + Send + 'static>(
        local: ResolvedAddrs,
        key: Arc<str>,
        is_datagram: bool,
        shutdown: CancellationToken,
        idle: Option<Duration>,
        diagnostics: Diagnostics,
    ) -> Arc<Self>
    where
        L::Item: StreamForward,
    {
        let (tx, rx) = mpsc::channel(128);
        let active = Arc::new(AtomicUsize::new(0));
        let done = CancellationToken::new();
        let done_guard = done.clone().drop_guard();
        let actor_shutdown = shutdown.clone();
        let actor_active = active.clone();
        let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            let _done_guard = done_guard;
            run::<L>(
                rx,
                local,
                key,
                is_datagram,
                actor_shutdown,
                idle,
                diagnostics,
                actor_active,
            )
            .await;
        }));
        Arc::new(Self {
            tx,
            active,
            shutdown,
            done,
            task,
        })
    }
    pub async fn stop(&self) {
        self.shutdown.cancel();
        self.done.cancelled().await;
    }

    pub fn event(&self, event: DirectEvent, reply: Reply) {
        let _ = self.tx.try_send(Envelope {
            event: Some(event),
            reply,
        });
    }
    pub fn connected(&self, reply: Reply) {
        let _ = self.tx.try_send(Envelope { event: None, reply });
    }
    pub fn has_active(&self) -> bool {
        self.active.load(Ordering::Acquire) != 0 && !self.task.is_finished()
    }
}
#[allow(clippy::too_many_arguments)]
async fn run<L: StreamProvider + Send + 'static>(
    mut rx: mpsc::Receiver<Envelope>,
    local: ResolvedAddrs,
    key: Arc<str>,
    is_datagram: bool,
    shutdown: CancellationToken,
    idle: Option<Duration>,
    diagnostics: Diagnostics,
    active: Arc<AtomicUsize>,
) where
    L::Item: StreamForward,
{
    let mut network: Option<Arc<Network>> = None;
    let reports = Arc::new(ProviderReports::default());
    let mut sessions = HashMap::<SessionId, mpsc::Sender<SessionEvent>>::new();
    let mut tasks = JoinSet::new();
    let mut arrivals = JoinSet::new();
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            envelope = rx.recv() => {
                let Some(Envelope { event, reply }) = envelope else { break; };
                reports.connected(reply.clone());
                let Some(event) = event else { continue; };
                match event {
                    DirectEvent::Offer { grant, stun_addr } => {
                        if sessions.contains_key(&grant.id) { continue; }
                        if sessions.len() >= 64 || !grant.consumer.valid() || grant.service != key.as_ref() || grant.is_datagram != is_datagram || grant.ttl_ms == 0 || grant.ttl_ms > SETUP_TTL_MS {
                            send(&reply, DirectProviderMessage::Reject { id:grant.id, reason:"invalid offer or provider capacity reached".into() }); continue;
                        }
                        let net = match &network { Some(net) => net.clone(), None => match Network::new(true) { Ok(net) => { network=Some(net.clone()); net }, Err(error) => { send(&reply,DirectProviderMessage::Reject { id:grant.id, reason:error.to_string() });continue; } } };
                        let (tx, rx) = mpsc::channel(8); let id=grant.id; sessions.insert(id,tx);
                        let local=local.clone();let active=active.clone();let diagnostics=diagnostics.clone();let reports=reports.clone();
                        tasks.spawn(async move {
                            let result = session::<L>(net, grant, stun_addr, reply, rx, local, idle, diagnostics, active, reports).await;
                            if let Err(error)=result { tracing::debug!(%error, "provider direct session ended"); }
                            id
                        });
                    }
                    DirectEvent::Granted { grant, .. } => { if let Some(tx)=sessions.get(&grant.id) { let _=tx.try_send(SessionEvent::Granted); } }
                    DirectEvent::RelaySelect { id } => { if let Some(tx)=sessions.get(&id) { let _=tx.try_send(SessionEvent::Relay); } }
                    DirectEvent::Cancel { id } => { if let Some(tx)=sessions.get(&id) { let _=tx.try_send(SessionEvent::Cancel); } }
                    _ => {}
                }
            }
            connection = async { match &network { Some(net) => net.accept().await, None => std::future::pending().await } } => {
                if let Some(connection)=connection {
                    tracing::debug!(peer=%connection.remote_address(), "direct TLS accepted");
                    if arrivals.len() >= 64 { connection.close(0u32.into(),b"session capacity");continue; }
                    arrivals.spawn(async move {
                        let connection=QuicGuard(connection);
                        tokio::time::timeout(Duration::from_secs(2),async {
                            let (send,mut recv)=connection.0.accept_bi().await.map_err(io_error)?;
                            let mut id=[0;16];recv.read_exact(&mut id).await.map_err(io_error)?;
                            let mut tag=[0;32];recv.read_exact(&mut tag).await.map_err(io_error)?;
                            Ok::<_,io::Error>((id,Incoming { connection,send,recv,tag }))
                        }).await
                    });
                }
            }
            Some(result)=arrivals.join_next(), if !arrivals.is_empty() => {
                if let Ok(Ok(Ok((id,incoming))))=result && let Some(tx)=sessions.get(&id) { let _=tx.try_send(SessionEvent::Incoming(incoming)); }
            }
            Some(result)=tasks.join_next(), if !tasks.is_empty() => { if let Ok(id)=result { sessions.remove(&id); } }
        }
    }
    tasks.shutdown().await;
    arrivals.shutdown().await;
}
fn send(reply: &Reply, message: DirectProviderMessage) {
    let _ = reply.try_send(LocalControlWrite::Direct(message));
}

#[allow(clippy::too_many_arguments)]
async fn session<L: StreamProvider>(
    network: Arc<Network>,
    mut grant: SessionGrant,
    stun_addr: std::net::SocketAddr,
    reply: Reply,
    mut events: mpsc::Receiver<SessionEvent>,
    local: ResolvedAddrs,
    idle: Option<Duration>,
    diagnostics: Diagnostics,
    active: Arc<AtomicUsize>,
    reports: Arc<ProviderReports>,
) -> io::Result<()>
where
    L::Item: StreamForward,
{
    let deadline = Instant::now() + Duration::from_millis(grant.ttl_ms);
    let _probe_guard = ProbeGuard {
        network: network.clone(),
        id: grant.id,
    };
    grant.provider = network.peer(stun_addr).await;
    network.authorize(&grant, true, deadline)?;
    send(
        &reply,
        DirectProviderMessage::Accept {
            id: grant.id,
            peer: grant.provider.clone(),
        },
    );
    let mut incoming: Option<Incoming> = None;
    let mut granted = false;
    let mut probes = tokio::time::interval(Duration::from_millis(120));
    loop {
        if granted && incoming.is_some() {
            break;
        }
        tokio::select! {
            ()=tokio::time::sleep_until(deadline)=>return Err(io::Error::new(io::ErrorKind::TimedOut,"provider setup expired")),
            _=probes.tick()=>{network.probe(&grant,true).await;},
            event=events.recv()=>match event {
                Some(SessionEvent::Granted)=>{tracing::debug!(id=?grant.id,"direct grant accepted by provider");granted=true;},
                Some(SessionEvent::Incoming(candidate)) if incoming.is_none()=> {
                    tracing::debug!(id=?grant.id,"direct session bind received");
                    match verify(&candidate,&grant) { Ok(()) => { incoming=Some(candidate); }, Err(error) => tracing::debug!(%error, "direct session binding rejected") }
                }
                Some(SessionEvent::Relay)=>{send(&reply,DirectProviderMessage::RelaySelected{id:grant.id});return Ok(());}
                Some(SessionEvent::Cancel)|None=>return Ok(()),
                _=>{}
            }
        }
    }
    let Some(mut incoming) = incoming else {
        return Err(io::Error::other("missing prepared connection"));
    };
    incoming.send.write_all(&[1]).await.map_err(io_error)?;
    // Keep the read future alive while processing probe ticks or coordination.
    {
        let start = async { incoming.recv.read_u8().await };
        tokio::pin!(start);
        loop {
            tokio::select! {
                ()=tokio::time::sleep_until(deadline)=>return Err(io::Error::new(io::ErrorKind::TimedOut,"direct commit expired")),
                result=&mut start=>{if result?!=2{return Err(io::Error::other("invalid direct commit"));}break;}
                event=events.recv()=>match event {
                    Some(SessionEvent::Relay)=>{send(&reply,DirectProviderMessage::RelaySelected{id:grant.id});return Ok(());}
                    Some(SessionEvent::Cancel)|None=>return Ok(()),
                    _=>{}
                }
            }
        }
    }
    // A committed session belongs to the endpoints even while its backend dial
    // is pending. A simultaneous credential rejection must not tear it down.
    let _active = ActiveSession::new(active);
    // This is the sole backend dial for direct. No backend is opened while probing.
    let local = match tokio::time::timeout_at(deadline, L::from_addr(local.as_slice())).await {
        Ok(Ok(local)) => local,
        _ => {
            let _ = incoming.send.write_all(&[4]).await;
            return Err(io::Error::other("local service connect failed"));
        }
    };
    incoming.send.write_all(&[3]).await.map_err(io_error)?;
    let framing = endpoint::framing_key(&incoming.connection.0, &grant)?;
    let mut report = Report {
        reports,
        session: (&grant).into(),
        secret: grant.secret,
        seq: 0,
        diagnostics,
    };
    report.diagnostics.path_started(SessionPath::Direct);
    report.tick();
    tracing::info!(event="session_path_selected",path="direct",service=%grant.service,peer=%incoming.connection.0.remote_address());
    let forwarding = super::forward_quic(local, incoming.send, incoming.recv, framing, idle);
    tokio::pin!(forwarding);
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    tick.tick().await;
    loop {
        tokio::select! {result=&mut forwarding=>return result.map_err(io_error),_=tick.tick()=>report.tick()}
    }
}
fn verify(incoming: &Incoming, grant: &SessionGrant) -> io::Result<()> {
    let identity = incoming
        .connection
        .0
        .peer_identity()
        .ok_or_else(|| io::Error::other("missing client identity"))?;
    let certs = identity
        .downcast::<Vec<quinn::rustls::pki_types::CertificateDer<'static>>>()
        .map_err(|_| io::Error::other("unexpected client identity"))?;
    if certs.len() != 1 || certs[0].as_ref() != grant.consumer.certificate {
        return Err(io::Error::other(
            "client identity does not match this session",
        ));
    }
    endpoint::verify_binding(&incoming.connection.0, grant, &incoming.tag)
}
struct ProbeGuard {
    network: Arc<Network>,
    id: SessionId,
}
impl Drop for ProbeGuard {
    fn drop(&mut self) {
        self.network.forget(&self.id);
    }
}
#[derive(Default)]
struct ProviderReports {
    latest: super::reports::Latest,
    routes: std::sync::Mutex<Vec<Reply>>,
}
impl ProviderReports {
    fn connected(&self, reply: Reply) {
        let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
        routes.retain(|route| !route.is_closed());
        if !routes.iter().any(|route| route.same_channel(&reply)) {
            if routes.len() < 64 {
                routes.push(reply.clone());
            }
            for update in self.latest.snapshot() {
                send(&reply, DirectProviderMessage::Report(update));
            }
        }
    }
    fn put(&self, update: SessionUpdate) {
        self.latest.put(update.clone());
        let routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
        for route in routes.iter() {
            if route
                .try_send(LocalControlWrite::Direct(DirectProviderMessage::Report(
                    update.clone(),
                )))
                .is_ok()
            {
                break;
            }
        }
    }
}
struct ActiveSession(Arc<AtomicUsize>);
impl ActiveSession {
    fn new(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::AcqRel);
        Self(active)
    }
}
impl Drop for ActiveSession {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Report {
    reports: Arc<ProviderReports>,
    session: SessionSummary,
    secret: SessionSecret,
    seq: u64,
    diagnostics: Diagnostics,
}
impl Report {
    fn update(&mut self, state: SessionState) {
        self.seq += 1;
        self.reports.put(SessionUpdate {
            session: self.session.clone(),
            secret: self.secret,
            report: SessionReport {
                id: self.session.id,
                seq: self.seq,
                path: SessionPath::Direct,
                state,
                reason: None,
            },
        });
    }
    fn tick(&mut self) {
        self.update(SessionState::Active);
    }
}
impl Drop for Report {
    fn drop(&mut self) {
        self.diagnostics.path_closed(SessionPath::Direct);
        self.update(SessionState::Closed);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use tokio::net::TcpListener;
    use tokio_util::task::AbortOnDropHandle;
    use uni_stream::stream::TcpStreamProvider;

    #[tokio::test]
    async fn committed_direct_cannot_acknowledge_relay_or_dial_backend_twice() {
        let provider = Network::new(true).unwrap();
        let consumer = Network::new(false).unwrap();
        let stun = "127.0.0.1:9".parse().unwrap();
        let mut grant = SessionGrant {
            id: random_session_id(),
            server_instance: random_session_id(),
            generation: 1,
            namespace: 0,
            service: "commit-boundary".into(),
            is_datagram: false,
            consumer_key: 0,
            provider_key: 0,
            consumer: consumer.peer(stun).await,
            provider: PeerInfo {
                certificate: vec![],
                candidates: vec![],
            },
            secret: SessionSecret::random(),
            ttl_ms: SETUP_TTL_MS,
        };
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (replies, mut reply_rx) = mpsc::channel(16);
        let (events, event_rx) = mpsc::channel(8);
        let active = Arc::new(AtomicUsize::new(0));
        let task = AbortOnDropHandle::new(tokio::spawn(session::<TcpStreamProvider>(
            provider.clone(),
            grant.clone(),
            stun,
            replies,
            event_rx,
            ResolvedAddrs::from(backend.local_addr().unwrap()),
            None,
            Diagnostics::default(),
            active.clone(),
            Arc::new(ProviderReports::default()),
        )));
        let LocalControlWrite::Direct(DirectProviderMessage::Accept { peer, .. }) =
            reply_rx.recv().await.unwrap()
        else {
            panic!("expected accepted offer");
        };
        grant.provider = peer;
        let deadline = Instant::now() + Duration::from_secs(3);
        consumer.authorize(&grant, false, deadline).unwrap();
        events.send(SessionEvent::Granted).await.unwrap();
        let (connection, incoming) = tokio::join!(
            consumer.connect(&grant, deadline),
            tokio::time::timeout_at(deadline, provider.accept())
        );
        let connection = connection.unwrap();
        let incoming = incoming.unwrap().unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(b"bind").await.unwrap();
        let (provider_send, mut provider_recv) = incoming.accept_bi().await.unwrap();
        let mut bind = [0; 4];
        provider_recv.read_exact(&mut bind).await.unwrap();
        events
            .send(SessionEvent::Incoming(Incoming {
                connection: QuicGuard(incoming),
                send: provider_send,
                recv: provider_recv,
                tag: endpoint::binding(&connection, &grant).unwrap(),
            }))
            .await
            .unwrap();
        assert_eq!(recv.read_u8().await.unwrap(), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), backend.accept())
                .await
                .is_err(),
            "preparation must not connect the backend"
        );
        send.write_all(&[2]).await.unwrap();
        let (_backend, _) = tokio::time::timeout(Duration::from_secs(2), backend.accept())
            .await
            .unwrap()
            .unwrap();
        // The consumer loses Started. A late relay request must not authorize
        // replay, whether the provider is forwarding or notices the reset first.
        let _ = recv.stop(0u32.into());
        let _ = events.send(SessionEvent::Relay).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), backend.accept())
                .await
                .is_err()
        );
        connection.close(0u32.into(), b"test complete");
        let _ = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap();
        while let Ok(message) = reply_rx.try_recv() {
            assert!(!matches!(
                message,
                LocalControlWrite::Direct(DirectProviderMessage::RelaySelected { .. })
            ));
        }
        assert_eq!(active.load(Ordering::Acquire), 0);
    }
}
