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

use pb_mapper::common::checksum::set_process_msg_header_key;
use pb_mapper::common::config::{get_pb_mapper_server_async, get_sockaddr_async};
use pb_mapper::common::message::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper::local::client::status::get_status;
use pb_mapper::local::client::{run_client_side_cli_with_callback, ClientStatusCallback};
use pb_mapper::local::server::{
    run_server_side_cli_with_callback, ServerTunnelOptions, StatusCallback,
};
use pb_mapper::pb_server::{run_server_with_shutdown, ServerStatusInfo};
use pb_mapper::utils::addr::each_addr;
use uni_stream::stream::got_one_socket_addr;
use uni_stream::stream::{
    ListenerProvider, StreamProvider, TcpListenerProvider, TcpStreamProvider, UdpListenerProvider,
    UdpStreamProvider,
};

use crate::error::CtlError;

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
) -> Result<bool, CtlError> {
    let addr = get_sockaddr_async(server_addr)
        .await
        .map_err(|e| CtlError::invalid_address(format!("Invalid server address: {e}")))?;

    match TcpStreamProvider::from_addr(addr).await {
        Ok(mut stream) => {
            let status_req = PbConnStatusReq::Keys;
            match get_status(&mut stream, status_req).await {
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
    if normalized.len() != 32 {
        return Err(CtlError::invalid_argument(
            "MSG_HEADER_KEY must be exactly 32 bytes (256-bit) when provided",
        ));
    }
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

impl PbMapperState {
    async fn reset_status_caches(&self) {
        {
            let mut cache = self.local_server_status_cache.write().await;
            *cache = LocalServerStatus {
                is_running: false,
                active_connections: 0,
                registered_services: 0,
                uptime_seconds: 0,
            };
        }
        {
            let mut last_update = self.local_server_status_last_update.write().await;
            *last_update = None;
        }
        self.local_server_status_refreshing
            .store(false, Ordering::Release);

        self.service_status_cache.write().await.clear();
        self.client_status_cache.write().await.clear();
        self.service_status_refreshing.write().await.clear();
        self.client_status_refreshing.write().await.clear();
    }
    pub fn new(app_directory_path: Option<String>) -> Self {
        let config_dir = Self::get_config_dir(&app_directory_path);
        tracing::info!("Using config directory: {:?}", config_dir);

        let local_server_status_cache = Arc::new(RwLock::new(LocalServerStatus {
            is_running: false,
            active_connections: 0,
            registered_services: 0,
            uptime_seconds: 0,
        }));

        let temp_state = Self {
            server_handle: None,
            server_shutdown_token: None,
            server_status_sender: None,
            server_start_time: None,
            registered_services: Arc::new(RwLock::new(HashMap::new())),
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            service_handles: HashMap::new(),
            client_handles: HashMap::new(),
            config: AppConfig::default(),
            config_dir: config_dir.clone(),
            app_directory_path: app_directory_path.clone(),
            local_server_status_cache: local_server_status_cache.clone(),
            local_server_status_last_update: Arc::new(RwLock::new(None)),
            local_server_status_refreshing: Arc::new(AtomicBool::new(false)),
            service_status_cache: Arc::new(RwLock::new(HashMap::new())),
            client_status_cache: Arc::new(RwLock::new(HashMap::new())),
            service_status_refreshing: Arc::new(RwLock::new(HashSet::new())),
            client_status_refreshing: Arc::new(RwLock::new(HashSet::new())),
            registering: Arc::new(StdMutex::new(HashSet::new())),
            connecting: Arc::new(StdMutex::new(HashSet::new())),
        };

        let config = temp_state.load_config().unwrap_or_else(|e| {
            tracing::warn!("Could not load config: {}, using defaults", e);
            AppConfig::default()
        });

        tracing::info!(
            "Loaded configuration: server_address={}, keep_alive={}, msg_header_key_set={}",
            config.server_address,
            config.keep_alive_enabled,
            !config.msg_header_key.is_empty()
        );

        let state = Self {
            server_handle: None,
            server_shutdown_token: None,
            server_status_sender: None,
            server_start_time: None,
            registered_services: Arc::new(RwLock::new(HashMap::new())),
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            service_handles: HashMap::new(),
            client_handles: HashMap::new(),
            config,
            config_dir,
            app_directory_path,
            local_server_status_cache,
            local_server_status_last_update: Arc::new(RwLock::new(None)),
            local_server_status_refreshing: Arc::new(AtomicBool::new(false)),
            service_status_cache: Arc::new(RwLock::new(HashMap::new())),
            client_status_cache: Arc::new(RwLock::new(HashMap::new())),
            service_status_refreshing: Arc::new(RwLock::new(HashSet::new())),
            client_status_refreshing: Arc::new(RwLock::new(HashSet::new())),
            registering: Arc::new(StdMutex::new(HashSet::new())),
            connecting: Arc::new(StdMutex::new(HashSet::new())),
        };
        if let Err(e) = state.apply_msg_header_key_env() {
            tracing::error!("Failed to apply MSG_HEADER_KEY during init: {}", e);
        }
        state
    }

    pub fn set_app_directory_path(&mut self, path: Option<String>) -> Result<(), CtlError> {
        self.app_directory_path = path;
        self.config_dir = Self::get_config_dir(&self.app_directory_path);

        // Reload config from new location if exists
        match self.load_config() {
            Ok(config) => self.config = config,
            Err(e) => {
                tracing::warn!("Failed to reload config after setting app dir: {}", e);
            }
        }
        self.apply_msg_header_key_env()?;

        Ok(())
    }

    fn apply_msg_header_key_env(&self) -> Result<(), CtlError> {
        let key = (!self.config.msg_header_key.is_empty()).then_some(&*self.config.msg_header_key);
        // The library validates the key's length and shape; a rejection here is
        // the stored setting being wrong, not something going wrong.
        set_process_msg_header_key(key).map_err(CtlError::invalid_argument)
    }

    #[allow(unused_variables)]
    fn get_config_dir(app_directory_path: &Option<String>) -> PathBuf {
        // An explicit path wins everywhere. Mobile is where it normally comes
        // from — Flutter hands it over, because there is no OS config dir to
        // discover — but honouring it on desktop too is what lets a test point
        // a state at a temporary directory instead of the user's real config.
        if let Some(app_dir) = app_directory_path {
            let path = PathBuf::from(app_dir).join("pb-mapper-ui");
            tracing::info!("Using caller-provided app directory: {:?}", path);
            return path;
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            tracing::warn!("No app directory provided for mobile platform, using relative path");
            PathBuf::from("pb-mapper-ui")
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            if let Some(config_dir) = dirs::config_dir() {
                config_dir.join("pb-mapper-ui")
            } else if let Some(home_dir) = dirs::home_dir() {
                home_dir.join(".config").join("pb-mapper-ui")
            } else {
                tracing::warn!("Could not determine home directory, using current directory");
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join("pb-mapper-ui-config")
            }
        }
    }

    fn get_config_file_path(&self) -> PathBuf {
        let config_dir = Self::get_config_dir(&self.app_directory_path);

        if let Err(e) = std::fs::create_dir_all(&config_dir) {
            tracing::warn!(
                "Failed to create config directory {:?}: {}, using current directory",
                config_dir,
                e
            );
            return PathBuf::from("pb_mapper_config.json");
        }

        let config_file = config_dir.join("config.json");
        tracing::info!("Using config file path: {:?}", config_file);
        config_file
    }

    pub fn load_config(&self) -> Result<AppConfig, CtlError> {
        let config_path = self.get_config_file_path();
        if config_path.exists() {
            let contents =
                fs::read_to_string(config_path).map_err(|e| CtlError::io(e.to_string()))?;
            let mut config: AppConfig =
                serde_json::from_str(&contents).map_err(|e| CtlError::io(e.to_string()))?;
            config.msg_header_key = normalize_msg_header_key(config.msg_header_key)?;
            Ok(config)
        } else {
            Ok(AppConfig::default())
        }
    }

    pub fn save_config(&self) -> Result<(), CtlError> {
        let config_path = self.get_config_file_path();
        let contents =
            serde_json::to_string_pretty(&self.config).map_err(|e| CtlError::io(e.to_string()))?;
        fs::write(config_path, contents).map_err(|e| CtlError::io(e.to_string()))?;
        Ok(())
    }

    fn get_service_config_path(&self) -> PathBuf {
        self.config_dir.join("services.json")
    }

    fn get_client_config_path(&self) -> PathBuf {
        self.config_dir.join("clients.json")
    }

    pub fn load_service_configs(&self) -> ServiceConfigStore {
        let path = self.get_service_config_path();
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| ServiceConfigStore {
                services: HashMap::new(),
            }),
            Err(_) => ServiceConfigStore {
                services: HashMap::new(),
            },
        }
    }

    pub fn save_service_configs(&self, store: &ServiceConfigStore) -> Result<(), CtlError> {
        let path = self.get_service_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CtlError::io(format!("Failed to create config dir: {e}")))?;
        }

        let content = serde_json::to_string_pretty(store)
            .map_err(|e| CtlError::io(format!("Failed to serialize config: {e}")))?;

        fs::write(&path, content)
            .map_err(|e| CtlError::io(format!("Failed to write config file: {e}")))?;
        Ok(())
    }

    pub fn save_service_config(
        &self,
        service_key: &str,
        local_address: &str,
        protocol: &str,
        enable_encryption: bool,
        enable_keep_alive: bool,
    ) -> Result<(), CtlError> {
        let mut store = self.load_service_configs();
        let now = SystemTime::now();

        let config = ServiceConfigData {
            service_key: service_key.to_string(),
            local_address: local_address.to_string(),
            protocol: protocol.to_string(),
            enable_encryption,
            enable_keep_alive,
            created_at: if store.services.contains_key(service_key) {
                store.services[service_key].created_at
            } else {
                now
            },
        };

        store.services.insert(service_key.to_string(), config);
        self.save_service_configs(&store)
    }

    pub fn delete_service_config(&self, service_key: &str) -> Result<(), CtlError> {
        let mut store = self.load_service_configs();
        store.services.remove(service_key);
        self.save_service_configs(&store)
    }

    pub fn load_client_configs(&self) -> ClientConfigStore {
        let path = self.get_client_config_path();
        match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| ClientConfigStore {
                clients: HashMap::new(),
            }),
            Err(_) => ClientConfigStore {
                clients: HashMap::new(),
            },
        }
    }

    pub fn save_client_configs(&self, store: &ClientConfigStore) -> Result<(), CtlError> {
        let path = self.get_client_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CtlError::io(format!("Failed to create config dir: {e}")))?;
        }

        let content = serde_json::to_string_pretty(store)
            .map_err(|e| CtlError::io(format!("Failed to serialize client config: {e}")))?;

        fs::write(&path, content)
            .map_err(|e| CtlError::io(format!("Failed to write client config file: {e}")))?;
        Ok(())
    }

    pub fn save_client_config(
        &self,
        service_key: &str,
        local_address: &str,
        protocol: &str,
        enable_keep_alive: bool,
    ) -> Result<(), CtlError> {
        let mut store = self.load_client_configs();
        let now = SystemTime::now();

        let config = ClientConfigData {
            service_key: service_key.to_string(),
            local_address: local_address.to_string(),
            protocol: protocol.to_string(),
            enable_keep_alive,
            created_at: if store.clients.contains_key(service_key) {
                store.clients[service_key].created_at
            } else {
                now
            },
        };

        store.clients.insert(service_key.to_string(), config);
        self.save_client_configs(&store)
    }

    pub fn delete_client_config(&self, service_key: &str) -> Result<(), CtlError> {
        let mut store = self.load_client_configs();
        store.clients.remove(service_key);
        self.save_client_configs(&store)
    }

    pub async fn start_server(
        &mut self,
        port: u16,
        enable_keep_alive: bool,
    ) -> Result<(), CtlError> {
        if self.server_handle.is_some() {
            return Err(CtlError::already_exists("Server is already running"));
        }

        let ip_addr = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
        let bind_addr = std::net::SocketAddr::new(ip_addr, port);

        // Preflight bind to surface "port already in use" errors before spawning.
        let listener = TcpListener::bind(bind_addr).await.map_err(|e| {
            CtlError::address_in_use(format!("Failed to bind server on {bind_addr}: {e}"))
        })?;
        drop(listener);

        tracing::info!("Starting pb-mapper server on {}:{}", ip_addr, port);

        let shutdown_token = CancellationToken::new();
        let shutdown_token_clone = shutdown_token.clone();

        let (status_sender, status_receiver) = tokio::sync::mpsc::unbounded_channel();

        let handle = tokio::spawn(async move {
            if let Err(e) = run_server_with_shutdown(
                (ip_addr, port),
                shutdown_token_clone,
                Some(status_receiver),
                enable_keep_alive,
            )
            .await
            {
                tracing::error!("pb-mapper server stopped with error: {e}");
            }
        });

        self.server_handle = Some(handle);
        self.server_shutdown_token = Some(shutdown_token);
        self.server_status_sender = Some(status_sender);
        self.server_start_time = Some(SystemTime::now());

        {
            let mut cache = self.local_server_status_cache.write().await;
            *cache = LocalServerStatus {
                is_running: true,
                active_connections: 0,
                registered_services: 0,
                uptime_seconds: 0,
            };
        }
        {
            let mut last_update = self.local_server_status_last_update.write().await;
            *last_update = Some(Instant::now());
        }

        tracing::info!("pb-mapper server started successfully");
        Ok(())
    }

    pub async fn stop_server(&mut self) -> Result<(), CtlError> {
        if let (Some(handle), Some(shutdown_token)) =
            (self.server_handle.take(), self.server_shutdown_token.take())
        {
            self.server_status_sender = None;

            shutdown_token.cancel();

            let shutdown_timeout = tokio::time::Duration::from_secs(5);

            match tokio::time::timeout(shutdown_timeout, handle).await {
                Ok(_) => {
                    tracing::info!("Server shutdown gracefully");
                }
                Err(_) => {
                    tracing::warn!("Server shutdown timed out, may not have closed gracefully");
                }
            }

            self.server_start_time = None;

            {
                let mut cache = self.local_server_status_cache.write().await;
                *cache = LocalServerStatus {
                    is_running: false,
                    active_connections: 0,
                    registered_services: 0,
                    uptime_seconds: 0,
                };
            }
            {
                let mut last_update = self.local_server_status_last_update.write().await;
                *last_update = Some(Instant::now());
            }

            for (_, handle) in self.service_handles.drain() {
                handle.abort();
            }

            for (_, handle) in self.client_handles.drain() {
                handle.abort();
            }

            self.registered_services.write().await.clear();
            self.active_connections.write().await.clear();

            tracing::info!("pb-mapper server stopped, all services and connections terminated");
            Ok(())
        } else {
            Err(CtlError::not_found("Server is not running"))
        }
    }

    async fn finish_register(&mut self, commit: RegisterCommit) -> Result<(), CtlError> {
        let RegisterCommit {
            service_key,
            local_address,
            protocol,
            enable_encryption,
            enable_keep_alive,
            local_sock_addr,
            remote_sock_addr,
        } = commit;

        if let Some(previous) = self.service_handles.remove(&service_key) {
            tracing::warn!(
                "Service '{service_key}' is already registered, replacing existing handle"
            );
            // Dropping a `JoinHandle` does not stop the task. Without this the
            // replaced tunnel kept running and retrying, with nothing left
            // holding a handle able to abort it.
            previous.abort();
        }

        tracing::info!(
            "Registering service '{}' with protocol {}, local address {}, server address {}",
            service_key,
            protocol,
            local_address,
            self.config.server_address
        );

        self.save_service_config(
            &service_key,
            &local_address,
            &protocol,
            enable_encryption,
            enable_keep_alive,
        )
        .map_err(|e| CtlError::io(format!("Failed to save service configuration: {e}")))?;

        let key_clone = service_key.clone();
        let service_key_for_status = service_key.clone();

        let callback: StatusCallback = Box::new(move |status: &str| {
            tracing::info!(
                "Service {} status update: {}",
                service_key_for_status,
                status
            );
        });

        let handle = if protocol.to_uppercase() == "TCP" {
            tokio::spawn(async move {
                let _ = run_server_side_cli_with_callback::<TcpStreamProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    ServerTunnelOptions {
                        need_codec: enable_encryption,
                        is_datagram: false,
                        keep_alive: enable_keep_alive,
                    },
                    Some(callback),
                )
                .await;
            })
        } else {
            tokio::spawn(async move {
                let _ = run_server_side_cli_with_callback::<UdpStreamProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    ServerTunnelOptions {
                        need_codec: enable_encryption,
                        is_datagram: true,
                        keep_alive: enable_keep_alive,
                    },
                    Some(callback),
                )
                .await;
            })
        };

        self.service_handles.insert(service_key.clone(), handle);

        {
            let mut cache = self.service_status_cache.write().await;
            cache.insert(
                service_key.clone(),
                StatusCacheEntry {
                    status: "retrying".to_string(),
                    message: "Connecting to pb-mapper server...".to_string(),
                    updated_at: Instant::now(),
                },
            );
        }
        self.schedule_service_status_refresh(&service_key).await;

        let service_info = ServiceInfo {
            service_key: service_key.clone(),
            protocol,
            local_address,
            status: "Registering".to_string(),
        };

        self.registered_services
            .write()
            .await
            .insert(service_key.clone(), service_info);

        tracing::info!("Service '{}' registration initiated", service_key);
        Ok(())
    }

    pub async fn unregister_service(&mut self, service_key: String) -> Result<(), CtlError> {
        if let Some(handle) = self.service_handles.remove(&service_key) {
            handle.abort();
        }

        if self
            .registered_services
            .write()
            .await
            .remove(&service_key)
            .is_some()
        {
            tracing::info!("Service '{}' unregistered successfully", service_key);
            Ok(())
        } else {
            Err(CtlError::not_found(format!(
                "Service '{service_key}' is not registered"
            )))
        }
    }

    pub async fn delete_service_config_and_stop(
        &mut self,
        service_key: String,
    ) -> Result<(), CtlError> {
        if let Some(handle) = self.service_handles.remove(&service_key) {
            handle.abort();
        }

        self.registered_services.write().await.remove(&service_key);

        self.delete_service_config(&service_key)
    }

    async fn finish_connect(&mut self, commit: ConnectCommit) -> Result<(), CtlError> {
        let ConnectCommit {
            service_key,
            local_address,
            protocol,
            enable_keep_alive,
            local_sock_addr,
            remote_sock_addr,
        } = commit;

        if let Some(previous) = self.client_handles.remove(&service_key) {
            tracing::warn!(
                "Client for service '{service_key}' is already connected, replacing handle"
            );
            // As in `finish_register`: dropping the handle leaves the old
            // client's retry loop running with nothing able to stop it.
            previous.abort();
        }

        let protocol_upper = protocol.to_uppercase();

        tracing::info!(
            "Connecting to service '{}' with protocol {}, local address {}, server address {}",
            service_key,
            protocol,
            local_address,
            self.config.server_address
        );

        let key_clone = service_key.clone();

        let status_callback: ClientStatusCallback = {
            let service_key_for_callback = service_key.clone();
            Box::new(move |status: &str| {
                tracing::info!("Client {} status: {}", service_key_for_callback, status);
            })
        };

        let handle = if protocol_upper == "TCP" {
            tokio::spawn(async move {
                run_client_side_cli_with_callback::<TcpListenerProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    enable_keep_alive,
                    Some(status_callback),
                )
                .await;
            })
        } else {
            tokio::spawn(async move {
                run_client_side_cli_with_callback::<UdpListenerProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    enable_keep_alive,
                    Some(status_callback),
                )
                .await;
            })
        };

        self.client_handles.insert(service_key.clone(), handle);

        {
            let mut cache = self.client_status_cache.write().await;
            cache.insert(
                service_key.clone(),
                StatusCacheEntry {
                    status: "retrying".to_string(),
                    message: "Connecting to pb-mapper server...".to_string(),
                    updated_at: Instant::now(),
                },
            );
        }
        self.schedule_client_status_refresh(&service_key).await;

        let connection_info = ConnectionInfo {
            service_key: service_key.clone(),
            client_id: format!("client-{service_key}"),
            status: "Connected".to_string(),
        };

        self.active_connections
            .write()
            .await
            .insert(service_key.clone(), connection_info);

        tracing::info!("Connected to service '{}' successfully", service_key);
        Ok(())
    }

    /// Claims a service key for a registration. See [`KeyClaim`].
    fn claim_registering(&self, service_key: &str) -> Result<KeyClaim, CtlError> {
        claim_key(&self.registering, service_key, "being registered")
    }

    /// Claims a service key for a client connection. See [`KeyClaim`].
    fn claim_connecting(&self, service_key: &str) -> Result<KeyClaim, CtlError> {
        claim_key(&self.connecting, service_key, "being connected")
    }

    pub async fn disconnect_service(&mut self, service_key: String) -> Result<(), CtlError> {
        // Aborting the task is the part that matters: it is what stops the
        // retry loop still dialling in the background.
        let aborted = match self.client_handles.remove(&service_key) {
            Some(handle) => {
                handle.abort();
                true
            }
            None => false,
        };

        let was_listed = self
            .active_connections
            .write()
            .await
            .remove(&service_key)
            .is_some();

        // Reported failure only when there was nothing to stop. It used to key
        // off the bookkeeping map alone, so a client whose task had been
        // aborted could still be reported as "not connected" — an error for an
        // operation that had in fact just done its job.
        if aborted || was_listed {
            tracing::info!("Disconnected from service '{}'", service_key);
            Ok(())
        } else {
            Err(CtlError::not_found(format!(
                "Service '{service_key}' is not connected"
            )))
        }
    }

    pub async fn delete_client_config_and_stop(
        &mut self,
        service_key: String,
    ) -> Result<(), CtlError> {
        if let Some(handle) = self.client_handles.remove(&service_key) {
            handle.abort();
        }

        self.active_connections.write().await.remove(&service_key);

        self.delete_client_config(&service_key)
    }

    pub async fn get_config_status(&self) -> AppConfig {
        self.config.clone()
    }

    pub async fn update_config(
        &mut self,
        server_address: String,
        keep_alive: bool,
        msg_header_key: String,
    ) -> Result<(), CtlError> {
        let msg_header_key = normalize_msg_header_key(msg_header_key)?;
        self.config.server_address = server_address;
        self.config.keep_alive_enabled = keep_alive;
        self.config.msg_header_key = msg_header_key;
        self.apply_msg_header_key_env()?;
        self.save_config()?;
        self.reset_status_caches().await;
        Ok(())
    }

    pub async fn get_service_configs(&self) -> Vec<ServiceConfigInfo> {
        let store = self.load_service_configs();
        let mut services = Vec::new();

        let mut sorted_configs: Vec<_> = store.services.values().collect();
        sorted_configs.sort_by_key(|config| config.created_at);

        for config in sorted_configs {
            let (status, message) = self.calculate_service_status(&config.service_key).await;

            services.push(ServiceConfigInfo {
                service_key: config.service_key.clone(),
                local_address: config.local_address.clone(),
                protocol: config.protocol.clone(),
                enable_encryption: config.enable_encryption,
                enable_keep_alive: config.enable_keep_alive,
                status,
                status_message: message,
                created_at_ms: config
                    .created_at
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                updated_at_ms: SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            });
        }

        services
    }

    pub async fn get_service_status(&self, service_key: String) -> ServiceStatusResponse {
        let (status, message) = self.calculate_service_status(&service_key).await;
        ServiceStatusResponse {
            service_key,
            status,
            message,
        }
    }

    pub async fn get_client_configs(&self) -> Vec<ClientConfigInfo> {
        let store = self.load_client_configs();
        let mut client_infos = Vec::new();

        for (service_key, config) in store.clients.iter() {
            let (status, status_message) = self.calculate_client_status(service_key).await;

            client_infos.push(ClientConfigInfo {
                service_key: config.service_key.clone(),
                local_address: config.local_address.clone(),
                protocol: config.protocol.clone(),
                enable_keep_alive: config.enable_keep_alive,
                status,
                status_message,
                created_at_ms: config
                    .created_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                updated_at_ms: config
                    .created_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            });
        }

        client_infos.sort_by_key(|info| info.created_at_ms);
        client_infos
    }

    pub async fn get_client_status(&self, service_key: String) -> ClientStatusResponse {
        let (status, message) = self.calculate_client_status(&service_key).await;
        ClientStatusResponse {
            service_key,
            status,
            message,
        }
    }

    pub async fn get_local_server_status(&self) -> LocalServerStatus {
        let is_running = self.server_handle.is_some();
        if !is_running {
            let status = LocalServerStatus {
                is_running: false,
                active_connections: 0,
                registered_services: 0,
                uptime_seconds: 0,
            };
            {
                let mut cache = self.local_server_status_cache.write().await;
                *cache = status.clone();
            }
            {
                let mut last_update = self.local_server_status_last_update.write().await;
                *last_update = Some(Instant::now());
            }
            return status;
        }

        let should_refresh = {
            let last_update = self.local_server_status_last_update.read().await;
            cache_is_stale(*last_update, STATUS_CACHE_TTL)
        };

        if should_refresh {
            self.schedule_local_server_status_refresh();
        }

        let cache = self.local_server_status_cache.read().await;
        cache.clone()
    }

    fn schedule_local_server_status_refresh(&self) {
        if self
            .local_server_status_refreshing
            .swap(true, Ordering::AcqRel)
        {
            return;
        }

        let sender = self.server_status_sender.clone();
        let cache = self.local_server_status_cache.clone();
        let last_update = self.local_server_status_last_update.clone();
        let refreshing = self.local_server_status_refreshing.clone();
        let start_time = self.server_start_time;

        tokio::spawn(async move {
            let mut status = LocalServerStatus {
                is_running: true,
                active_connections: 0,
                registered_services: 0,
                uptime_seconds: start_time
                    .and_then(|ts| SystemTime::now().duration_since(ts).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };

            if let Some(sender) = sender {
                let (response_sender, response_receiver) = tokio::sync::oneshot::channel();
                if sender.send(response_sender).is_ok() {
                    if let Ok(Ok(info)) =
                        tokio::time::timeout(Duration::from_millis(200), response_receiver).await
                    {
                        status.active_connections = info.active_connections;
                        status.registered_services = info.registered_services;
                        status.uptime_seconds = info.uptime_seconds;
                    }
                }
            }

            {
                let mut cache = cache.write().await;
                *cache = status;
            }
            {
                let mut last_update = last_update.write().await;
                *last_update = Some(Instant::now());
            }
            refreshing.store(false, Ordering::Release);
        });
    }

    pub async fn get_server_status_detail(&self) -> Result<ServerStatusDetail, CtlError> {
        self.force_refresh_server_status().await
    }

    /// The connections the server holds for one key, from the protocol's own
    /// structured query rather than the Debug dump in `server_map`.
    pub async fn get_service_conns(
        &self,
        service_key: String,
    ) -> Result<Vec<ServiceConnInfo>, CtlError> {
        let server_addr = self.config.server_address.clone();
        match tokio::time::timeout(
            FORCE_REFRESH_TIMEOUT,
            get_service_conns_with_addr(&server_addr, &service_key),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(CtlError::timeout(format!(
                "Timed out asking {server_addr} about {service_key}"
            ))),
        }
    }

    /// Perform a blocking status refresh — waits for the actual network result
    /// instead of returning stale cache.
    pub async fn force_refresh_server_status(&self) -> Result<ServerStatusDetail, CtlError> {
        let server_addr = self.config.server_address.clone();

        let detail = match tokio::time::timeout(
            FORCE_REFRESH_TIMEOUT,
            fetch_real_status_with_addr(&server_addr),
        )
        .await
        {
            Ok(Ok((services, remote_id_data))) => ServerStatusDetail {
                server_available: true,
                registered_services: services,
                server_map: remote_id_data.server_map,
                active_connections: remote_id_data.active,
                idle_connections: remote_id_data.idle,
            },
            Ok(Err(e)) => {
                tracing::warn!("Force refresh failed: {}", e);
                ServerStatusDetail {
                    server_available: false,
                    registered_services: Vec::new(),
                    server_map: String::new(),
                    active_connections: String::new(),
                    idle_connections: String::new(),
                }
            }
            Err(_) => {
                tracing::warn!("Force refresh timed out after {:?}", FORCE_REFRESH_TIMEOUT);
                ServerStatusDetail {
                    server_available: false,
                    registered_services: Vec::new(),
                    server_map: String::new(),
                    active_connections: String::new(),
                    idle_connections: String::new(),
                }
            }
        };

        Ok(detail)
    }

    // Cache service status to avoid blocking UI with network checks on every paint.
    async fn get_cached_service_status(&self, service_key: &str) -> (String, String) {
        if let Some(handle) = self.service_handles.get(service_key) {
            if handle.is_finished() {
                return (
                    "failed".to_string(),
                    "Service connection terminated".to_string(),
                );
            }

            let cached = {
                let cache = self.service_status_cache.read().await;
                cache.get(service_key).cloned()
            };

            let should_refresh = cached
                .as_ref()
                .map(|entry| entry.updated_at.elapsed() > STATUS_CACHE_TTL)
                .unwrap_or(true);

            if should_refresh {
                self.schedule_service_status_refresh(service_key).await;
            }

            if let Some(entry) = cached {
                return (entry.status, entry.message);
            }

            return (
                "retrying".to_string(),
                "Checking service status...".to_string(),
            );
        }

        (
            "stopped".to_string(),
            "Service is not registered".to_string(),
        )
    }

    // Cache client status to avoid blocking UI with network checks on every paint.
    async fn get_cached_client_status(&self, service_key: &str) -> (String, String) {
        if let Some(handle) = self.client_handles.get(service_key) {
            if handle.is_finished() {
                return (
                    "failed".to_string(),
                    "Client connection terminated".to_string(),
                );
            }

            let cached = {
                let cache = self.client_status_cache.read().await;
                cache.get(service_key).cloned()
            };

            let should_refresh = cached
                .as_ref()
                .map(|entry| entry.updated_at.elapsed() > STATUS_CACHE_TTL)
                .unwrap_or(true);

            if should_refresh {
                self.schedule_client_status_refresh(service_key).await;
            }

            if let Some(entry) = cached {
                return (entry.status, entry.message);
            }

            return (
                "retrying".to_string(),
                "Checking client status...".to_string(),
            );
        }

        ("stopped".to_string(), "Client is not connected".to_string())
    }

    async fn schedule_service_status_refresh(&self, service_key: &str) {
        {
            let mut refreshing = self.service_status_refreshing.write().await;
            if refreshing.contains(service_key) {
                return;
            }
            refreshing.insert(service_key.to_string());
        }

        let server_addr = self.config.server_address.clone();
        let cache = self.service_status_cache.clone();
        let refreshing = self.service_status_refreshing.clone();
        let key = service_key.to_string();

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                STATUS_REFRESH_TIMEOUT,
                check_service_with_get_status(&server_addr, &key),
            )
            .await;

            let (status, message) = match result {
                Ok(Ok(true)) => (
                    "running".to_string(),
                    "Service is running normally".to_string(),
                ),
                Ok(Ok(false)) => (
                    "retrying".to_string(),
                    "Service is in retry connection loop".to_string(),
                ),
                Ok(Err(_)) | Err(_) => (
                    "failed".to_string(),
                    "Cannot connect to pb-server".to_string(),
                ),
            };

            {
                let mut cache = cache.write().await;
                cache.insert(
                    key.clone(),
                    StatusCacheEntry {
                        status,
                        message,
                        updated_at: Instant::now(),
                    },
                );
            }

            let mut refreshing = refreshing.write().await;
            refreshing.remove(&key);
        });
    }

    async fn schedule_client_status_refresh(&self, service_key: &str) {
        {
            let mut refreshing = self.client_status_refreshing.write().await;
            if refreshing.contains(service_key) {
                return;
            }
            refreshing.insert(service_key.to_string());
        }

        let server_addr = self.config.server_address.clone();
        let cache = self.client_status_cache.clone();
        let refreshing = self.client_status_refreshing.clone();
        let key = service_key.to_string();

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                STATUS_REFRESH_TIMEOUT,
                check_service_with_get_status(&server_addr, &key),
            )
            .await;

            let (status, message) = match result {
                Ok(Ok(true)) => (
                    "running".to_string(),
                    "Client is connected normally".to_string(),
                ),
                Ok(Ok(false)) => (
                    "retrying".to_string(),
                    "Client is in retry connection loop".to_string(),
                ),
                Ok(Err(_)) | Err(_) => (
                    "failed".to_string(),
                    "Cannot connect to pb-server".to_string(),
                ),
            };

            {
                let mut cache = cache.write().await;
                cache.insert(
                    key.clone(),
                    StatusCacheEntry {
                        status,
                        message,
                        updated_at: Instant::now(),
                    },
                );
            }

            let mut refreshing = refreshing.write().await;
            refreshing.remove(&key);
        });
    }

    async fn calculate_service_status(&self, service_key: &str) -> (String, String) {
        self.get_cached_service_status(service_key).await
    }

    async fn calculate_client_status(&self, service_key: &str) -> (String, String) {
        self.get_cached_client_status(service_key).await
    }
}

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
