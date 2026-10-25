use super::{QuicConnectionDiagnostics, RelayDiagnostics, RelayTransport};
use pb_mapper_core::{checksum::Credential, config::ResolvedAddrs};
use pb_mapper_protocol::{
    command::{MessageSerializer, PbConnRequest, PbConnResponse},
    quic::*,
    secure::ClientHeaderSession,
};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        Arc, Mutex as StateMutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex;

struct Entry {
    credential: Credential,
    runtime: tokio::runtime::Id,
    generation: u64,
    shared: Arc<QuicConnection>,
}
#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    created: u64,
    opened: u64,
    reused: u64,
    error: Option<String>,
}
#[derive(Default)]
pub(crate) struct Pool {
    state: StateMutex<State>,
    setup: Mutex<()>,
    stale: AtomicBool,
}

impl Pool {
    /// Retire cached carriers after a control failure without cancelling sibling
    /// streams. Their independent owners keep established sessions alive.
    pub fn invalidate(&self) {
        self.stale.store(true, Ordering::Release);
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cached(
        &self,
        addrs: &ResolvedAddrs,
        credential: Credential,
        generation: u64,
    ) -> Option<Arc<QuicConnection>> {
        let mut state = self.state();
        if self.stale.swap(false, Ordering::AcqRel) {
            state.entries.clear();
        }
        let runtime = tokio::runtime::Handle::current().id();
        state.entries.retain(|entry| {
            entry.shared.connection.close_reason().is_none()
                && entry.runtime == runtime
                && entry.generation == generation
        });
        state
            .entries
            .iter()
            .find(|entry| {
                entry.credential == credential
                    && addrs.as_slice().contains(&entry.shared.relay_addr)
            })
            .map(|entry| entry.shared.clone())
    }

    pub async fn connect(
        &self,
        addrs: &ResolvedAddrs,
        credential: Credential,
        generation: u64,
    ) -> io::Result<QuicStream> {
        let (shared, reused) = if let Some(shared) = self.cached(addrs, credential, generation) {
            (shared, true)
        } else {
            // Serialize cold dials, but keep observations and cached opens independent
            // of network I/O. Recheck after acquiring admission to deduplicate callers.
            let _setup = self.setup.lock().await;
            if let Some(shared) = self.cached(addrs, credential, generation) {
                (shared, true)
            } else {
                let shared = match crate::addr::race_candidates(
                    addrs.as_slice(),
                    CONNECT_TIMEOUT,
                    move |addr| dial(addr, credential),
                )
                .await
                {
                    Ok(shared) => shared,
                    Err(error) => {
                        self.state().error = Some(error.to_string());
                        return Err(error);
                    }
                };
                let mut state = self.state();
                state.created += 1;
                // Evict only the cache reference; active streams retain their owner.
                if state.entries.len() >= 16 {
                    state.entries.remove(0);
                }
                state.entries.push(Entry {
                    credential,
                    runtime: tokio::runtime::Handle::current().id(),
                    generation,
                    shared: shared.clone(),
                });
                (shared, false)
            }
        };
        // Stream-credit waiting must not block other callers or observations.
        // No request is replayed here, including when opening a pooled stream fails.
        let pair = match shared.connection.open_bi().await {
            Ok(pair) => pair,
            Err(error) => {
                self.state().error = Some(error.to_string());
                return Err(io::Error::other(error));
            }
        };
        let mut state = self.state();
        state.opened += 1;
        state.reused += u64::from(reused);
        state.error = None;
        Ok(QuicStream::new(shared, pair))
    }

    pub async fn diagnostics(&self) -> RelayDiagnostics {
        let state = self.state();
        RelayDiagnostics {
            transport: RelayTransport::Quic,
            connections_created: state.created,
            streams_opened: state.opened,
            streams_reused: state.reused,
            last_error: state.error.clone(),
            connections: state
                .entries
                .iter()
                .filter(|entry| entry.shared.connection.close_reason().is_none())
                .map(|entry| {
                    let connection = &entry.shared.connection;
                    let stats = connection.stats();
                    QuicConnectionDiagnostics {
                        connection_id: connection.stable_id() as u64,
                        peer_addr: connection.remote_address(),
                        rtt_us: connection.rtt().as_micros().min(u64::MAX.into()) as u64,
                        congestion_window: stats.path.cwnd,
                        mtu: stats.path.current_mtu,
                        sent_packets: stats.path.sent_packets,
                        lost_packets: stats.path.lost_packets,
                        congestion_events: stats.path.congestion_events,
                        sent_bytes: stats.udp_tx.bytes,
                        received_bytes: stats.udp_rx.bytes,
                    }
                })
                .collect(),
        }
    }
}

async fn dial(addr: SocketAddr, credential: Credential) -> io::Result<Arc<QuicConnection>> {
    tokio::time::timeout(CONNECT_TIMEOUT, dial_inner(addr, credential))
        .await
        .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
}

async fn dial_inner(addr: SocketAddr, credential: Credential) -> io::Result<Arc<QuicConnection>> {
    // Bootstrap only identity metadata over the existing authenticated transport.
    let mut tcp = tokio::net::TcpStream::connect(addr).await?;
    tcp.set_nodelay(true)?;
    let relay_addr = tcp.peer_addr()?;
    let session = ClientHeaderSession::new_v2(&credential).map_err(io::Error::other)?;
    let response = session
        .exchange(
            &mut tcp,
            &PbConnRequest::QuicEndpoint
                .encode()
                .map_err(io::Error::other)?,
            CONNECT_TIMEOUT,
        )
        .await
        .map_err(io::Error::other)?;
    let capability = match PbConnResponse::decode(&response).map_err(io::Error::other)? {
        PbConnResponse::QuicEndpoint(Some(capability))
            if capability.port != 0
                && !capability.certificate.is_empty()
                && capability.certificate.len() <= 4096 =>
        {
            capability
        }
        PbConnResponse::Error(error) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{}: {}", error.code, error.message),
            ));
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "relay does not offer QUIC; select TCP explicitly to use this node",
            ));
        }
    };
    drop(tcp);
    let bind_ip = match relay_addr.ip() {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    let mut endpoint = quinn::Endpoint::client(SocketAddr::new(bind_ip, 0))?;
    endpoint.set_default_client_config(client_config(&capability.certificate)?);
    let connection = endpoint
        .connect(
            SocketAddr::new(relay_addr.ip(), capability.port),
            SERVER_NAME,
        )
        .map_err(io::Error::other)?
        .await
        .map_err(io::Error::other)?;
    let shared = Arc::new(QuicConnection {
        connection,
        endpoint,
        relay_addr,
    });
    let pair = shared
        .connection
        .open_bi()
        .await
        .map_err(io::Error::other)?;
    let mut stream = QuicStream::new(shared.clone(), pair);
    let session = ClientHeaderSession::new_v2(&credential).map_err(io::Error::other)?;
    let reply = session
        .exchange(&mut stream, &binding(&shared.connection)?, CONNECT_TIMEOUT)
        .await
        .map_err(io::Error::other)?;
    if reply != b"ready" {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "QUIC authentication rejected",
        ));
    }
    tracing::info!(event = "relay_quic_connected", %relay_addr, peer = %shared.connection.remote_address(), connection_id = shared.connection.stable_id());
    Ok(shared)
}
