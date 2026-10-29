//! Shared Flutter-FFI application state and module boundaries.
//!
//! ```text
//! Flutter command -> Arc<Mutex<PbMapperState>> -> configuration / runtime / status
//!                                             -> change events back to Flutter
//! ```
//!
//! Slow DNS, bind, and connectivity work is deliberately performed outside the global
//! state lock. Per-key claims prevent duplicate setup while keeping unrelated UI reads
//! and operations responsive.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex as SyncMutex;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use pb_mapper_auth::{AuthConfig, AuthRuntime};
use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, Connection, DirectStage, PathPolicy, RecoveryFailure,
    RegisterOptions, RegisterRequest, Registration, RelayTransport, RemoteId, Transport,
    TunnelDiagnostics, TunnelEventKind, TunnelStatus,
};
use pb_mapper_core::checksum::{
    Credential, get_process_credential, parse_credential, set_process_msg_header_key,
};
use pb_mapper_core::config::{ResolvedAddrs, resolve_addrs_async, resolve_pb_mapper_server_async};
use pb_mapper_server::{
    DirectServerConfig, QuicServerConfig, RelayServerOptions, ServerStatusInfo,
    run_server_on_listener_with_options,
};
use uni_stream::stream::{ListenerProvider, TcpListenerProvider, UdpListenerProvider};

use crate::ctl::Origin;
use crate::error::{CtlError, ErrorCode};
use crate::events;
use crate::relay::{no_quic, sdk_error};

const STATUS_CACHE_TTL: Duration = Duration::from_secs(2);
const FORCE_REFRESH_TIMEOUT: Duration = Duration::from_millis(5000);

/// What the relay answered: its services, the routing table, and how long
/// the services took to come back.
type RelayAnswer = (Vec<String>, RemoteId, Duration);

/// What the relay holds, asked over the carrier this computer uses within
/// [`FORCE_REFRESH_TIMEOUT`]; and with `ask_quic`, beside it and over TCP
/// whichever carrier that is, what the relay says about QUIC: the UDP port it
/// offers it on, or `None` for none, or nothing when it did not answer.
async fn fetch_real_status(
    client: &Client,
    ask_quic: bool,
) -> (Result<RelayAnswer, CtlError>, Option<Option<u16>>) {
    let answer = async {
        let keys = async {
            let started = Instant::now();
            (client.list_keys().await, started.elapsed())
        };
        let ((keys_result, latency), remote_id_result) = tokio::join!(keys, client.remote_id());
        // The reason as it is: Settings' Test shows it to the user, after its
        // own "The relay didn't answer".
        let services = keys_result.map_err(sdk_error)?;
        let remote_id = remote_id_result.unwrap_or_else(|e| {
            tracing::warn!("Failed to get remote-id data: {}, using empty data", e);
            RemoteId {
                server_map: String::new(),
                active: String::new(),
                idle: String::new(),
            }
        });
        Ok((services, remote_id, latency))
    };
    let answer = async {
        tokio::time::timeout(FORCE_REFRESH_TIMEOUT, answer)
            .await
            .unwrap_or_else(|_| {
                Err(CtlError::timeout(format!(
                    "{} did not answer within {} seconds",
                    client.server(),
                    FORCE_REFRESH_TIMEOUT.as_secs()
                )))
            })
    };
    // Bounded by the SDK's own timeout; outside the one above, so a TCP
    // question that stalls cannot make a relay that answered look silent.
    let quic = async {
        if !ask_quic {
            return None;
        }
        client
            .relay_quic_port()
            .await
            .inspect_err(|e| tracing::debug!("The relay did not say whether it offers QUIC: {e}"))
            .ok()
    };
    tokio::join!(answer, quic)
}

/// Whether the relay can carry a tunnel now, asked before one starts so that
/// a relay it cannot use is refused with its reason instead of retried: one
/// that cannot be reached, refuses this credential for good, or over QUIC,
/// offers no QUIC. Anything that may clear by itself — a slow answer, a
/// refusal the relay calls retryable — is left to the tunnel's own retries.
///
/// One authenticated question over TCP, the one QUIC discovery asks. A bare
/// connect would prove less, and the relay logs each as a failed login.
async fn preflight(client: &Client) -> Result<(), CtlError> {
    use pb_mapper_client::sdk::Error as SdkError;
    match (client.relay_transport(), client.relay_quic_port().await) {
        (
            _,
            Err(
                error @ (SdkError::Connect { .. }
                | SdkError::Address { .. }
                | SdkError::Remote {
                    retryable: false, ..
                }),
            ),
        ) => Err(sdk_error(error)),
        (_, Err(error)) => {
            tracing::debug!("Starting a tunnel the relay could not be asked about: {error}");
            Ok(())
        }
        (RelayTransport::Quic, Ok(None)) => Err(no_quic()),
        _ => Ok(()),
    }
}

fn cache_is_stale(last_update: Option<Instant>, ttl: Duration) -> bool {
    match last_update {
        Some(ts) => ts.elapsed() > ttl,
        None => true,
    }
}

fn normalize_msg_header_key(msg_header_key: String) -> Result<String, CtlError> {
    let normalized = msg_header_key.trim().to_string();
    if normalized.is_empty() {
        return Ok(normalized);
    }
    parse_credential(&normalized).map_err(CtlError::invalid_argument)?;
    Ok(normalized)
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ServiceConfigData {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_encryption: bool,
    pub enable_keep_alive: bool,
    pub created_at: SystemTime,
    /// Whether a connection may reach it directly rather than through the
    /// relay. On unless turned off: a share saved before there was a choice
    /// allows it, as `register` does.
    #[serde(default = "direct_by_default")]
    pub allow_direct: bool,
    /// Whether to ask the gateway to map the direct UDP port. Off unless
    /// turned on, and kept while direct is off, to apply when it is on again.
    #[serde(default)]
    pub port_mapping: bool,
}

fn direct_by_default() -> bool {
    true
}

/// How a tunnel may go past the relay, as it is saved with the tunnel.
#[derive(Clone, Copy)]
pub struct DirectOptions {
    pub allow_direct: bool,
    /// Only acted on while `allow_direct` is.
    pub port_mapping: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ServiceConfigStore {
    pub services: HashMap<String, ServiceConfigData>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ClientConfigData {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_keep_alive: bool,
    pub created_at: SystemTime,
    /// The namespace the service is in, when it is not this credential's own:
    /// an administrator connecting to a temporary credential's service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<u64>,
    /// Whether each session tries a direct path before the relay. Off is
    /// `connect --relay-only`.
    #[serde(default = "direct_by_default")]
    pub allow_direct: bool,
    /// As [`ServiceConfigData::port_mapping`].
    #[serde(default)]
    pub port_mapping: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ClientConfigStore {
    pub clients: HashMap<String, ClientConfigData>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    pub server_address: String,
    pub keep_alive_enabled: bool,
    pub msg_header_key: String,
    /// How this computer reaches the relay: TCP, as a config saved before
    /// there was a choice, or QUIC. A tunnel keeps the one it started with.
    pub relay_transport: RelayTransport,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_address: "localhost:7666".to_string(),
            keep_alive_enabled: true,
            msg_header_key: String::new(),
            relay_transport: RelayTransport::Tcp,
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServiceConfigInfo {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_encryption: bool,
    pub enable_keep_alive: bool,
    pub allow_direct: bool,
    pub port_mapping: bool,
    pub status: String,
    pub status_message: String,
    /// How its sessions travel, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathInfo>,
    /// How it reaches the relay, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_transport: Option<RelayTransport>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientConfigInfo {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_keep_alive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<u64>,
    /// While it runs, the namespace the relay routes it in: [`Self::namespace`],
    /// or else that of the credential it was started with, which a credential
    /// changed since does not move.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routed_namespace: Option<u64>,
    pub allow_direct: bool,
    pub port_mapping: bool,
    pub status: String,
    pub status_message: String,
    /// How its sessions travel, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathInfo>,
    /// How it reaches the relay, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_transport: Option<RelayTransport>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// How a running tunnel's sessions travel: each TCP connection or UDP flow
/// through it takes a direct path or the relay, decided when it opens.
///
/// A connection counts both paths and its direct attempts. A share counts
/// only the sessions that reached it directly: the rest come in over its
/// registration with the relay, which does not tell them apart.
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PathInfo {
    /// Sessions open now, on each path.
    pub active_direct: u64,
    pub active_relay: u64,
    /// Sessions opened since the tunnel started, on each path.
    pub direct_sessions: u64,
    pub relay_sessions: u64,
    /// The stage reached by a direct attempt in flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<DirectStage>,
    /// How the latest attempt ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<PathOutcome>,
}

/// How a direct attempt ended.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "path", rename_all = "camelCase")]
pub enum PathOutcome {
    /// It went direct, set up in this long.
    Direct {
        #[serde(rename = "setupMs")]
        setup_ms: u64,
    },
    /// It gave way to the relay at this stage. Later sessions go straight to
    /// the relay for a while (10 s, doubling to 5 min) before trying again.
    Relay { stage: DirectStage },
}

impl PathInfo {
    /// What the diagnostics of a running tunnel say about its paths; None
    /// before any session or attempt.
    fn of(diagnostics: &TunnelDiagnostics) -> Option<Self> {
        let direct = diagnostics.direct.as_ref()?;
        let last = diagnostics
            .events
            .iter()
            .rev()
            .find_map(|event| match event.kind {
                TunnelEventKind::DirectEstablished { setup_ms } => {
                    Some(PathOutcome::Direct { setup_ms })
                }
                TunnelEventKind::DirectFallback { stage } => Some(PathOutcome::Relay { stage }),
                _ => None,
            });
        Some(Self {
            active_direct: direct.active_direct,
            active_relay: direct.active_relay,
            direct_sessions: direct.direct_sessions,
            relay_sessions: direct.relay_sessions,
            attempt: direct.attempt.map(|attempt| attempt.stage),
            last,
        })
    }

    /// What a change of is worth telling the window about: which paths are
    /// in use, an attempt and how the last one ended, but not the counts.
    fn shape(&self) -> (bool, bool, Option<DirectStage>, Option<bool>) {
        (
            self.active_direct > 0,
            self.active_relay > 0,
            self.attempt,
            self.last
                .map(|last| matches!(last, PathOutcome::Direct { .. })),
        )
    }
}

/// The relay this computer runs itself. The default is a stopped one.
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocalServerStatus {
    pub is_running: bool,
    /// The port it listens on, while it runs: the page shows and dials this,
    /// not whatever its port field last held.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The UDP port it takes QUIC on, while it runs with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quic_port: Option<u16>,
    pub active_connections: u32,
    pub registered_services: u32,
    pub uptime_seconds: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatusDetail {
    pub server_available: bool,
    pub registered_services: Vec<String>,
    pub server_map: String,
    pub active_connections: String,
    pub idle_connections: String,
    /// Why the relay did not answer, for the person who asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How long it took to answer, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// The UDP port the relay offers QUIC on, when it said it offers it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quic_port: Option<u16>,
    /// Whether the relay said whether it offers QUIC, which it is asked over
    /// TCP: so even when the carrier this computer uses does not get through.
    pub quic_known: bool,
}

/// One control connection the server is holding for a service key.
///
/// Mirrors `PbServiceConnStatus` from the protocol. Every field is real,
/// which is the point: the string this replaces could only ever say that a
/// connection existed.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServiceConnInfo {
    pub conn_id: u32,
    pub generation: u64,
    pub protocol_version: u16,
    pub healthy: bool,
    pub last_rx_age_ms: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatusResponse {
    pub service_key: String,
    pub status: String,
    pub message: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatusResponse {
    pub service_key: String,
    pub status: String,
    pub message: String,
}

#[derive(Clone)]
#[allow(dead_code)]
struct ServiceInfo {
    service_key: String,
    protocol: String,
    local_address: String,
    status: String,
}

#[derive(Clone)]
#[allow(dead_code)]
struct ConnectionInfo {
    service_key: String,
    client_id: String,
    status: String,
}

/// Holds a service key for the duration of one setup, and releases it however
/// that setup ends.
///
/// Setting a tunnel up runs its slow parts — DNS, the preflight connect — with
/// the state lock released. That reopens a window the old always-locked version
/// closed by accident: two callers, say the window and a terminal, could both
/// get through the preflight and both insert, and the second would overwrite
/// the first's [`JoinHandle`], leaving a tunnel running that nothing could
/// abort. Holding the key across the gap is what closes it again.
///
/// The set is behind a blocking mutex rather than tokio's so that `Drop` can
/// release it; it is only ever held for a set insert or remove.
struct KeyClaim {
    key: String,
    claims: Arc<SyncMutex<HashSet<String>>>,
}

impl Drop for KeyClaim {
    fn drop(&mut self) {
        self.claims.lock().remove(&self.key);
    }
}

/// Claims `key`, or reports that someone else is already setting it up.
fn claim_key(
    claims: &Arc<SyncMutex<HashSet<String>>>,
    key: &str,
    what: &str,
) -> Result<KeyClaim, CtlError> {
    let mut guard = claims.lock();
    if !guard.insert(key.to_string()) {
        return Err(CtlError::already_in_progress(format!(
            "'{key}' is already {what}"
        )));
    }
    Ok(KeyClaim {
        key: key.to_string(),
        claims: claims.clone(),
    })
}

#[derive(Clone)]
struct PinnedTunnel {
    credential: Credential,
    endpoint: ResolvedAddrs,
    /// The relay address it was started against, as configured then.
    relay: String,
    /// The namespace a connection subscribes in, when not its credential's own.
    namespace: Option<u64>,
}

impl PinnedTunnel {
    /// The namespace the relay routes this tunnel in.
    fn routed_namespace(&self) -> u64 {
        self.namespace.unwrap_or_else(|| self.credential.key_id())
    }

    /// Whether it runs on `relay`: started under that address, or dialling
    /// any address `endpoint` resolves to, since the same relay may have been
    /// written another way (a hostname, its IP) when it started.
    fn is_on(&self, relay: &str, endpoint: Option<&ResolvedAddrs>) -> bool {
        self.relay == relay
            || endpoint.is_some_and(|endpoint| {
                self.endpoint
                    .as_slice()
                    .iter()
                    .any(|addr| endpoint.as_slice().contains(addr))
            })
    }
}

/// A running tunnel, as the SDK runs it for the CLI too. Dropping it stops
/// the tunnel.
pub(crate) enum TunnelHandle {
    Share(Registration),
    Connect(Connection),
}

impl TunnelHandle {
    fn status(&self) -> TunnelStatus {
        match self {
            Self::Share(handle) => handle.status(),
            Self::Connect(handle) => handle.status(),
        }
    }

    pub(crate) fn diagnostics(&self) -> TunnelDiagnostics {
        match self {
            Self::Share(handle) => handle.diagnostics(),
            Self::Connect(handle) => handle.diagnostics(),
        }
    }

    pub(crate) fn subscribe_events(&self) -> pb_mapper_client::sdk::TunnelEvents {
        match self {
            Self::Share(handle) => handle.subscribe_events(),
            Self::Connect(handle) => handle.subscribe_events(),
        }
    }

    fn subscribe(&self) -> tokio::sync::watch::Receiver<TunnelStatus> {
        match self {
            Self::Share(handle) => handle.subscribe(),
            Self::Connect(handle) => handle.subscribe(),
        }
    }

    /// Its state as the list shows it, and how its sessions travel.
    fn view(&self) -> TunnelView {
        let diagnostics = self.diagnostics();
        let transport = diagnostics.relay_transport;
        let (status, message) = match self.status() {
            TunnelStatus::Connected => ("running", String::new()),
            // Still dialling, or failed and trying again: "failed" is what a
            // tunnel whose last attempt failed has always reported, while its
            // retry loop runs on.
            TunnelStatus::Starting | TunnelStatus::Retrying => {
                match diagnostics
                    .last_failure
                    .filter(|_| diagnostics.consecutive_failures > 0)
                {
                    Some(failure) => ("failed", failure_message(failure, self, transport)),
                    None => ("retrying", "Connecting to the relay".to_string()),
                }
            }
            TunnelStatus::Failed(reason) => ("failed", reason),
            // Nothing here stops a tunnel but removing it, so a stopped one
            // that is still held ended on its own.
            TunnelStatus::Stopped => ("failed", "The tunnel stopped".to_string()),
        };
        TunnelView {
            status,
            message,
            path: PathInfo::of(&diagnostics),
            transport: Some(transport),
        }
    }
}

/// What the relay's last answer, or the lack of one, means for a tunnel.
/// Over QUIC the relay is reached on a UDP port of its own, which a network
/// can block while TCP gets through, so that is named.
fn failure_message(
    failure: RecoveryFailure,
    handle: &TunnelHandle,
    transport: RelayTransport,
) -> String {
    let quic = transport == RelayTransport::Quic;
    match failure {
        RecoveryFailure::Dns => "The relay's address does not resolve",
        RecoveryFailure::Transport if quic => {
            "Cannot reach the relay over QUIC; check that it offers QUIC and its UDP port is open"
        }
        RecoveryFailure::Transport => "Cannot reach the relay",
        RecoveryFailure::Timeout if quic => "The relay did not answer over QUIC in time",
        RecoveryFailure::Timeout => "The relay did not answer in time",
        RecoveryFailure::ServiceUnavailable => match handle {
            TunnelHandle::Share(_) => "The relay cannot take this service now",
            TunnelHandle::Connect(_) => "The relay does not list this service",
        },
        RecoveryFailure::Rejected => "The relay refused this computer's credential",
        RecoveryFailure::NetworkChanged => "The network changed; reconnecting",
    }
    .to_string()
}

/// A tunnel's state as the list shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
struct TunnelView {
    status: &'static str,
    message: String,
    path: Option<PathInfo>,
    transport: Option<RelayTransport>,
}

impl TunnelView {
    /// The view of a tunnel this computer has configured but is not running.
    fn stopped(message: &str) -> Self {
        Self {
            status: "stopped",
            message: message.to_string(),
            path: None,
            transport: None,
        }
    }
}

struct TunnelRuntime {
    /// Shared only with the task that reports its changes, which holds it
    /// weakly: removing the runtime still stops the tunnel.
    handle: Arc<TunnelHandle>,
    pin: PinnedTunnel,
}

/// This computer's tunnels on one relay, as rotating that relay's
/// administrator key leaves them. See [`PbMapperState::tunnels_on`].
#[derive(Default, Debug)]
pub struct RelayTunnels {
    /// On the retired administrator key: they start again on the new one.
    pub shares: Vec<String>,
    pub connections: Vec<String>,
    /// On another credential, a temporary one, which the rotation erased.
    /// The administrator key is not theirs to take: a tunnel started under
    /// it lands in the administrator's namespace, not its own.
    pub stranded: Vec<String>,
}

/// Stops the tunnel under `key`, if there is one: dropping its handle does,
/// since the SDK's `LiveTunnel` cancels and aborts its worker on drop.
fn abort_runtime(map: &mut HashMap<String, TunnelRuntime>, key: &str) -> bool {
    map.remove(key).is_some()
}

/// Everything [`PbMapperState::finish_register`] needs once the slow work is
/// done, the tunnel itself included: it starts before the commit, and stops
/// again if the commit fails.
struct RegisterCommit {
    service_key: String,
    local_address: String,
    protocol: String,
    enable_encryption: bool,
    enable_keep_alive: bool,
    allow_direct: bool,
    port_mapping: bool,
    handle: Registration,
    pin: PinnedTunnel,
}

/// Everything [`PbMapperState::finish_connect`] needs once the slow work is
/// done. See [`RegisterCommit`].
struct ConnectCommit {
    service_key: String,
    local_address: String,
    protocol: String,
    enable_keep_alive: bool,
    allow_direct: bool,
    port_mapping: bool,
    handle: Connection,
    pin: PinnedTunnel,
}

/// The transport `protocol` names: TCP, or else UDP.
fn transport_of(protocol: &str) -> Transport {
    if protocol.eq_ignore_ascii_case("TCP") {
        Transport::Tcp
    } else {
        Transport::Udp
    }
}

/// What the SDK said when it refused a tunnel before starting it.
fn sdk_refusal(error: pb_mapper_client::sdk::Error) -> CtlError {
    CtlError::invalid_argument(error.to_string())
}

pub struct PbMapperState {
    server_handle: Option<JoinHandle<()>>,
    server_auth: Option<AuthRuntime>,
    server_shutdown_token: Option<CancellationToken>,
    server_status_sender:
        Option<tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<ServerStatusInfo>>>,
    server_start_time: Option<SystemTime>,
    server_port: Option<u16>,
    server_quic_port: Option<u16>,
    /// The client this window's own questions to the relay go through, kept
    /// for as long as the relay settings it was made from stay as they are:
    /// a QUIC connection, and the relay's resolved address, then outlive
    /// one question. Tunnels share both with it.
    relay_client: SyncMutex<Option<(AppConfig, Client)>>,
    /// What the relay said about QUIC for these settings: asked again only by
    /// a forced check (Settings' Test) or a change of settings, not every
    /// periodic one, since a relay from before QUIC logs the question.
    quic_offer: SyncMutex<Option<(AppConfig, Option<u16>)>>,
    registered_services: Arc<RwLock<HashMap<String, ServiceInfo>>>,
    active_connections: Arc<RwLock<HashMap<String, ConnectionInfo>>>,
    service_runtime: HashMap<String, TunnelRuntime>,
    client_runtime: HashMap<String, TunnelRuntime>,
    config: AppConfig,
    config_dir: PathBuf,
    app_directory_path: Option<String>,
    local_server_status_cache: Arc<RwLock<LocalServerStatus>>,
    local_server_status_last_update: Arc<RwLock<Option<Instant>>>,
    local_server_status_refreshing: Arc<AtomicBool>,
    /// Keys currently being set up. See [`KeyClaim`]. Separate sets because a
    /// key can legitimately be registered and connected to at the same time.
    registering: Arc<SyncMutex<HashSet<String>>>,
    connecting: Arc<SyncMutex<HashSet<String>>>,
}

mod configuration;
mod runtime;
mod status;

/// Registers a service, holding the state lock only for the bookkeeping.
///
/// Three phases. The middle one is the point: resolving addresses and dialling
/// the pb-mapper server are unbounded — a blackholed address costs the whole
/// connect timeout — and while they ran under the lock every other caller waited
/// them out, including a status read that wanted nothing from this key at all.
pub async fn register_service(
    state: &Arc<Mutex<PbMapperState>>,
    service_key: String,
    local_address: String,
    protocol: String,
    enable_encryption: bool,
    enable_keep_alive: bool,
    direct: DirectOptions,
) -> Result<(), CtlError> {
    let DirectOptions {
        allow_direct,
        port_mapping,
    } = direct;
    // 1. Claim the key and take what the slow work needs. Microseconds.
    //    `_claim` is held to the end of the function on purpose: dropping it
    //    early would release the key while the setup is still running.
    //    The credential and carrier are captured with the relay address so a
    //    later config change cannot pair a new key with the already-resolved
    //    socket.
    let (_claim, server_address, credential, relay_transport) = {
        let state = state.lock().await;
        let claim = state.claim_registering(&service_key)?;
        let credential = get_process_credential().map_err(CtlError::invalid_argument)?;
        (
            claim,
            state.config.server_address.clone(),
            credential,
            state.config.relay_transport,
        )
    };

    // 2. The slow parts, with the lock released, ending with the tunnel
    //    started.
    resolve_addrs_async(&local_address)
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid local address: {e}")))?;
    let remote_addrs = resolve_pb_mapper_server_async(Some(&server_address))
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid server address: {e}")))?;
    let client = Client::from_credential(&server_address, credential, enable_keep_alive, None)
        .with_relay_transport(relay_transport)
        .map_err(sdk_refusal)?;
    // Surface a relay this tunnel cannot use now, rather than retrying it.
    preflight(&client).await?;
    let handle = client
        .register_with_options(
            RegisterRequest {
                key: service_key.clone(),
                local_addr: local_address.clone(),
                transport: transport_of(&protocol),
                codec: enable_encryption,
                force_namespace: false,
            },
            RegisterOptions {
                allow_direct,
                idle_timeout: None,
                port_mapping,
            },
        )
        .await
        .map_err(sdk_refusal)?;

    // 3. Commit. Microseconds again.
    state
        .lock()
        .await
        .finish_register(RegisterCommit {
            service_key,
            local_address,
            protocol,
            enable_encryption,
            enable_keep_alive,
            allow_direct,
            port_mapping,
            handle,
            pin: PinnedTunnel {
                credential,
                endpoint: remote_addrs,
                relay: server_address,
                namespace: None,
            },
        })
        .await
}

/// Connects to a registered service. Phased exactly like [`register_service`];
/// the slow step here is the local bind that proves the port is free.
/// `namespace` names whose service it is when that is not this credential's
/// own, which only the administrator key may ask for. With `allow_direct`
/// each session tries a direct path first, and the relay when that fails.
pub async fn connect_service(
    state: &Arc<Mutex<PbMapperState>>,
    service_key: String,
    local_address: String,
    protocol: String,
    enable_keep_alive: bool,
    namespace: Option<u64>,
    direct: DirectOptions,
) -> Result<(), CtlError> {
    let DirectOptions {
        allow_direct,
        port_mapping,
    } = direct;
    let (_claim, server_address, credential, relay_transport) = {
        let state = state.lock().await;
        let claim = state.claim_connecting(&service_key)?;
        let credential = get_process_credential().map_err(CtlError::invalid_argument)?;
        (
            claim,
            state.config.server_address.clone(),
            credential,
            state.config.relay_transport,
        )
    };

    let local_addrs = resolve_addrs_async(&local_address)
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid local address: {e}")))?;
    let remote_addrs = resolve_pb_mapper_server_async(Some(&server_address))
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid server address: {e}")))?;

    // Preflight local bind to detect "port already in use" before starting client.
    let transport = transport_of(&protocol);
    let bound = match transport {
        Transport::Tcp => TcpListenerProvider::bind(local_addrs.as_slice())
            .await
            .map(drop),
        Transport::Udp => UdpListenerProvider::bind(local_addrs.as_slice())
            .await
            .map(drop),
    };
    bound.map_err(|e| {
        CtlError::address_in_use(format!("Failed to bind local address {local_address}: {e}"))
    })?;
    let client = Client::from_credential(&server_address, credential, enable_keep_alive, namespace)
        .with_relay_transport(relay_transport)
        .map_err(sdk_refusal)?;
    // A relay that is down is retried, as it always was: the connection
    // listens locally meanwhile. One that offers no QUIC never will.
    if relay_transport == RelayTransport::Quic {
        preflight(&client).await?;
    }
    let handle = client
        .connect_with_options(
            ConnectRequest {
                key: service_key.clone(),
                local_addr: local_address.clone(),
                transport,
            },
            ConnectOptions {
                path: if allow_direct {
                    PathPolicy::Auto
                } else {
                    PathPolicy::RelayOnly
                },
                port_mapping,
            },
        )
        .await
        .map_err(sdk_refusal)?;

    state
        .lock()
        .await
        .finish_connect(ConnectCommit {
            service_key,
            local_address,
            protocol,
            enable_keep_alive,
            allow_direct,
            port_mapping,
            handle,
            pin: PinnedTunnel {
                credential,
                endpoint: remote_addrs,
                relay: server_address,
                namespace,
            },
        })
        .await
}

#[cfg(test)]
// The crate denies these so a panic never reaches a Flutter caller through the
// FFI boundary. In a test a panic *is* the failure report.
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK;

    use super::*;
    use crate::error::ErrorCode;

    type ProcessCredentialGuard = tokio::sync::MutexGuard<'static, ()>;

    /// A state rooted in a temporary directory, so a test never reads or writes
    /// the real user config.
    ///
    /// Building a state applies its stored `MSG_HEADER_KEY` to the process, and
    /// a fresh temporary directory has no stored key — so this *clears* the
    /// process credential. That is process-global, so the returned guard is what
    /// keeps one test's state from wiping the credential another test just set.
    /// Hold it for the whole test, not just the construction.
    async fn temp_state(
        name: &str,
    ) -> (ProcessCredentialGuard, Arc<Mutex<PbMapperState>>, PathBuf) {
        let guard = PROCESS_CREDENTIAL_TEST_LOCK.lock().await;
        let root = std::env::temp_dir().join(format!(
            "pb-mapper-ffi-test-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let state = PbMapperState::new(Some(root.to_string_lossy().into_owned()));
        (guard, Arc::new(Mutex::new(state)), root)
    }

    #[tokio::test]
    async fn ui_server_uses_its_writable_config_directory_and_reports_readiness() {
        let (_process_credential_guard, state, root) = temp_state("server-auth-path").await;
        let quic_port = std::net::UdpSocket::bind("0.0.0.0:0")
            .and_then(|socket| socket.local_addr())
            .unwrap()
            .port();
        let auth_dir = {
            let mut state = state.lock().await;
            let auth_dir = state.config_dir.join("auth");
            state
                .start_server(0, false, Some(quic_port))
                .await
                .expect("UI server should bind and initialize authentication");
            assert!(state.server_handle.is_some());
            let status = state.get_local_server_status().await;
            assert!(status.is_running);
            assert_eq!(status.quic_port, Some(quic_port));
            auth_dir
        };

        assert!(auth_dir.join("admin.key").is_file());
        let isolated = state
            .lock()
            .await
            .isolated_admin_key()
            .expect("embedded relay should expose its administrator key");
        assert_eq!(isolated.len(), 32);
        state
            .lock()
            .await
            .stop_server()
            .await
            .expect("UI server should stop cleanly");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// After a rotation only the rotated relay's tunnels on the retired key
    /// start again. One on another credential would land in the
    /// administrator's namespace under the new key, so it is only named; one
    /// on another relay is left alone. The relay is known by its address as
    /// well as by how it was written.
    #[tokio::test]
    async fn a_rotation_restarts_only_the_relays_tunnels_on_the_retired_key() {
        let (_process_credential_guard, state, root) = temp_state("rotation-tunnels").await;
        let retired = pb_mapper_core::checksum::parse_credential(&"a".repeat(32)).unwrap();
        let other = pb_mapper_core::checksum::parse_credential(&"b".repeat(32)).unwrap();
        let relay: std::net::SocketAddr = "127.0.0.1:7666".parse().unwrap();
        let elsewhere: std::net::SocketAddr = "127.0.0.2:7666".parse().unwrap();
        // A real handle, on a relay that is not there: only the pin matters.
        let idle = Client::from_credential("127.0.0.1:1", retired, false, None)
            .register_with_options(
                RegisterRequest {
                    key: "idle".into(),
                    local_addr: "127.0.0.1:1".into(),
                    transport: Transport::Tcp,
                    codec: false,
                    force_namespace: false,
                },
                RegisterOptions::default(),
            )
            .await
            .unwrap();
        let handle = Arc::new(TunnelHandle::Share(idle));
        let runtime = |credential, name: &str, addr| TunnelRuntime {
            handle: handle.clone(),
            pin: PinnedTunnel {
                credential,
                endpoint: ResolvedAddrs::from(addr),
                relay: name.to_owned(),
                namespace: None,
            },
        };
        let mut state = state.lock().await;
        for (key, tunnel) in [
            ("web", runtime(retired, "relay.example:7666", relay)),
            ("cam", runtime(other, "relay.example:7666", relay)),
        ] {
            state.service_runtime.insert(key.to_owned(), tunnel);
        }
        for (key, tunnel) in [
            ("db", runtime(retired, "127.0.0.1:7666", relay)),
            ("far", runtime(retired, "other.example:7666", elsewhere)),
        ] {
            state.client_runtime.insert(key.to_owned(), tunnel);
        }

        let found = state.tunnels_on("relay.example:7666", Some(&relay.into()), retired);
        assert_eq!(found.shares, ["web"]);
        assert_eq!(found.connections, ["db"]);
        assert_eq!(found.stranded, ["cam"]);
        drop(state);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each session of a tunnel goes direct when both ends allow it, and the
    /// list says so; a share that keeps to the relay sends its connections
    /// there, and the list says at which stage direct gave way.
    #[tokio::test]
    async fn the_list_says_which_path_each_tunnel_takes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (_process_credential_guard, state, root) = temp_state("paths").await;
        let relay = pb_mapper_testkit::Relay::start_with_direct(
            "ffi-paths",
            Some(DirectServerConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                advertise: "127.0.0.1:0".parse().unwrap(),
            }),
        )
        .await;
        set_process_msg_header_key(Some(pb_mapper_testkit::TEST_ADMIN_KEY)).unwrap();
        state.lock().await.config.server_address = relay.addr().to_string();
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = echo.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = socket.split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });

        let mut ports = Vec::new();
        for (key, direct) in [("direct", true), ("relayed", false)] {
            register_service(
                &state,
                key.into(),
                backend.clone(),
                "TCP".into(),
                true,
                false,
                DirectOptions {
                    allow_direct: direct,
                    port_mapping: false,
                },
            )
            .await
            .unwrap();
            let port = pb_mapper_testkit::reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
            connect_service(
                &state,
                key.into(),
                port.to_string(),
                "TCP".into(),
                false,
                None,
                DirectOptions {
                    allow_direct: true,
                    port_mapping: false,
                },
            )
            .await
            .unwrap();
            ports.push(port);
        }
        let running = || async {
            let state = state.lock().await;
            let clients = state.get_client_configs().await;
            clients.len() == 2 && clients.iter().all(|client| client.status == "running")
        };
        tokio::time::timeout(pb_mapper_testkit::READY_TIMEOUT, async {
            while !running().await {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("both connections come up");

        // One session through each, held open while the list is read.
        let mut sessions = Vec::new();
        for port in ports {
            let mut socket = tokio::net::TcpStream::connect(port).await.unwrap();
            socket.write_all(b"path").await.unwrap();
            let mut echoed = [0; 4];
            tokio::time::timeout(
                pb_mapper_testkit::READY_TIMEOUT,
                socket.read_exact(&mut echoed),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(&echoed, b"path");
            sessions.push(socket);
        }

        let state = state.lock().await;
        let path = |key: &str, infos: &[(String, Option<PathInfo>)]| {
            infos
                .iter()
                .find(|(name, _)| name == key)
                .and_then(|(_, path)| *path)
                .unwrap_or_else(|| panic!("{key} reports no path"))
        };
        let clients: Vec<_> = state
            .get_client_configs()
            .await
            .into_iter()
            .map(|client| (client.service_key, client.path))
            .collect();
        let shares: Vec<_> = state
            .get_service_configs()
            .await
            .into_iter()
            .map(|share| (share.service_key, share.path))
            .collect();

        let direct = path("direct", &clients);
        assert_eq!((direct.active_direct, direct.active_relay), (1, 0));
        assert!(matches!(direct.last, Some(PathOutcome::Direct { .. })));
        assert_eq!(path("direct", &shares).active_direct, 1);

        // The relay asked the share, which declined: no grant came back.
        let relayed = path("relayed", &clients);
        assert_eq!((relayed.active_direct, relayed.active_relay), (0, 1));
        assert_eq!(
            relayed.last,
            Some(PathOutcome::Relay {
                stage: DirectStage::Discovering
            })
        );
        // As the window reads it; `store_test.dart` parses this same shape.
        assert_eq!(
            serde_json::to_value(relayed).unwrap()["last"],
            serde_json::json!({ "path": "relay", "stage": "discovering" })
        );

        drop(sessions);
        drop(state);
        drop(relay);
        let _ = set_process_msg_header_key(None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// With QUIC chosen, tunnels and the window's relay check go over it and
    /// the list says so; switching back names the tunnels to move. A relay
    /// that offers no QUIC is refused before a tunnel starts, not retried.
    #[tokio::test]
    async fn tunnels_and_checks_follow_the_chosen_carrier() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (_process_credential_guard, state, root) = temp_state("carrier").await;
        let relay = pb_mapper_testkit::Relay::start_with_quic("ffi-carrier").await;
        set_process_msg_header_key(Some(pb_mapper_testkit::TEST_ADMIN_KEY)).unwrap();
        {
            let mut state = state.lock().await;
            state.config.server_address = relay.addr().to_string();
            state.config.relay_transport = RelayTransport::Quic;
        }
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = echo.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = socket.split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });

        // Kept on the relay, so the bytes cross the QUIC carrier.
        register_service(
            &state,
            "web".into(),
            backend.clone(),
            "TCP".into(),
            true,
            false,
            DirectOptions {
                allow_direct: false,
                port_mapping: true,
            },
        )
        .await
        .unwrap();
        let port = pb_mapper_testkit::reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
        connect_service(
            &state,
            "web".into(),
            port.to_string(),
            "TCP".into(),
            false,
            None,
            DirectOptions {
                allow_direct: false,
                port_mapping: true,
            },
        )
        .await
        .unwrap();
        tokio::time::timeout(pb_mapper_testkit::READY_TIMEOUT, async {
            loop {
                let clients = state.lock().await.get_client_configs().await;
                if clients.iter().all(|client| client.status == "running") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the connection comes up over QUIC");
        let mut socket = tokio::net::TcpStream::connect(port).await.unwrap();
        socket.write_all(b"quic").await.unwrap();
        let mut echoed = [0; 4];
        tokio::time::timeout(
            pb_mapper_testkit::READY_TIMEOUT,
            socket.read_exact(&mut echoed),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(&echoed, b"quic");

        {
            let state = state.lock().await;
            let share = &state.get_service_configs().await[0];
            let client = &state.get_client_configs().await[0];
            assert_eq!(share.relay_transport, Some(RelayTransport::Quic));
            assert_eq!(client.relay_transport, Some(RelayTransport::Quic));
            // Saved with each tunnel, but one kept on the relay asks no gateway.
            assert!(share.port_mapping && client.port_mapping);
            let tunnel = state.running_tunnel(false, "web").unwrap();
            assert!(tunnel.diagnostics().port_mapping.is_none());
            let mut legacy =
                serde_json::to_value(&state.load_service_configs().services["web"]).unwrap();
            legacy.as_object_mut().unwrap().remove("port_mapping");
            assert!(
                !serde_json::from_value::<ServiceConfigData>(legacy)
                    .unwrap()
                    .port_mapping
            );
            let detail = state.force_refresh_server_status().await.unwrap();
            assert!(detail.server_available, "{:?}", detail.error);
            assert!(detail.quic_port.is_some());
            let admin = parse_credential(pb_mapper_testkit::TEST_ADMIN_KEY).unwrap();
            let moving =
                state.on_other_carrier(&relay.addr().to_string(), None, admin, RelayTransport::Tcp);
            assert_eq!(moving.shares, ["web"]);
            assert_eq!(moving.connections, ["web"]);
        }

        let tcp_only = pb_mapper_testkit::Relay::start("ffi-carrier-tcp-only").await;
        set_process_msg_header_key(Some(pb_mapper_testkit::TEST_ADMIN_KEY)).unwrap();
        state.lock().await.config.server_address = tcp_only.addr().to_string();
        let refused = register_service(
            &state,
            "cam".into(),
            backend,
            "TCP".into(),
            false,
            false,
            DirectOptions {
                allow_direct: true,
                port_mapping: false,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code(), ErrorCode::ServerUnreachable, "{refused}");
        assert!(refused.to_string().contains("QUIC"), "{refused}");

        drop(socket);
        drop((relay, tcp_only));
        let _ = set_process_msg_header_key(None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A relay that offers QUIC on a UDP port nothing gets through to — one
    /// its firewall never opened — answers over TCP: the check says that it
    /// is UDP, and which port, rather than that the relay is silent.
    #[tokio::test]
    async fn a_relay_whose_quic_port_is_blocked_is_named_as_such() {
        let (_process_credential_guard, state, root) = temp_state("quic-blocked").await;
        // Holds the port and reads nothing: every QUIC packet is dropped.
        let blackhole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let blocked = blackhole.local_addr().unwrap().port();
        let relay = pb_mapper_testkit::Relay::start_with_options(
            "ffi-quic-blocked",
            pb_mapper_server::RelayServerOptions {
                direct: None,
                quic: Some(QuicServerConfig {
                    listen: "127.0.0.1:0".parse().unwrap(),
                    advertise_port: blocked,
                }),
            },
        )
        .await;
        set_process_msg_header_key(Some(pb_mapper_testkit::TEST_ADMIN_KEY)).unwrap();
        {
            let mut state = state.lock().await;
            state.config.server_address = relay.addr().to_string();
            state.config.relay_transport = RelayTransport::Quic;
        }

        let detail = state.lock().await.get_server_status_detail().await.unwrap();
        assert!(!detail.server_available);
        assert_eq!(detail.quic_port, Some(blocked));
        let error = detail.error.unwrap();
        assert!(error.contains(&format!("UDP {blocked}")), "{error}");

        drop((relay, blackhole));
        let _ = set_process_msg_header_key(None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The claim is what stands in for the lock that registration no longer
    /// holds across its slow phase. Without it, two callers — the window and a
    /// terminal, say — could both finish the preflight and both insert, and the
    /// second would replace the first's handle, stopping a tunnel the first
    /// caller was told had started.
    #[tokio::test]
    async fn a_second_registration_of_the_same_key_is_refused() {
        let (_process_credential_guard, state, root) = temp_state("claim").await;

        let held = {
            let guard = state.lock().await;
            guard
                .claim_registering("home")
                .expect("first claim should succeed")
        };

        let err = register_service(
            &state,
            "home".to_string(),
            "127.0.0.1:8080".to_string(),
            "TCP".to_string(),
            false,
            false,
            DirectOptions {
                allow_direct: true,
                port_mapping: false,
            },
        )
        .await
        .expect_err("a claimed key must be refused");
        assert_eq!(
            err.code(),
            ErrorCode::AlreadyInProgress,
            "expected a claim error, got: {err}"
        );

        // A different key is unaffected: the claim is per key, not a global gate.
        {
            let guard = state.lock().await;
            guard
                .claim_registering("other")
                .expect("an unrelated key should still be claimable");
        }

        drop(held);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The claim has to survive every way a setup can end, including the ones
    /// that return early. It is released by `Drop` precisely so that no error
    /// path can forget it — this pins that.
    #[tokio::test]
    async fn a_failed_registration_releases_its_claim() {
        let (_process_credential_guard, state, root) = temp_state("release").await;
        struct RestoreProcessKey;
        impl Drop for RestoreProcessKey {
            fn drop(&mut self) {
                let _ = set_process_msg_header_key(None);
            }
        }
        set_process_msg_header_key(Some("0123456789abcdefghijklmnopqrstuv"))
            .expect("test credential");
        let _restore_process_key = RestoreProcessKey;

        // Fails in phase 2, while the claim is held.
        let first = register_service(
            &state,
            "home".to_string(),
            "not a socket address".to_string(),
            "TCP".to_string(),
            false,
            false,
            DirectOptions {
                allow_direct: true,
                port_mapping: false,
            },
        )
        .await
        .expect_err("an unparseable local address should fail");
        assert_eq!(
            first.code(),
            ErrorCode::InvalidAddress,
            "expected an address error, got: {first}"
        );

        // If the claim had leaked, this would be refused rather than reaching
        // the same address parsing it failed on before.
        let second = register_service(
            &state,
            "home".to_string(),
            "not a socket address".to_string(),
            "TCP".to_string(),
            false,
            false,
            DirectOptions {
                allow_direct: true,
                port_mapping: false,
            },
        )
        .await
        .expect_err("still an unparseable address");
        assert_ne!(
            second.code(),
            ErrorCode::AlreadyInProgress,
            "the claim leaked after a failed registration: {second}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Registering and connecting share a key space in the UI but not a claim:
    /// exposing `home` and subscribing to `home` are different operations and
    /// must not block each other.
    #[tokio::test]
    async fn registering_and_connecting_claim_separately() {
        let (_process_credential_guard, state, root) = temp_state("separate").await;

        let guard = state.lock().await;
        let _registering = guard
            .claim_registering("home")
            .expect("register claim should succeed");
        guard
            .claim_connecting("home")
            .expect("connecting the same key must not be blocked by registering it");

        drop(guard);
        let _ = std::fs::remove_dir_all(&root);
    }
}
