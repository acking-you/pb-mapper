use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use pb_mapper::common::config::{
    get_pb_mapper_server_async, get_sockaddr_async, PB_MAPPER_KEEP_ALIVE,
};
use pb_mapper::common::message::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper::local::client::status::get_status;
use pb_mapper::local::client::{run_client_side_cli_with_callback, ClientStatusCallback};
use pb_mapper::local::server::{run_server_side_cli_with_callback, StatusCallback};
use pb_mapper::pb_server::{run_server_with_shutdown, ServerStatusInfo};
use pb_mapper::utils::addr::each_addr;
use uni_stream::stream::got_one_socket_addr;
use uni_stream::stream::{
    ListenerProvider, StreamProvider, TcpListenerProvider, TcpStreamProvider, UdpListenerProvider,
    UdpStreamProvider,
};

const STATUS_CACHE_TTL: Duration = Duration::from_secs(2);
const SERVER_STATUS_TTL: Duration = Duration::from_secs(2);
const STATUS_REFRESH_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Clone)]
struct StatusCacheEntry {
    status: String,
    message: String,
    updated_at: Instant,
}

async fn check_service_with_get_status(
    server_addr: &str,
    service_key: &str,
) -> Result<bool, String> {
    let addr = get_sockaddr_async(server_addr)
        .await
        .map_err(|e| format!("Invalid server address: {e}"))?;

    match TcpStreamProvider::from_addr(addr).await {
        Ok(mut stream) => {
            let status_req = PbConnStatusReq::Keys;
            match get_status(&mut stream, status_req).await {
                Ok(status_resp) => match status_resp {
                    PbConnStatusResp::Keys(keys) => {
                        if keys.contains(&service_key.to_string()) {
                            Ok(true)
                        } else {
                            Err("Service not found in server".to_string())
                        }
                    }
                    _ => Ok(true),
                },
                Err(_) => Ok(false),
            }
        }
        Err(_) => Err("Cannot connect to server".to_string()),
    }
}

async fn fetch_real_status_with_addr(
    server_addr: &str,
) -> Result<(Vec<String>, RemoteIdData), String> {
    let services = match get_server_keys_with_addr(server_addr).await {
        Ok(keys) => keys,
        Err(e) => return Err(format!("Failed to get server keys: {e}")),
    };

    let remote_id_data = match get_remote_id_data_with_addr(server_addr).await {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!("Failed to get remote-id data: {}, using empty data", e);
            RemoteIdData {
                server_map: String::new(),
                active: String::new(),
                idle: String::new(),
            }
        }
    };

    Ok((services, remote_id_data))
}

async fn get_server_keys_with_addr(server_addr: &str) -> Result<Vec<String>, String> {
    use tokio::net::TcpStream;

    let socket_addr = got_one_socket_addr(server_addr)
        .await
        .map_err(|e| format!("Invalid server address {server_addr}: {e}"))?;

    let mut stream = each_addr(socket_addr, TcpStream::connect)
        .await
        .map_err(|e| format!("Failed to connect to server: {e}"))?;

    let status_resp = get_status(&mut stream, PbConnStatusReq::Keys)
        .await
        .map_err(|e| format!("Failed to get status: {e}"))?;

    match status_resp {
        PbConnStatusResp::Keys(keys) => Ok(keys),
        _ => Err("Unexpected response type for Keys request".to_string()),
    }
}

async fn get_remote_id_data_with_addr(server_addr: &str) -> Result<RemoteIdData, String> {
    use tokio::net::TcpStream;

    let socket_addr = got_one_socket_addr(server_addr)
        .await
        .map_err(|e| format!("Invalid server address {server_addr}: {e}"))?;

    let mut stream = each_addr(socket_addr, TcpStream::connect)
        .await
        .map_err(|e| format!("Failed to connect to server: {e}"))?;

    let status_resp = get_status(&mut stream, PbConnStatusReq::RemoteId)
        .await
        .map_err(|e| format!("Failed to get status: {e}"))?;

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
        _ => Err("Unexpected response type for RemoteId request".to_string()),
    }
}

fn cache_is_stale(last_update: Option<Instant>, ttl: Duration) -> bool {
    match last_update {
        Some(ts) => ts.elapsed() > ttl,
        None => true,
    }
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
pub struct AppConfig {
    pub server_address: String,
    pub keep_alive_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_address: "localhost:7666".to_string(),
            keep_alive_enabled: true,
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
    server_status_cache: Arc<RwLock<ServerStatusDetail>>,
    server_status_last_update: Arc<RwLock<Option<Instant>>>,
    server_status_refreshing: Arc<AtomicBool>,
    local_server_status_cache: Arc<RwLock<LocalServerStatus>>,
    local_server_status_last_update: Arc<RwLock<Option<Instant>>>,
    local_server_status_refreshing: Arc<AtomicBool>,
    service_status_cache: Arc<RwLock<HashMap<String, StatusCacheEntry>>>,
    client_status_cache: Arc<RwLock<HashMap<String, StatusCacheEntry>>>,
    service_status_refreshing: Arc<RwLock<HashSet<String>>>,
    client_status_refreshing: Arc<RwLock<HashSet<String>>>,
}

impl PbMapperState {
    async fn reset_status_caches(&self) {
        {
            let mut cache = self.server_status_cache.write().await;
            *cache = ServerStatusDetail {
                server_available: false,
                registered_services: Vec::new(),
                server_map: String::new(),
                active_connections: String::new(),
                idle_connections: String::new(),
            };
        }
        {
            let mut last_update = self.server_status_last_update.write().await;
            *last_update = None;
        }
        self.server_status_refreshing
            .store(false, Ordering::Release);

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

        let server_status_cache = Arc::new(RwLock::new(ServerStatusDetail {
            server_available: false,
            registered_services: Vec::new(),
            server_map: String::new(),
            active_connections: String::new(),
            idle_connections: String::new(),
        }));
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
            server_status_cache: server_status_cache.clone(),
            server_status_last_update: Arc::new(RwLock::new(None)),
            server_status_refreshing: Arc::new(AtomicBool::new(false)),
            local_server_status_cache: local_server_status_cache.clone(),
            local_server_status_last_update: Arc::new(RwLock::new(None)),
            local_server_status_refreshing: Arc::new(AtomicBool::new(false)),
            service_status_cache: Arc::new(RwLock::new(HashMap::new())),
            client_status_cache: Arc::new(RwLock::new(HashMap::new())),
            service_status_refreshing: Arc::new(RwLock::new(HashSet::new())),
            client_status_refreshing: Arc::new(RwLock::new(HashSet::new())),
        };

        let config = temp_state.load_config().unwrap_or_else(|e| {
            tracing::warn!("Could not load config: {}, using defaults", e);
            AppConfig::default()
        });

        tracing::info!(
            "Loaded configuration: server_address={}, keep_alive={}",
            config.server_address,
            config.keep_alive_enabled
        );

        Self {
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
            server_status_cache,
            server_status_last_update: Arc::new(RwLock::new(None)),
            server_status_refreshing: Arc::new(AtomicBool::new(false)),
            local_server_status_cache,
            local_server_status_last_update: Arc::new(RwLock::new(None)),
            local_server_status_refreshing: Arc::new(AtomicBool::new(false)),
            service_status_cache: Arc::new(RwLock::new(HashMap::new())),
            client_status_cache: Arc::new(RwLock::new(HashMap::new())),
            service_status_refreshing: Arc::new(RwLock::new(HashSet::new())),
            client_status_refreshing: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    pub fn set_app_directory_path(&mut self, path: Option<String>) -> Result<(), String> {
        self.app_directory_path = path;
        self.config_dir = Self::get_config_dir(&self.app_directory_path);

        // Reload config from new location if exists
        match self.load_config() {
            Ok(config) => self.config = config,
            Err(e) => {
                tracing::warn!("Failed to reload config after setting app dir: {}", e);
            }
        }

        Ok(())
    }

    #[allow(unused_variables)]
    fn get_config_dir(app_directory_path: &Option<String>) -> PathBuf {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            if let Some(app_dir) = app_directory_path {
                let path = PathBuf::from(app_dir).join("pb-mapper-ui");
                tracing::info!("Using Flutter-provided app directory: {:?}", path);
                return path;
            } else {
                tracing::warn!(
                    "No app directory provided for mobile platform, using relative path"
                );
                PathBuf::from("pb-mapper-ui")
            }
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

    pub fn load_config(&self) -> Result<AppConfig, String> {
        let config_path = self.get_config_file_path();
        if config_path.exists() {
            let contents = fs::read_to_string(config_path).map_err(|e| e.to_string())?;
            let config: AppConfig = serde_json::from_str(&contents).map_err(|e| e.to_string())?;
            Ok(config)
        } else {
            Ok(AppConfig::default())
        }
    }

    pub fn save_config(&self) -> Result<(), String> {
        let config_path = self.get_config_file_path();
        let contents = serde_json::to_string_pretty(&self.config).map_err(|e| e.to_string())?;
        fs::write(config_path, contents).map_err(|e| e.to_string())?;
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

    pub fn save_service_configs(&self, store: &ServiceConfigStore) -> Result<(), String> {
        let path = self.get_service_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create config dir: {e}"))?;
        }

        let content = serde_json::to_string_pretty(store)
            .map_err(|e| format!("Failed to serialize config: {e}"))?;

        fs::write(&path, content).map_err(|e| format!("Failed to write config file: {e}"))?;
        Ok(())
    }

    pub fn save_service_config(
        &self,
        service_key: &str,
        local_address: &str,
        protocol: &str,
        enable_encryption: bool,
        enable_keep_alive: bool,
    ) -> Result<(), String> {
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

    pub fn delete_service_config(&self, service_key: &str) -> Result<(), String> {
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

    pub fn save_client_configs(&self, store: &ClientConfigStore) -> Result<(), String> {
        let path = self.get_client_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create config dir: {e}"))?;
        }

        let content = serde_json::to_string_pretty(store)
            .map_err(|e| format!("Failed to serialize client config: {e}"))?;

        fs::write(&path, content)
            .map_err(|e| format!("Failed to write client config file: {e}"))?;
        Ok(())
    }

    pub fn save_client_config(
        &self,
        service_key: &str,
        local_address: &str,
        protocol: &str,
        enable_keep_alive: bool,
    ) -> Result<(), String> {
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

    pub fn delete_client_config(&self, service_key: &str) -> Result<(), String> {
        let mut store = self.load_client_configs();
        store.clients.remove(service_key);
        self.save_client_configs(&store)
    }

    pub async fn start_server(&mut self, port: u16, enable_keep_alive: bool) -> Result<(), String> {
        if self.server_handle.is_some() {
            return Err("Server is already running".to_string());
        }

        if enable_keep_alive {
            std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
        } else {
            std::env::remove_var(PB_MAPPER_KEEP_ALIVE);
        }

        let ip_addr = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
        let bind_addr = std::net::SocketAddr::new(ip_addr, port);

        // Preflight bind to surface "port already in use" errors before spawning.
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| format!("Failed to bind server on {bind_addr}: {e}"))?;
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

    pub async fn stop_server(&mut self) -> Result<(), String> {
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
            Err("Server is not running".to_string())
        }
    }

    pub async fn register_service(
        &mut self,
        service_key: String,
        local_address: String,
        protocol: String,
        enable_encryption: bool,
        enable_keep_alive: bool,
    ) -> Result<(), String> {
        if self.service_handles.contains_key(&service_key) {
            tracing::warn!(
                "Service '{service_key}' is already registered, replacing existing handle"
            );
            self.service_handles.remove(&service_key);
        }

        if enable_keep_alive {
            std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
        }

        let local_sock_addr = get_sockaddr_async(&local_address)
            .await
            .map_err(|e| format!("Invalid local address: {e}"))?;
        let remote_sock_addr = get_pb_mapper_server_async(Some(&self.config.server_address))
            .await
            .map_err(|e| format!("Invalid server address: {e}"))?;

        // Preflight remote server connectivity to surface errors early.
        TcpStream::connect(remote_sock_addr).await.map_err(|e| {
            format!(
                "Failed to connect to server {}: {e}",
                self.config.server_address
            )
        })?;

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
        .map_err(|e| format!("Failed to save service configuration: {e}"))?;

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
                    enable_encryption,
                    false,
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
                    enable_encryption,
                    true,
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

    pub async fn unregister_service(&mut self, service_key: String) -> Result<(), String> {
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
            Err(format!("Service '{service_key}' is not registered"))
        }
    }

    pub async fn delete_service_config_and_stop(
        &mut self,
        service_key: String,
    ) -> Result<(), String> {
        if let Some(handle) = self.service_handles.remove(&service_key) {
            handle.abort();
        }

        self.registered_services.write().await.remove(&service_key);

        self.delete_service_config(&service_key)
    }

    pub async fn connect_service(
        &mut self,
        service_key: String,
        local_address: String,
        protocol: String,
        enable_keep_alive: bool,
    ) -> Result<(), String> {
        if self.client_handles.contains_key(&service_key) {
            tracing::warn!(
                "Client for service '{service_key}' is already connected, replacing handle"
            );
            self.client_handles.remove(&service_key);
        }

        if enable_keep_alive {
            std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
        }

        let local_sock_addr = get_sockaddr_async(&local_address)
            .await
            .map_err(|e| format!("Invalid local address: {e}"))?;
        let remote_sock_addr = get_pb_mapper_server_async(Some(&self.config.server_address))
            .await
            .map_err(|e| format!("Invalid server address: {e}"))?;

        // Preflight local bind to detect "port already in use" before starting client.
        let protocol_upper = protocol.to_uppercase();
        if protocol_upper == "TCP" {
            let listener = TcpListenerProvider::bind(local_sock_addr)
                .await
                .map_err(|e| format!("Failed to bind local address {local_address}: {e}"))?;
            drop(listener);
        } else {
            let listener = UdpListenerProvider::bind(local_sock_addr)
                .await
                .map_err(|e| format!("Failed to bind local address {local_address}: {e}"))?;
            drop(listener);
        }

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

    pub async fn disconnect_service(&mut self, service_key: String) -> Result<(), String> {
        if let Some(handle) = self.client_handles.remove(&service_key) {
            handle.abort();
        }

        if self
            .active_connections
            .write()
            .await
            .remove(&service_key)
            .is_some()
        {
            tracing::info!("Disconnected from service '{}'", service_key);
            Ok(())
        } else {
            Err(format!("Service '{service_key}' is not connected"))
        }
    }

    pub async fn delete_client_config_and_stop(
        &mut self,
        service_key: String,
    ) -> Result<(), String> {
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
    ) -> Result<(), String> {
        self.config.server_address = server_address;
        self.config.keep_alive_enabled = keep_alive;
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

    // Return cached status immediately so UI threads stay responsive; refresh happens in background.
    pub async fn get_server_status_detail(&self) -> Result<ServerStatusDetail, String> {
        let should_refresh = {
            let last_update = self.server_status_last_update.read().await;
            cache_is_stale(*last_update, SERVER_STATUS_TTL)
        };

        if should_refresh {
            self.schedule_server_status_refresh();
        }

        let cache = self.server_status_cache.read().await;
        Ok(cache.clone())
    }

    fn schedule_server_status_refresh(&self) {
        if self.server_status_refreshing.swap(true, Ordering::AcqRel) {
            return;
        }

        let server_addr = self.config.server_address.clone();
        let cache = self.server_status_cache.clone();
        let last_update = self.server_status_last_update.clone();
        let refreshing = self.server_status_refreshing.clone();

        tokio::spawn(async move {
            let detail = match tokio::time::timeout(
                STATUS_REFRESH_TIMEOUT,
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
                    tracing::warn!("Failed to fetch server status: {}", e);
                    ServerStatusDetail {
                        server_available: false,
                        registered_services: Vec::new(),
                        server_map: String::new(),
                        active_connections: String::new(),
                        idle_connections: String::new(),
                    }
                }
                Err(_) => ServerStatusDetail {
                    server_available: false,
                    registered_services: Vec::new(),
                    server_map: String::new(),
                    active_connections: String::new(),
                    idle_connections: String::new(),
                },
            };

            {
                let mut cache = cache.write().await;
                *cache = detail;
            }
            {
                let mut last_update = last_update.write().await;
                *last_update = Some(Instant::now());
            }
            refreshing.store(false, Ordering::Release);
        });
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
