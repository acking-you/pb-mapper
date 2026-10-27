//! Pooled QUIC carriers. A connection is dialled after authenticated TCP
//! discovery, pinned to the certificate discovery named, bound to the
//! credential by the TLS exporter, and then shared: each relay stream is one
//! bidirectional QUIC stream on it.
//!
//! The pool holds every connection it dialled until it closes it, retired or
//! not. Quinn closes a connection the moment its last handle drops, and that
//! discards what its streams had written but not yet delivered: a response
//! tail, say, on a connection no longer handed out whose last stream just
//! finished. So no stream's drop is ever what closes a connection here; the
//! sweep is, once nothing has been open on it for [`IDLE_CLOSE`].
use super::{
    ConnectError, QUIC_UNAVAILABLE, QuicConnectionDiagnostics, RelayDiagnostics, RelayTransport,
};
use pb_mapper_core::{checksum::Credential, config::ResolvedAddrs};
use pb_mapper_protocol::{command::PbErrorResponse, quic::*, secure::ClientHeaderSession};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    num::NonZeroU64,
    sync::{Arc, Mutex as StateMutex, MutexGuard, Weak},
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};

/// Connections handed out per pool; past this the oldest is retired.
const MAX_ENTRIES: usize = 16;

/// A connection with no stream open for this long is closed. The relay
/// holds an admission slot for each, and the client's keep-alive would
/// otherwise keep an unused one open for as long as the process lives. A
/// retired one waits as long, so that its last streams finish delivering.
const IDLE_CLOSE: Duration = Duration::from_secs(30);
const SWEEP_INTERVAL: Duration = Duration::from_secs(10);

/// References past which a connection is not handed out again, and another
/// is dialled rather than waiting for stream credit. Each stream holds one,
/// two once split, so this keeps under the relay's per-connection limit.
const REFERENCES_PER_CONNECTION: usize = MAX_STREAMS as usize;

/// How long the relay may be silent on a connection before a transport
/// failure retires it: well past a round trip and an acknowledgement's
/// delay on a slow path, and short of every setup budget (3 s or more), so
/// a path that stopped delivering is caught by the failure it caused.
const SILENCE: Duration = Duration::from_secs(2);

/// Which of a pool's connections a relay stream went over: its place in
/// the order the pool dialled them. Never reused, so a failure reported
/// late, once its connection is gone, cannot name the one that replaced
/// it; and a plain number, so recording it costs a stream nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CarrierId(NonZeroU64);

struct Entry {
    id: CarrierId,
    credential: Credential,
    runtime: tokio::runtime::Id,
    generation: u64,
    shared: Arc<QuicConnection>,
    /// Whether new streams still go over it. A retired connection stays
    /// open for the streams already on it.
    pooled: bool,
    /// When a stream was last handed out on it, or last seen open.
    last_open: Instant,
    /// The datagrams it had received when last looked at, and when a look
    /// last found more: the last time the relay was heard on it, as near as
    /// the pool has looked.
    received: u64,
    heard: Instant,
}

impl Entry {
    fn new(
        id: CarrierId,
        credential: Credential,
        generation: u64,
        shared: Arc<QuicConnection>,
    ) -> Self {
        Self {
            id,
            credential,
            runtime: tokio::runtime::Handle::current().id(),
            generation,
            received: received(&shared),
            shared,
            pooled: true,
            last_open: Instant::now(),
            heard: Instant::now(),
        }
    }

    fn is_open(&self) -> bool {
        self.shared.connection.close_reason().is_none()
    }

    /// When the relay was last heard on it, looking now: anything it sent
    /// counts, an acknowledgement at least, if the path still works.
    fn heard(&mut self, now: Instant) -> Instant {
        let received = received(&self.shared);
        if received > self.received {
            self.received = received;
            self.heard = now;
        }
        self.heard
    }
}

fn received(shared: &QuicConnection) -> u64 {
    shared.connection.stats().udp_rx.datagrams
}

#[derive(Default)]
struct State {
    /// Every connection not yet closed: those handed out first, in the order
    /// they were dialled, and those retired.
    entries: Vec<Entry>,
    created: u64,
    opened: u64,
    reused: u64,
    error: Option<String>,
    /// The last cold dial that failed, for the callers queued behind it.
    failed: Option<FailedDial>,
    sweeping: bool,
}

/// A dial that failed, kept so that the callers that queued behind it fail
/// with it, rather than each spending the setup budget on the same relay.
struct FailedDial {
    at: Instant,
    credential: Credential,
    generation: u64,
    rejected: Option<PbErrorResponse>,
    transport: (io::ErrorKind, String),
}

impl FailedDial {
    fn new(error: &ConnectError, credential: Credential, generation: u64) -> Self {
        let (rejected, transport) = match error {
            ConnectError::Rejected { response } => (
                Some(response.clone()),
                (io::ErrorKind::PermissionDenied, String::new()),
            ),
            ConnectError::Transport { source } => (None, (source.kind(), source.to_string())),
        };
        Self {
            at: Instant::now(),
            credential,
            generation,
            rejected,
            transport,
        }
    }

    fn error(&self) -> ConnectError {
        match &self.rejected {
            Some(response) => ConnectError::Rejected {
                response: response.clone(),
            },
            None => io::Error::new(self.transport.0, self.transport.1.clone()).into(),
        }
    }
}

fn lock(state: &StateMutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
pub(crate) struct Pool {
    state: Arc<StateMutex<State>>,
    setup: Mutex<()>,
}

impl Pool {
    /// A stream over `carrier` failed in transport. Retire that connection
    /// if the relay has been silent on it for [`SILENCE`], so that the next
    /// stream dials afresh rather than waiting out Quinn's idle timeout on a
    /// path that no longer delivers. One the relay is still heard on is
    /// working, whatever failed — a stream reset, a slow service behind it —
    /// and stays: retiring it would only leave its long-lived streams
    /// holding a relay slot beside its replacement's. No other connection is
    /// looked at, so one tunnel's failure retires neither another
    /// credential's connection nor, reported late, its own one's replacement.
    pub fn carrier_failed(&self, carrier: CarrierId) {
        let mut state = lock(&self.state);
        let now = Instant::now();
        if let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == carrier)
            && now.duration_since(entry.heard(now)) >= SILENCE
        {
            entry.pooled = false;
        }
    }

    fn cached(
        &self,
        addrs: &ResolvedAddrs,
        credential: Credential,
        generation: u64,
    ) -> Option<(CarrierId, Arc<QuicConnection>)> {
        let mut state = lock(&self.state);
        // Closed connections are dropped and an earlier network's retired;
        // another runtime's are left for it.
        state.entries.retain_mut(|entry| {
            if entry.generation < generation {
                entry.pooled = false;
            }
            entry.is_open()
        });
        let runtime = tokio::runtime::Handle::current().id();
        let entry = state.entries.iter_mut().find(|entry| {
            entry.pooled
                && entry.runtime == runtime
                && entry.generation == generation
                && entry.credential == credential
                && addrs.as_slice().contains(&entry.shared.relay_addr)
                && Arc::strong_count(&entry.shared) <= REFERENCES_PER_CONNECTION
        })?;
        let now = Instant::now();
        entry.last_open = now;
        entry.heard(now);
        Some((entry.id, entry.shared.clone()))
    }

    /// A stream over a connection for `credential`. `carrier` is set to that
    /// connection as soon as it is chosen, before waiting for stream credit,
    /// which a path that stopped delivering withholds: dropped streams
    /// return theirs only once the relay acknowledges them. A caller whose
    /// deadline cancels the wait still knows which connection to blame.
    pub async fn connect(
        &self,
        addrs: &ResolvedAddrs,
        credential: Credential,
        generation: u64,
        carrier: &mut Option<CarrierId>,
    ) -> Result<QuicStream, ConnectError> {
        let (chosen, reused) = if let Some(chosen) = self.cached(addrs, credential, generation) {
            (chosen, true)
        } else {
            // Serialize cold dials, but keep observations and cached opens independent
            // of network I/O. Recheck after acquiring admission to deduplicate callers.
            let queued = Instant::now();
            let _setup = self.setup.lock().await;
            match self.cached(addrs, credential, generation) {
                Some(chosen) => (chosen, true),
                None => {
                    // A dial that failed while this caller waited failed for it.
                    let failed = lock(&self.state)
                        .failed
                        .as_ref()
                        .filter(|failed| {
                            failed.at >= queued
                                && failed.credential == credential
                                && failed.generation == generation
                        })
                        .map(FailedDial::error);
                    if let Some(error) = failed {
                        return Err(error);
                    }
                    (self.dial(addrs, credential, generation).await?, false)
                }
            }
        };
        let (id, shared) = chosen;
        *carrier = Some(id);
        // Stream-credit waiting must not block other callers or observations.
        // No request is replayed here, including when opening a pooled stream fails.
        let pair = match shared.connection.open_bi().await {
            Ok(pair) => pair,
            Err(error) => {
                lock(&self.state).error = Some(error.to_string());
                return Err(io::Error::other(error).into());
            }
        };
        let mut state = lock(&self.state);
        state.opened += 1;
        state.reused += u64::from(reused);
        state.error = None;
        Ok(QuicStream::new(shared, pair))
    }

    async fn dial(
        &self,
        addrs: &ResolvedAddrs,
        credential: Credential,
        generation: u64,
    ) -> Result<(CarrierId, Arc<QuicConnection>), ConnectError> {
        let dialled =
            crate::addr::race_candidates(addrs.as_slice(), CONNECT_TIMEOUT, move |addr| {
                dial(addr, credential)
            })
            .await;
        let mut state = lock(&self.state);
        let shared = match dialled {
            Ok(shared) => shared,
            Err(error) => {
                let error = match error {
                    // A relay this process has reached over QUIC before was
                    // most likely restarted or rolled back without it: worth
                    // waiting out, slowly, rather than ending the tunnels
                    // that ran on it. One never reached over QUIC is final.
                    ConnectError::Rejected { mut response }
                        if response.code == QUIC_UNAVAILABLE && state.created > 0 =>
                    {
                        response.retryable = true;
                        response.message = "the relay stopped offering QUIC".into();
                        ConnectError::Rejected { response }
                    }
                    error => error,
                };
                state.error = Some(error.to_string());
                state.failed = Some(FailedDial::new(&error, credential, generation));
                return Err(error);
            }
        };
        let id = CarrierId(NonZeroU64::MIN.saturating_add(state.created));
        state.created += 1;
        if let Some(oldest) = (state.entries.iter().filter(|entry| entry.pooled).count()
            >= MAX_ENTRIES)
            .then(|| state.entries.iter_mut().find(|entry| entry.pooled))
            .flatten()
        {
            oldest.pooled = false;
        }
        state
            .entries
            .push(Entry::new(id, credential, generation, shared.clone()));
        let start_sweep = !std::mem::replace(&mut state.sweeping, true);
        // Spawned with the lock released: a runtime shutting down drops the
        // task at once, and its guard takes the lock to say so.
        drop(state);
        if start_sweep {
            tokio::spawn(sweep(Sweeping(Arc::downgrade(&self.state))));
        }
        Ok((id, shared))
    }

    pub fn diagnostics(&self) -> RelayDiagnostics {
        let state = lock(&self.state);
        let open = |entry: &&Entry| entry.is_open();
        // Those handed out first, then those retired.
        let connections = state
            .entries
            .iter()
            .filter(|entry| entry.pooled)
            .filter(open)
            .chain(
                state
                    .entries
                    .iter()
                    .filter(|entry| !entry.pooled)
                    .filter(open),
            )
            .map(|entry| describe(&entry.shared, entry.pooled))
            .collect();
        RelayDiagnostics {
            transport: RelayTransport::Quic,
            connections_created: state.created,
            streams_opened: state.opened,
            streams_reused: state.reused,
            last_error: state.error.clone(),
            connections,
        }
    }
}

fn describe(shared: &QuicConnection, pooled: bool) -> QuicConnectionDiagnostics {
    let connection = &shared.connection;
    let stats = connection.stats();
    QuicConnectionDiagnostics {
        connection_id: connection.stable_id() as u64,
        peer_addr: connection.remote_address(),
        pooled,
        rtt_us: connection.rtt().as_micros().min(u64::MAX.into()) as u64,
        congestion_window: stats.path.cwnd,
        mtu: stats.path.current_mtu,
        sent_packets: stats.path.sent_packets,
        lost_packets: stats.path.lost_packets,
        congestion_events: stats.path.congestion_events,
        sent_bytes: stats.udp_tx.bytes,
        received_bytes: stats.udp_rx.bytes,
    }
}

/// A sweep's claim on its pool. Made before the task is spawned, so that a
/// task dropped unpolled, with its runtime, still lets a dial start another.
struct Sweeping(Weak<StateMutex<State>>);

impl Drop for Sweeping {
    fn drop(&mut self) {
        if let Some(state) = self.0.upgrade() {
            lock(&state).sweeping = false;
        }
    }
}

/// Close the pool's connections, handed out or retired, that have had no
/// stream open for [`IDLE_CLOSE`], until the pool is gone or has none left;
/// the next dial starts it again.
async fn sweep(mut sweeping: Sweeping) {
    let state = sweeping.0.clone();
    let mut tick = tokio::time::interval(SWEEP_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tick.tick().await;
        let Some(state) = state.upgrade() else {
            return;
        };
        let mut state = lock(&state);
        let now = Instant::now();
        state.entries.retain_mut(|entry| {
            // Any reference past the pool's own is a stream still open: the
            // idle time counts from when the last one was seen.
            if Arc::strong_count(&entry.shared) > 1 {
                entry.last_open = now;
            }
            entry.heard(now);
            if now.duration_since(entry.last_open) >= IDLE_CLOSE {
                entry.shared.connection.close(0u32.into(), b"idle");
                return false;
            }
            entry.is_open()
        });
        if state.entries.is_empty() {
            // Decided under the lock a dial takes to start a sweep, so the
            // two cannot miss each other.
            state.sweeping = false;
            sweeping.0 = Weak::new();
            return;
        }
    }
}

async fn dial(
    addr: SocketAddr,
    credential: Credential,
) -> Result<Arc<QuicConnection>, ConnectError> {
    tokio::time::timeout(CONNECT_TIMEOUT, dial_inner(addr, credential))
        .await
        .unwrap_or_else(|_| Err(io::Error::from(io::ErrorKind::TimedOut).into()))
}

async fn dial_inner(
    addr: SocketAddr,
    credential: Credential,
) -> Result<Arc<QuicConnection>, ConnectError> {
    // Bootstrap only identity metadata over the existing authenticated transport.
    let (relay_addr, capability) = super::discover(addr, credential).await?;
    let bind_ip = match relay_addr.ip() {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    let endpoint = quinn::Endpoint::client(SocketAddr::new(bind_ip, 0))?;
    endpoint.set_default_client_config(client_config(&capability.certificate)?);
    // The relay's own address with QUIC's port, keeping an IPv6 scope.
    let mut quic_addr = relay_addr;
    quic_addr.set_port(capability.port);
    let connection = endpoint
        .connect(quic_addr, SERVER_NAME)
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
        if let Ok(pb_mapper_protocol::command::PbConnResponse::Error(response)) =
            pb_mapper_protocol::command::MessageSerializer::decode(&reply)
        {
            return Err(ConnectError::Rejected { response });
        }
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "QUIC authentication rejected",
        )
        .into());
    }
    tracing::info!(event = "relay_quic_connected", %relay_addr, peer = %shared.connection.remote_address(), connection_id = shared.connection.stable_id());
    Ok(shared)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_failure_retires_only_its_own_silent_carrier() {
        let (mut config, certificate) = server_config().unwrap();
        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(0u32.into());
        config.transport_config(Arc::new(transport));
        let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.local_addr().unwrap();
        let pool = Pool::default();
        let mut peers = Vec::new();
        // A carrier, another credential's, and the first one's replacement.
        for (id, key) in [(1, 1), (2, 2), (3, 1)] {
            let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
            endpoint.set_default_client_config(client_config(&certificate).unwrap());
            let (client, peer) =
                tokio::join!(endpoint.connect(address, SERVER_NAME).unwrap(), async {
                    server.accept().await.unwrap().await.unwrap()
                });
            let connection = client.unwrap();
            peers.push(peer);
            let shared = Arc::new(QuicConnection {
                connection,
                endpoint,
                relay_addr: address,
            });
            lock(&pool.state).entries.push(Entry::new(
                CarrierId(NonZeroU64::new(id).unwrap()),
                Credential::Admin([key; 32]),
                0,
                shared,
            ));
        }
        let addresses = ResolvedAddrs::from_candidates(vec![address]).unwrap();
        let mut selected = None;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(1),
                pool.connect(&addresses, Credential::Admin([1; 32]), 0, &mut selected),
            )
            .await
            .is_err(),
            "the peer offered no stream credit"
        );
        let selected = selected.expect("cancellation must retain the selected carrier");
        let pooled = || {
            lock(&pool.state)
                .entries
                .iter()
                .map(|entry| entry.pooled)
                .collect::<Vec<_>>()
        };
        // Make silence deterministic, independent of handshake ACK timing.
        {
            let mut state = lock(&pool.state);
            assert_eq!(selected, state.entries[0].id);
            for entry in &mut state.entries {
                entry.received = received(&entry.shared);
                entry.heard = Instant::now() - SILENCE;
            }
            state.entries[0].heard = Instant::now();
        }
        pool.carrier_failed(selected);
        assert_eq!(pooled(), [true; 3], "a carrier still heard is working");
        lock(&pool.state).entries[0].heard = Instant::now() - SILENCE;
        pool.carrier_failed(selected);
        pool.carrier_failed(selected);
        assert_eq!(
            pooled(),
            [false, true, true],
            "once silent it is retired; another credential's and its replacement are not"
        );
    }
}
