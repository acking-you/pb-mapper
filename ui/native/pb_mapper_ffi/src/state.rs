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
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use pb_mapper::common::auth::{AuthConfig, AuthRuntime};
use pb_mapper::common::checksum::{
    get_process_credential, parse_credential, set_process_msg_header_key, Credential,
};
use pb_mapper::common::config::{get_pb_mapper_server_async, get_sockaddr_async};
use pb_mapper::common::message::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper::local::client::status::{get_status, get_status_with_credential};
use pb_mapper::local::client::{run_client_side_cli_with_pinned_credential, ClientStatusCallback};
use pb_mapper::local::server::{
    run_server_side_cli_with_pinned_credential, ServerTunnelOptions, StatusCallback,
};
use pb_mapper::pb_server::{run_server_on_listener, ServerStatusInfo};
use pb_mapper::utils::addr::each_addr;
use uni_stream::stream::got_one_socket_addr;
use uni_stream::stream::{
    ListenerProvider, StreamProvider, TcpListenerProvider, TcpStreamProvider, UdpListenerProvider,
    UdpStreamProvider,
};

use crate::ctl::Origin;
use crate::error::CtlError;
use crate::events;

const STATUS_CACHE_TTL: Duration = Duration::from_secs(2);
const STATUS_REFRESH_TIMEOUT: Duration = Duration::from_millis(3000);
const FORCE_REFRESH_TIMEOUT: Duration = Duration::from_millis(5000);

#[derive(Clone)]
struct StatusCacheEntry {
    status: String,
    message: String,
    updated_at: Instant,
}

async fn check_service_with_get_status(
    server_addr: &str,
    service_key: &str,
    credential: Option<Credential>,
) -> Result<bool, CtlError> {
    let addr = get_sockaddr_async(server_addr)
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid server address: {e}")))?;

    match TcpStreamProvider::from_addr(addr).await {
        Ok(mut stream) => {
            let status_req = PbConnStatusReq::Keys;
            let status = match credential {
                Some(credential) => {
                    get_status_with_credential(&mut stream, status_req, None, &credential).await
                }
                None => get_status(&mut stream, status_req).await,
            };
            match status {
                Ok(status_resp) => match status_resp {
                    PbConnStatusResp::Keys(keys) => {
                        if keys.contains(&service_key.to_string()) {
                            Ok(true)
                        } else {
                            Err(CtlError::not_found("Service not found in server"))
                        }
                    }
                    _ => Ok(true),
                },
                Err(_) => Ok(false),
            }
        }
        Err(_) => Err(CtlError::server_unreachable("Cannot connect to server")),
    }
}

async fn fetch_real_status_with_addr(
    server_addr: &str,
) -> Result<(Vec<String>, RemoteIdData), CtlError> {
    let (keys_result, remote_id_result) = tokio::join!(
        get_server_keys_with_addr(server_addr),
        get_remote_id_data_with_addr(server_addr),
    );

    let services =
        keys_result.map_err(|e| CtlError::internal(format!("Failed to get server keys: {e}")))?;
    let remote_id_data = remote_id_result.unwrap_or_else(|e| {
        tracing::warn!("Failed to get remote-id data: {}, using empty data", e);
        RemoteIdData {
            server_map: String::new(),
            active: String::new(),
            idle: String::new(),
        }
    });

    Ok((services, remote_id_data))
}

async fn get_server_keys_with_addr(server_addr: &str) -> Result<Vec<String>, CtlError> {
    use tokio::net::TcpStream;

    let socket_addr = got_one_socket_addr(server_addr).await.map_err(|e| {
        CtlError::invalid_address(format!("Invalid server address {server_addr}: {e}"))
    })?;

    let mut stream = each_addr(socket_addr, TcpStream::connect)
        .await
        .map_err(|e| CtlError::server_unreachable(format!("Failed to connect to server: {e}")))?;

    let status_resp = get_status(&mut stream, PbConnStatusReq::Keys)
        .await
        .map_err(|e| CtlError::internal(format!("Failed to get status: {e}")))?;

    match status_resp {
        PbConnStatusResp::Keys(keys) => Ok(keys),
        _ => Err(CtlError::protocol(
            "Unexpected response type for Keys request",
        )),
    }
}

/// The connections the server is holding for one service key.
///
/// The UI used to render `server_map`, which is `format!("{map:?}")` on the
/// server side — a Debug dump, and so not something anything should parse. The
/// same data is already available structured through `PbConnStatusReq::Service`,
/// which the CLI has always used and the FFI never exposed. No wire change:
/// servers already answer this.
async fn get_service_conns_with_addr(
    server_addr: &str,
    service_key: &str,
) -> Result<Vec<ServiceConnInfo>, CtlError> {
    let socket_addr = got_one_socket_addr(server_addr).await.map_err(|e| {
        CtlError::invalid_address(format!("Invalid server address {server_addr}: {e}"))
    })?;

    let mut stream = each_addr(socket_addr, TcpStream::connect)
        .await
        .map_err(|e| CtlError::server_unreachable(format!("Failed to connect to server: {e}")))?;

    let status_resp = get_status(
        &mut stream,
        PbConnStatusReq::Service {
            key: service_key.to_string(),
        },
    )
    .await
    .map_err(|e| CtlError::internal(format!("Failed to get status: {e}")))?;

    match status_resp {
        PbConnStatusResp::Service { connections, .. } => Ok(connections
            .into_iter()
            .map(|c| ServiceConnInfo {
                conn_id: c.conn_id,
                generation: c.generation,
                protocol_version: c.protocol_version,
                healthy: c.healthy,
                last_rx_age_ms: c.last_rx_age_ms,
            })
            .collect()),
        _ => Err(CtlError::protocol(
            "Unexpected response type for Service request",
        )),
    }
}

async fn get_remote_id_data_with_addr(server_addr: &str) -> Result<RemoteIdData, CtlError> {
    use tokio::net::TcpStream;

    let socket_addr = got_one_socket_addr(server_addr).await.map_err(|e| {
        CtlError::invalid_address(format!("Invalid server address {server_addr}: {e}"))
    })?;

    let mut stream = each_addr(socket_addr, TcpStream::connect)
        .await
        .map_err(|e| CtlError::server_unreachable(format!("Failed to connect to server: {e}")))?;

    let status_resp = get_status(&mut stream, PbConnStatusReq::RemoteId)
        .await
        .map_err(|e| CtlError::internal(format!("Failed to get status: {e}")))?;

    match status_resp {
        PbConnStatusResp::RemoteId {
            server_map,
            active,
            idle,
        } => Ok(RemoteIdData {
            server_map,
            active,
            idle,
        }),
        _ => Err(CtlError::protocol(
            "Unexpected response type for RemoteId request",
        )),
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
}

#[derive(Serialize, Deserialize)]
pub struct ClientConfigStore {
    pub clients: HashMap<String, ClientConfigData>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct AppConfig {
    pub server_address: String,
    pub keep_alive_enabled: bool,
    pub msg_header_key: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_address: "localhost:7666".to_string(),
            keep_alive_enabled: true,
            msg_header_key: String::new(),
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
    pub status: String,
    pub status_message: String,
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
    pub status: String,
    pub status_message: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LocalServerStatus {
    pub is_running: bool,
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

/// Helper struct to hold RemoteId response data
struct RemoteIdData {
    server_map: String,
    active: String,
    idle: String,
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
/// The set is behind a `std::sync::Mutex` rather than tokio's so that `Drop` can
/// release it; it is only ever held for a set insert or remove.
struct KeyClaim {
    key: String,
    claims: Arc<StdMutex<HashSet<String>>>,
}

impl Drop for KeyClaim {
    fn drop(&mut self) {
        if let Ok(mut claims) = self.claims.lock() {
            claims.remove(&self.key);
        }
    }
}

/// Claims `key`, or reports that someone else is already setting it up.
fn claim_key(
    claims: &Arc<StdMutex<HashSet<String>>>,
    key: &str,
    what: &str,
) -> Result<KeyClaim, CtlError> {
    let mut guard = claims
        .lock()
        .map_err(|_| CtlError::internal(format!("{what} state for '{key}' is poisoned")))?;
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

/// Everything [`PbMapperState::finish_register`] needs once the slow work is done.
struct RegisterCommit {
    service_key: String,
    local_address: String,
    protocol: String,
    enable_encryption: bool,
    enable_keep_alive: bool,
    local_sock_addr: SocketAddr,
    remote_sock_addr: SocketAddr,
}

/// Everything [`PbMapperState::finish_connect`] needs once the slow work is done.
struct ConnectCommit {
    service_key: String,
    local_address: String,
    protocol: String,
    enable_keep_alive: bool,
    local_sock_addr: SocketAddr,
    remote_sock_addr: SocketAddr,
}

pub struct PbMapperState {
    server_handle: Option<JoinHandle<()>>,
    server_shutdown_token: Option<CancellationToken>,
    server_status_sender:
        Option<tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<ServerStatusInfo>>>,
    server_start_time: Option<SystemTime>,
    registered_services: Arc<RwLock<HashMap<String, ServiceInfo>>>,
    active_connections: Arc<RwLock<HashMap<String, ConnectionInfo>>>,
    service_handles: HashMap<String, JoinHandle<()>>,
    client_handles: HashMap<String, JoinHandle<()>>,
    service_credentials: HashMap<String, Credential>,
    client_credentials: HashMap<String, Credential>,
    config: AppConfig,
    config_dir: PathBuf,
    app_directory_path: Option<String>,
    local_server_status_cache: Arc<RwLock<LocalServerStatus>>,
    local_server_status_last_update: Arc<RwLock<Option<Instant>>>,
    local_server_status_refreshing: Arc<AtomicBool>,
    service_status_cache: Arc<RwLock<HashMap<String, StatusCacheEntry>>>,
    client_status_cache: Arc<RwLock<HashMap<String, StatusCacheEntry>>>,
    service_status_refreshing: Arc<RwLock<HashSet<String>>>,
    client_status_refreshing: Arc<RwLock<HashSet<String>>>,
    /// Keys currently being set up. See [`KeyClaim`]. Separate sets because a
    /// key can legitimately be registered and connected to at the same time.
    registering: Arc<StdMutex<HashSet<String>>>,
    connecting: Arc<StdMutex<HashSet<String>>>,
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
) -> Result<(), CtlError> {
    // 1. Claim the key and take what the slow work needs. Microseconds.
    //    `_claim` is held to the end of the function on purpose: dropping it
    //    early would release the key while the setup is still running.
    let (_claim, server_address) = {
        let state = state.lock().await;
        let claim = state.claim_registering(&service_key)?;
        (claim, state.config.server_address.clone())
    };

    // 2. The slow parts, with the lock released.
    let local_sock_addr = get_sockaddr_async(&local_address)
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid local address: {e}")))?;
    let remote_sock_addr = get_pb_mapper_server_async(Some(&server_address))
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid server address: {e}")))?;
    // Preflight remote server connectivity to surface errors early.
    TcpStream::connect(remote_sock_addr).await.map_err(|e| {
        CtlError::server_unreachable(format!("Failed to connect to server {server_address}: {e}"))
    })?;

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
            local_sock_addr,
            remote_sock_addr,
        })
        .await
}

/// Connects to a registered service. Phased exactly like [`register_service`];
/// the slow step here is the local bind that proves the port is free.
pub async fn connect_service(
    state: &Arc<Mutex<PbMapperState>>,
    service_key: String,
    local_address: String,
    protocol: String,
    enable_keep_alive: bool,
) -> Result<(), CtlError> {
    let (_claim, server_address) = {
        let state = state.lock().await;
        let claim = state.claim_connecting(&service_key)?;
        (claim, state.config.server_address.clone())
    };

    let local_sock_addr = get_sockaddr_async(&local_address)
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid local address: {e}")))?;
    let remote_sock_addr = get_pb_mapper_server_async(Some(&server_address))
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid server address: {e}")))?;

    // Preflight local bind to detect "port already in use" before starting client.
    if protocol.to_uppercase() == "TCP" {
        let listener = TcpListenerProvider::bind(local_sock_addr)
            .await
            .map_err(|e| {
                CtlError::address_in_use(format!(
                    "Failed to bind local address {local_address}: {e}"
                ))
            })?;
        drop(listener);
    } else {
        let listener = UdpListenerProvider::bind(local_sock_addr)
            .await
            .map_err(|e| {
                CtlError::address_in_use(format!(
                    "Failed to bind local address {local_address}: {e}"
                ))
            })?;
        drop(listener);
    }

    state
        .lock()
        .await
        .finish_connect(ConnectCommit {
            service_key,
            local_address,
            protocol,
            enable_keep_alive,
            local_sock_addr,
            remote_sock_addr,
        })
        .await
}

#[cfg(test)]
// The crate denies these so a panic never reaches a Flutter caller through the
// FFI boundary. In a test a panic *is* the failure report.
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    /// A state rooted in a temporary directory, so a test never reads or writes
    /// the real user config.
    fn temp_state(name: &str) -> (Arc<Mutex<PbMapperState>>, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "pb-mapper-ffi-test-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let state = PbMapperState::new(Some(root.to_string_lossy().into_owned()));
        (Arc::new(Mutex::new(state)), root)
    }

    #[tokio::test]
    async fn ui_server_uses_its_writable_config_directory_and_reports_readiness() {
        let (state, root) = temp_state("server-auth-path");
        let auth_dir = {
            let mut state = state.lock().await;
            let auth_dir = state.config_dir.join("auth");
            state
                .start_server(0, false)
                .await
                .expect("UI server should bind and initialize authentication");
            assert!(state.server_handle.is_some());
            assert!(state.get_local_server_status().await.is_running);
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

    /// The claim is what stands in for the lock that registration no longer
    /// holds across its slow phase. Without it, two callers — the window and a
    /// terminal, say — could both finish the preflight and both insert, and the
    /// second would overwrite the first's `JoinHandle`, leaving a tunnel with
    /// nothing able to abort it.
    #[tokio::test]
    async fn a_second_registration_of_the_same_key_is_refused() {
        let (state, root) = temp_state("claim");

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
        let (state, root) = temp_state("release");

        // Fails in phase 2, while the claim is held.
        let first = register_service(
            &state,
            "home".to_string(),
            "not a socket address".to_string(),
            "TCP".to_string(),
            false,
            false,
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
        let (state, root) = temp_state("separate");

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
