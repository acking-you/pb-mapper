use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use messages::prelude::{Actor, Address, Context, Notifiable};
use rinf::{DartSignal, RustSignal};
// Removed robius-directories import to avoid Android ndk-context panic
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tokio_with_wasm::alias as tokio;

use crate::signals::{
    ActiveConnectionInfo, ActiveConnectionsUpdate, ClientConfigInfo, ClientConfigsUpdate,
    ClientConnectionStatus, ClientStatusResponse, ConfigSaveResult, ConfigStatusUpdate,
    ConnectServiceRequest, DeleteClientConfigRequest, DeleteServiceConfigRequest,
    DisconnectServiceRequest, LocalServerStatusUpdate, RegisterServiceRequest,
    RegisteredServiceInfo, RegisteredServicesUpdate, RequestClientConfigs, RequestClientStatus,
    RequestConfig, RequestLocalServerStatus, RequestServerStatus, RequestServiceConfigs,
    RequestServiceStatus, ServerStatusDetailUpdate, ServerStatusUpdate, ServiceConfigInfo,
    ServiceConfigsUpdate, ServiceRegistrationStatusUpdate, ServiceStatusResponse,
    ServiceStatusUpdate, StartServerRequest, StopServerRequest, UnregisterServiceRequest,
    UpdateConfigRequest,
};
use pb_mapper::common::config::{PB_MAPPER_KEEP_ALIVE, get_pb_mapper_server, get_sockaddr};
use pb_mapper::common::listener::{TcpListenerProvider, UdpListenerProvider};
use pb_mapper::common::message::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper::common::stream::got_one_socket_addr;
use pb_mapper::common::stream::{TcpStreamProvider, UdpStreamProvider};
use pb_mapper::local::client::status::get_status;
use pb_mapper::local::client::{ClientStatusCallback, run_client_side_cli_with_callback};
use pb_mapper::local::server::{StatusCallback, run_server_side_cli_with_callback};
use pb_mapper::pb_server::{ServerStatusInfo, run_server_with_shutdown};
use pb_mapper::utils::addr::each_addr;

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
#[derive(Serialize, Deserialize)]
struct AppConfig {
    server_address: String,
    keep_alive_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_address: "localhost:7666".to_string(),
            keep_alive_enabled: true,
        }
    }
}

/// Helper struct to hold RemoteId response data
struct RemoteIdData {
    server_map: String,
    active: String,
    idle: String,
}

pub struct PbMapperActor {
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
    app_directory_path: Option<String>, // Flutter-provided app directory path
    _owned_tasks: JoinSet<()>,
}

#[derive(Clone)]
struct ServiceInfo {
    service_key: String,
    protocol: String,
    local_address: String,
    status: String,
}

#[derive(Clone)]
struct ConnectionInfo {
    service_key: String,
    client_id: String,
    status: String,
}

impl Actor for PbMapperActor {}

impl PbMapperActor {
    pub fn new(self_addr: Address<Self>, app_directory_path: Option<String>) -> Self {
        let mut owned_tasks = JoinSet::new();

        // Spawn signal listeners
        owned_tasks.spawn(Self::listen_to_start_server(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_stop_server(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_register_service(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_unregister_service(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_connect_service(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_disconnect_service(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_request_config(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_update_config(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_request_server_status(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_request_local_server_status(
            self_addr.clone(),
        ));
        owned_tasks.spawn(Self::listen_to_request_service_configs(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_request_service_status(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_delete_service_config(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_request_client_configs(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_request_client_status(self_addr.clone()));
        owned_tasks.spawn(Self::listen_to_delete_client_config(self_addr.clone()));

        // Note: Removed periodic_status_updates - UI will actively request status

        // Initialize config directory
        let config_dir = Self::get_config_dir(&app_directory_path);
        tracing::info!("Using config directory: {:?}", config_dir);

        // Create a temporary actor to load config (since load_config needs &self)
        let temp_actor = Self {
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
            _owned_tasks: JoinSet::new(),
        };

        // Load configuration from file
        let config = temp_actor.load_config().unwrap_or_else(|e| {
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
            _owned_tasks: owned_tasks,
        }
    }

    #[allow(unused)]
    fn get_config_dir(app_directory_path: &Option<String>) -> PathBuf {
        // For mobile platforms, use Flutter-provided app directory path
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            if let Some(app_dir) = app_directory_path {
                let path = PathBuf::from(app_dir).join("pb-mapper-ui");
                tracing::info!("Using Flutter-provided app directory: {:?}", path);
                return path;
            } else {
                // Fallback for mobile platforms when no path provided
                tracing::warn!(
                    "No app directory provided for mobile platform, using relative path"
                );
                PathBuf::from("pb-mapper-ui")
            }
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            // Use config directory for desktop platforms
            if let Some(config_dir) = dirs::config_dir() {
                config_dir.join("pb-mapper-ui")
            } else if let Some(home_dir) = dirs::home_dir() {
                home_dir.join(".config").join("pb-mapper-ui")
            } else {
                // Fallback to current directory if home directory is not available
                tracing::warn!("Could not determine home directory, using current directory");
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join("pb-mapper-ui-config")
            }
        }
    }

    fn get_service_config_path(&self) -> PathBuf {
        self.config_dir.join("services.json")
    }

    fn get_client_config_path(&self) -> PathBuf {
        self.config_dir.join("clients.json")
    }

    fn load_service_configs(&self) -> ServiceConfigStore {
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

    fn save_service_configs(&self, store: &ServiceConfigStore) -> Result<(), String> {
        let path = self.get_service_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create config dir: {e}"))?;
        }

        let content = serde_json::to_string_pretty(store)
            .map_err(|e| format!("Failed to serialize config: {e}"))?;

        fs::write(&path, content).map_err(|e| format!("Failed to write config file: {e}"))?;

        Ok(())
    }

    fn save_service_config(
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

    fn delete_service_config(&self, service_key: &str) -> Result<(), String> {
        let mut store = self.load_service_configs();
        store.services.remove(service_key);
        self.save_service_configs(&store)
    }

    fn load_client_configs(&self) -> ClientConfigStore {
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

    fn save_client_configs(&self, store: &ClientConfigStore) -> Result<(), String> {
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

    fn save_client_config(
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

    fn delete_client_config(&self, service_key: &str) -> Result<(), String> {
        let mut store = self.load_client_configs();
        store.clients.remove(service_key);
        self.save_client_configs(&store)
    }

    async fn calculate_service_status(&self, service_key: &str) -> (String, String) {
        // Check if service handle exists
        if let Some(handle) = self.service_handles.get(service_key) {
            if handle.is_finished() {
                (
                    "failed".to_string(),
                    "Service connection terminated".to_string(),
                )
            } else {
                // Service is running, now check actual status with get_status
                let server_addr = &self.config.server_address;
                match self
                    .check_service_with_get_status(server_addr, service_key)
                    .await
                {
                    Ok(true) => (
                        "running".to_string(),
                        "Service is running normally".to_string(),
                    ),
                    Ok(false) => (
                        "retrying".to_string(),
                        "Service is in retry connection loop".to_string(),
                    ),
                    Err(_) => (
                        "failed".to_string(),
                        "Cannot connect to pb-server".to_string(),
                    ),
                }
            }
        } else {
            (
                "stopped".to_string(),
                "Service is not registered".to_string(),
            )
        }
    }

    async fn check_service_with_get_status(
        &self,
        server_addr: &str,
        service_key: &str,
    ) -> Result<bool, String> {
        use pb_mapper::common::stream::{StreamProvider, TcpStreamProvider};
        use pb_mapper::local::client::status::get_status;

        let addr = get_sockaddr(server_addr).map_err(|e| format!("Invalid server address: {e}"))?;

        // Create TCP connection to server
        match TcpStreamProvider::from_addr(addr).await {
            Ok(mut stream) => {
                let status_req = PbConnStatusReq::Keys; // Request keys from server
                match get_status(&mut stream, status_req).await {
                    Ok(status_resp) => {
                        // Check if our service key is in the response
                        match status_resp {
                            PbConnStatusResp::Keys(keys) => {
                                if keys.contains(&service_key.to_string()) {
                                    Ok(true) // Service is registered and running
                                } else {
                                    Err("Service not found in server".to_string())
                                }
                            }
                            _ => Ok(true), // Server responded, assume service is running
                        }
                    }
                    Err(_) => {
                        // Connection failed, service is in retry mode
                        Ok(false)
                    }
                }
            }
            Err(_) => {
                // Cannot connect to server
                Err("Cannot connect to server".to_string())
            }
        }
    }

    async fn calculate_client_status(&self, service_key: &str) -> (String, String) {
        // Check if client handle exists
        if let Some(handle) = self.client_handles.get(service_key) {
            if handle.is_finished() {
                (
                    "failed".to_string(),
                    "Client connection terminated".to_string(),
                )
            } else {
                // Client is running, now check actual status with get_status
                let server_addr = &self.config.server_address;
                match self
                    .check_service_with_get_status(server_addr, service_key)
                    .await
                {
                    Ok(true) => (
                        "running".to_string(),
                        "Client is connected normally".to_string(),
                    ),
                    Ok(false) => (
                        "retrying".to_string(),
                        "Client is in retry connection loop".to_string(),
                    ),
                    Err(_) => (
                        "failed".to_string(),
                        "Cannot connect to pb-server".to_string(),
                    ),
                }
            }
        } else {
            ("stopped".to_string(), "Client is not connected".to_string())
        }
    }

    /// Fetch real status data from pb-mapper server
    async fn fetch_real_status(&self) -> Result<(Vec<String>, RemoteIdData), String> {
        let server_addr = self.config.server_address.clone();

        // First get keys (list of registered services)
        let services = match self.get_server_keys(&server_addr).await {
            Ok(keys) => keys,
            Err(e) => return Err(format!("Failed to get server keys: {e}")),
        };

        // Then get remote-id data (server map and connection info)
        let remote_id_data = match self.get_remote_id_data(&server_addr).await {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!("Failed to get remote-id data: {}, using empty data", e);
                // If we can get keys but not remote-id, still return success with empty remote data
                RemoteIdData {
                    server_map: String::new(),
                    active: String::new(),
                    idle: String::new(),
                }
            }
        };

        Ok((services, remote_id_data))
    }

    /// Get list of registered service keys from server
    async fn get_server_keys(&self, server_addr: &str) -> Result<Vec<String>, String> {
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

    /// Get remote-id data from server
    async fn get_remote_id_data(&self, server_addr: &str) -> Result<RemoteIdData, String> {
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

    async fn listen_to_start_server(mut self_addr: Address<Self>) {
        let receiver = StartServerRequest::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_stop_server(mut self_addr: Address<Self>) {
        let receiver = StopServerRequest::get_dart_signal_receiver();
        #[allow(clippy::redundant_pattern_matching)]
        while let Some(_) = receiver.recv().await {
            let _ = self_addr.notify(StopServerRequest).await;
        }
    }

    async fn listen_to_register_service(mut self_addr: Address<Self>) {
        let receiver = RegisterServiceRequest::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_unregister_service(mut self_addr: Address<Self>) {
        let receiver = UnregisterServiceRequest::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_connect_service(mut self_addr: Address<Self>) {
        let receiver = ConnectServiceRequest::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_disconnect_service(mut self_addr: Address<Self>) {
        let receiver = DisconnectServiceRequest::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_request_config(mut self_addr: Address<Self>) {
        let receiver = RequestConfig::get_dart_signal_receiver();
        #[allow(clippy::redundant_pattern_matching)]
        while let Some(_) = receiver.recv().await {
            let _ = self_addr.notify(RequestConfig).await;
        }
    }

    async fn listen_to_update_config(mut self_addr: Address<Self>) {
        let receiver = UpdateConfigRequest::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_request_server_status(mut self_addr: Address<Self>) {
        let receiver = RequestServerStatus::get_dart_signal_receiver();
        #[allow(clippy::redundant_pattern_matching)]
        while let Some(_) = receiver.recv().await {
            let _ = self_addr.notify(RequestServerStatus).await;
        }
    }

    async fn listen_to_request_local_server_status(mut self_addr: Address<Self>) {
        let receiver = RequestLocalServerStatus::get_dart_signal_receiver();
        #[allow(clippy::redundant_pattern_matching)]
        while let Some(_) = receiver.recv().await {
            let _ = self_addr.notify(RequestLocalServerStatus).await;
        }
    }

    async fn listen_to_request_service_configs(mut self_addr: Address<Self>) {
        let receiver = RequestServiceConfigs::get_dart_signal_receiver();
        #[allow(clippy::redundant_pattern_matching)]
        while let Some(_) = receiver.recv().await {
            let _ = self_addr.notify(RequestServiceConfigs).await;
        }
    }

    async fn listen_to_request_service_status(mut self_addr: Address<Self>) {
        let receiver = RequestServiceStatus::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_delete_service_config(mut self_addr: Address<Self>) {
        let receiver = DeleteServiceConfigRequest::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_request_client_configs(mut self_addr: Address<Self>) {
        let receiver = RequestClientConfigs::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_request_client_status(mut self_addr: Address<Self>) {
        let receiver = RequestClientStatus::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn listen_to_delete_client_config(mut self_addr: Address<Self>) {
        let receiver = DeleteClientConfigRequest::get_dart_signal_receiver();
        while let Some(signal_pack) = receiver.recv().await {
            let _ = self_addr.notify(signal_pack.message).await;
        }
    }

    async fn start_server_internal(
        &mut self,
        port: u16,
        enable_keep_alive: bool,
    ) -> Result<(), String> {
        if self.server_handle.is_some() {
            return Err("Server is already running".to_string());
        }

        // Set keep-alive environment variable
        if enable_keep_alive {
            unsafe {
                std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
            }
        } else {
            unsafe {
                std::env::remove_var(PB_MAPPER_KEEP_ALIVE);
            }
        }

        // Always use IPv4 for simplicity
        let ip_addr = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));

        tracing::info!("Starting pb-mapper server on {}:{}", ip_addr, port);

        // Create a cancellation token for graceful shutdown
        let shutdown_token = CancellationToken::new();
        let shutdown_token_clone = shutdown_token.clone();

        // Create status channel for real-time server status queries
        let (status_sender, status_receiver) = tokio::sync::mpsc::unbounded_channel();

        let handle = tokio::spawn(async move {
            run_server_with_shutdown((ip_addr, port), shutdown_token_clone, Some(status_receiver))
                .await;
        });

        self.server_handle = Some(handle);
        self.server_shutdown_token = Some(shutdown_token);
        self.server_status_sender = Some(status_sender);
        self.server_start_time = Some(SystemTime::now());

        ServerStatusUpdate {
            status: "Running".to_string(),
            active_connections: 0,
            uptime: 0,
        }
        .send_signal_to_dart();

        tracing::info!("pb-mapper server started successfully");
        Ok(())
    }

    async fn stop_server_internal(&mut self) -> Result<(), String> {
        if let (Some(handle), Some(shutdown_token)) =
            (self.server_handle.take(), self.server_shutdown_token.take())
        {
            // Clear the status sender channel
            self.server_status_sender = None;

            // Signal graceful shutdown
            shutdown_token.cancel();

            // Wait a reasonable time for graceful shutdown
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

            // Stop all service registration handles
            for (_, handle) in self.service_handles.drain() {
                handle.abort();
            }

            // Stop all client connection handles
            for (_, handle) in self.client_handles.drain() {
                handle.abort();
            }

            // Clear registered services and connections
            self.registered_services.write().await.clear();
            self.active_connections.write().await.clear();

            ServerStatusUpdate {
                status: "Stopped".to_string(),
                active_connections: 0,
                uptime: 0,
            }
            .send_signal_to_dart();

            RegisteredServicesUpdate { services: vec![] }.send_signal_to_dart();

            ActiveConnectionsUpdate {
                connections: vec![],
            }
            .send_signal_to_dart();

            tracing::info!("pb-mapper server stopped, all services and connections terminated");
            Ok(())
        } else {
            Err("Server is not running".to_string())
        }
    }

    async fn register_service_internal(
        &mut self,
        service_key: String,
        local_address: String,
        protocol: String,
        enable_encryption: bool,
        enable_keep_alive: bool,
    ) -> Result<(), String> {
        if self.service_handles.contains_key(&service_key) {
            tracing::warn!(
                "Service '{service_key}' is already registered,You should check service key in flutter UI"
            );
            self.service_handles.remove(&service_key);
        }

        if enable_keep_alive {
            unsafe {
                std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
            }
        }

        let local_sock_addr =
            get_sockaddr(&local_address).map_err(|e| format!("Invalid local address: {e}"))?;
        let remote_sock_addr = get_pb_mapper_server(Some(&self.config.server_address))
            .map_err(|e| format!("Invalid server address: {e}"))?;

        tracing::info!(
            "Registering service '{}' with protocol {}, local address {}, server address {}",
            service_key,
            protocol,
            local_address,
            self.config.server_address
        );

        // Save service configuration immediately
        if let Err(e) = self.save_service_config(
            &service_key,
            &local_address,
            &protocol,
            enable_encryption,
            enable_keep_alive,
        ) {
            return Err(format!("Failed to save service configuration: {e}"));
        }

        // Send initial "retrying" status to indicate registration is starting
        ServiceRegistrationStatusUpdate {
            service_key: service_key.clone(),
            status: "retrying".to_string(),
            message: "Starting service registration...".to_string(),
        }
        .send_signal_to_dart();

        let key_clone = service_key.clone();
        let service_key_for_status = service_key.clone();
        let service_key_for_callback = service_key.clone();

        // Create status callback
        let callback: StatusCallback = Box::new(move |status: &str| {
            let status_signal = match status {
                "connected" => ServiceRegistrationStatusUpdate {
                    service_key: service_key_for_callback.clone(),
                    status: "running".to_string(),
                    message: "Service successfully connected to pb-server".to_string(),
                },
                "retrying" => ServiceRegistrationStatusUpdate {
                    service_key: service_key_for_callback.clone(),
                    status: "retrying".to_string(),
                    message: "Service is retrying connection to pb-server".to_string(),
                },
                failed_msg => ServiceRegistrationStatusUpdate {
                    service_key: service_key_for_callback.clone(),
                    status: "failed".to_string(),
                    message: format!("Service connection failed with:{}", failed_msg),
                },
            };
            status_signal.send_signal_to_dart();
        });

        let handle = if protocol.to_uppercase() == "TCP" {
            tokio::spawn(async move {
                let result = run_server_side_cli_with_callback::<TcpStreamProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    enable_encryption,
                    Some(callback),
                )
                .await;

                // Service finished - send appropriate status based on result
                ServiceRegistrationStatusUpdate {
                    service_key: service_key_for_status.clone(),
                    status: "stopped".to_string(),
                    message: "Service registration stopped".to_string(),
                }
                .send_signal_to_dart();

                result
            })
        } else {
            let service_key_for_status_udp = service_key_for_status.clone();
            tokio::spawn(async move {
                let result = run_server_side_cli_with_callback::<UdpStreamProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    enable_encryption,
                    Some(callback),
                )
                .await;

                // Service finished - send appropriate status based on result
                ServiceRegistrationStatusUpdate {
                    service_key: service_key_for_status_udp,
                    status: "stopped".to_string(),
                    message: "Service registration stopped".to_string(),
                }
                .send_signal_to_dart();

                result
            })
        };

        self.service_handles.insert(service_key.clone(), handle);

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

        let services: Vec<RegisteredServiceInfo> = self
            .registered_services
            .read()
            .await
            .values()
            .map(|service| RegisteredServiceInfo {
                service_key: service.service_key.clone(),
                protocol: service.protocol.clone(),
                local_address: service.local_address.clone(),
                status: service.status.clone(),
            })
            .collect();

        RegisteredServicesUpdate { services }.send_signal_to_dart();

        ServiceStatusUpdate {
            message: format!("Service '{service_key}' registration initiated"),
        }
        .send_signal_to_dart();

        tracing::info!("Service '{}' registration initiated", service_key);
        Ok(())
    }

    async fn unregister_service_internal(&mut self, service_key: String) -> Result<(), String> {
        if let Some(handle) = self.service_handles.remove(&service_key) {
            handle.abort();
        }

        // Keep configuration in file, only stop the service

        if self
            .registered_services
            .write()
            .await
            .remove(&service_key)
            .is_some()
        {
            // Send stopped status update
            ServiceRegistrationStatusUpdate {
                service_key: service_key.clone(),
                status: "stopped".to_string(),
                message: "Service stopped by user".to_string(),
            }
            .send_signal_to_dart();

            let services: Vec<RegisteredServiceInfo> = self
                .registered_services
                .read()
                .await
                .values()
                .map(|service| RegisteredServiceInfo {
                    service_key: service.service_key.clone(),
                    protocol: service.protocol.clone(),
                    local_address: service.local_address.clone(),
                    status: service.status.clone(),
                })
                .collect();

            RegisteredServicesUpdate { services }.send_signal_to_dart();

            ServiceStatusUpdate {
                message: format!("Service '{service_key}' unregistered successfully"),
            }
            .send_signal_to_dart();

            tracing::info!("Service '{}' unregistered successfully", service_key);
            Ok(())
        } else {
            Err(format!("Service '{service_key}' is not registered"))
        }
    }

    async fn connect_service_internal(
        &mut self,
        service_key: String,
        local_address: String,
        protocol: String,
        enable_keep_alive: bool,
    ) -> Result<(), String> {
        if self.client_handles.contains_key(&service_key) {
            tracing::warn!(
                "Client for service '{service_key}' is already connected.You should complete the correct detection on the Flutter side."
            );
            self.client_handles.remove(&service_key);
        }

        if enable_keep_alive {
            unsafe {
                std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
            }
        }

        let local_sock_addr =
            get_sockaddr(&local_address).map_err(|e| format!("Invalid local address: {e}"))?;
        let remote_sock_addr = get_pb_mapper_server(Some(&self.config.server_address))
            .map_err(|e| format!("Invalid server address: {e}"))?;

        tracing::info!(
            "Connecting to service '{}' with protocol {}, local address {}, server address {}",
            service_key,
            protocol,
            local_address,
            self.config.server_address
        );

        let key_clone = service_key.clone();

        // Create callback to update client status
        let status_callback: ClientStatusCallback = {
            let service_key_for_callback = service_key.clone();
            Box::new(move |status: &str| {
                // Send client connection status update
                ClientConnectionStatus {
                    status: format!("Client {service_key_for_callback}: {status}"),
                }
                .send_signal_to_dart();
            })
        };

        let handle = if protocol.to_uppercase() == "TCP" {
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

        let connection_info = ConnectionInfo {
            service_key: service_key.clone(),
            client_id: format!("client-{service_key}"),
            status: "Connected".to_string(),
        };

        self.active_connections
            .write()
            .await
            .insert(service_key.clone(), connection_info);

        let connections: Vec<ActiveConnectionInfo> = self
            .active_connections
            .read()
            .await
            .values()
            .map(|conn| ActiveConnectionInfo {
                service_key: conn.service_key.clone(),
                client_id: conn.client_id.clone(),
                status: conn.status.clone(),
            })
            .collect();

        ActiveConnectionsUpdate { connections }.send_signal_to_dart();

        ClientConnectionStatus {
            status: format!("Connected to service '{service_key}'"),
        }
        .send_signal_to_dart();

        tracing::info!("Connected to service '{}' successfully", service_key);
        Ok(())
    }

    async fn disconnect_service_internal(&mut self, service_key: String) -> Result<(), String> {
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
            let connections: Vec<ActiveConnectionInfo> = self
                .active_connections
                .read()
                .await
                .values()
                .map(|conn| ActiveConnectionInfo {
                    service_key: conn.service_key.clone(),
                    client_id: conn.client_id.clone(),
                    status: conn.status.clone(),
                })
                .collect();

            ActiveConnectionsUpdate { connections }.send_signal_to_dart();

            ClientConnectionStatus {
                status: format!("Disconnected from service '{service_key}'"),
            }
            .send_signal_to_dart();

            tracing::info!("Disconnected from service '{}'", service_key);
            Ok(())
        } else {
            Err(format!("Service '{service_key}' is not connected"))
        }
    }

    async fn get_uptime(&self) -> u64 {
        if let Some(start_time) = self.server_start_time {
            SystemTime::now()
                .duration_since(start_time)
                .unwrap_or_default()
                .as_secs()
        } else {
            0
        }
    }

    async fn query_local_server_status(&self) -> LocalServerStatusUpdate {
        let is_running = self.server_handle.is_some();

        if is_running && self.server_status_sender.is_some() {
            // Try to get real-time status from server
            if let Some(sender) = &self.server_status_sender {
                let (response_sender, response_receiver) = tokio::sync::oneshot::channel();

                if sender.send(response_sender).is_ok() {
                    // Wait for response with timeout
                    if let Ok(Ok(info)) =
                        tokio::time::timeout(tokio::time::Duration::from_secs(1), response_receiver)
                            .await
                    {
                        return LocalServerStatusUpdate {
                            is_running: true,
                            active_connections: info.active_connections,
                            registered_services: info.registered_services,
                            uptime_seconds: info.uptime_seconds,
                        };
                    }
                }
            }

            // Fallback to basic info if channel communication fails
            LocalServerStatusUpdate {
                is_running: true,
                active_connections: 0,  // Unavailable
                registered_services: 0, // Unavailable
                uptime_seconds: self.get_uptime().await,
            }
        } else {
            // Server not running
            LocalServerStatusUpdate {
                is_running: false,
                active_connections: 0,
                registered_services: 0,
                uptime_seconds: 0,
            }
        }
    }

    #[allow(dead_code)]
    async fn send_status_update(&self) {
        let uptime = self.get_uptime().await;
        let active_connections = self.active_connections.read().await.len() as u32;
        let status = if self.server_handle.is_some() {
            "Running".to_string()
        } else {
            "Stopped".to_string()
        };

        ServerStatusUpdate {
            status,
            active_connections,
            uptime,
        }
        .send_signal_to_dart();
    }
}

// Implement Notifiable for each signal type
#[async_trait]
impl Notifiable<StartServerRequest> for PbMapperActor {
    async fn notify(&mut self, msg: StartServerRequest, _: &Context<Self>) {
        if let Err(e) = self
            .start_server_internal(msg.port, msg.enable_keep_alive)
            .await
        {
            tracing::error!("Failed to start server: {}", e);
            ServerStatusUpdate {
                status: format!("Error: {e}"),
                active_connections: 0,
                uptime: 0,
            }
            .send_signal_to_dart();
        }
    }
}

#[async_trait]
impl Notifiable<StopServerRequest> for PbMapperActor {
    async fn notify(&mut self, _msg: StopServerRequest, _: &Context<Self>) {
        if let Err(e) = self.stop_server_internal().await {
            tracing::error!("Failed to stop server: {}", e);
        }
    }
}

#[async_trait]
impl Notifiable<RegisterServiceRequest> for PbMapperActor {
    async fn notify(&mut self, msg: RegisterServiceRequest, _: &Context<Self>) {
        if let Err(e) = self
            .register_service_internal(
                msg.service_key.clone(),
                msg.local_address,
                msg.protocol,
                msg.enable_encryption,
                msg.enable_keep_alive,
            )
            .await
        {
            tracing::error!("Failed to register service '{}': {}", msg.service_key, e);

            // Send specific error feedback to UI
            ServiceRegistrationStatusUpdate {
                service_key: msg.service_key.clone(),
                status: "failed".to_string(),
                message: format!("Registration failed: {e}"),
            }
            .send_signal_to_dart();

            // Also send general service status update
            ServiceStatusUpdate {
                message: format!("Error registering '{}': {e}", msg.service_key),
            }
            .send_signal_to_dart();
        }
    }
}

#[async_trait]
impl Notifiable<UnregisterServiceRequest> for PbMapperActor {
    async fn notify(&mut self, msg: UnregisterServiceRequest, _: &Context<Self>) {
        if let Err(e) = self.unregister_service_internal(msg.service_key).await {
            tracing::error!("Failed to unregister service: {}", e);
            ServiceStatusUpdate {
                message: format!("Error: {e}"),
            }
            .send_signal_to_dart();
        }
    }
}

#[async_trait]
impl Notifiable<ConnectServiceRequest> for PbMapperActor {
    async fn notify(&mut self, msg: ConnectServiceRequest, _: &Context<Self>) {
        // First save client configuration
        if let Err(e) = self.save_client_config(
            &msg.service_key,
            &msg.local_address,
            &msg.protocol,
            msg.enable_keep_alive,
        ) {
            tracing::error!("Failed to save client config: {}", e);
        }

        if let Err(e) = self
            .connect_service_internal(
                msg.service_key.clone(),
                msg.local_address,
                msg.protocol,
                msg.enable_keep_alive,
            )
            .await
        {
            tracing::error!("Failed to connect to service: {}", e);
            ClientConnectionStatus {
                status: format!("Error connecting {}: {e}", msg.service_key),
            }
            .send_signal_to_dart();
        } else {
            ClientConnectionStatus {
                status: format!("Successfully started connection to {}", msg.service_key),
            }
            .send_signal_to_dart();
        }
    }
}

#[async_trait]
impl Notifiable<DisconnectServiceRequest> for PbMapperActor {
    async fn notify(&mut self, msg: DisconnectServiceRequest, _: &Context<Self>) {
        if let Err(e) = self
            .disconnect_service_internal(msg.service_key.clone())
            .await
        {
            tracing::error!("Failed to disconnect service: {}", e);
            ClientConnectionStatus {
                status: format!("Error disconnecting {}: {e}", msg.service_key),
            }
            .send_signal_to_dart();
        } else {
            ClientConnectionStatus {
                status: format!("Successfully disconnected {}", msg.service_key),
            }
            .send_signal_to_dart();
        }
    }
}

#[async_trait]
impl Notifiable<RequestConfig> for PbMapperActor {
    async fn notify(&mut self, _msg: RequestConfig, _: &Context<Self>) {
        // Only send current configuration, don't trigger status updates
        self.send_config_status().await;
    }
}

#[async_trait]
impl Notifiable<UpdateConfigRequest> for PbMapperActor {
    async fn notify(&mut self, msg: UpdateConfigRequest, _: &Context<Self>) {
        // Update configuration in memory
        self.config.server_address = msg.server_address;
        self.config.keep_alive_enabled = msg.enable_keep_alive;

        // Attempt to save configuration to file
        match self.save_config() {
            Ok(_) => {
                tracing::info!("Configuration saved successfully");

                // Send success result to Flutter
                ConfigSaveResult {
                    success: true,
                    message: "Configuration saved successfully".to_string(),
                }
                .send_signal_to_dart();

                // Send updated configuration status
                self.send_config_status().await;
            }
            Err(e) => {
                tracing::error!("Failed to save configuration: {}", e);

                // Send error result to Flutter
                ConfigSaveResult {
                    success: false,
                    message: format!("Failed to save configuration: {e}"),
                }
                .send_signal_to_dart();
            }
        }
    }
}

#[async_trait]
impl Notifiable<RequestServerStatus> for PbMapperActor {
    async fn notify(&mut self, _msg: RequestServerStatus, _: &Context<Self>) {
        tracing::info!("Received request for server status");

        // Get real status data from pb-mapper server
        let (
            server_available,
            registered_services,
            server_map,
            active_connections,
            idle_connections,
        ) = match self.fetch_real_status().await {
            Ok((services, remote_id_data)) => {
                tracing::info!("Successfully fetched real server status");
                (
                    true,
                    services,
                    remote_id_data.server_map,
                    remote_id_data.active,
                    remote_id_data.idle,
                )
            }
            Err(e) => {
                tracing::warn!("Failed to fetch server status: {}", e);
                // Return unavailable status with empty data
                (false, vec![], String::new(), String::new(), String::new())
            }
        };

        ServerStatusDetailUpdate {
            server_available,
            registered_services,
            server_map,
            active_connections,
            idle_connections,
        }
        .send_signal_to_dart();

        tracing::info!(
            "Server status sent to Flutter UI (available: {})",
            server_available
        );
    }
}

#[async_trait]
impl Notifiable<RequestLocalServerStatus> for PbMapperActor {
    async fn notify(&mut self, _msg: RequestLocalServerStatus, _: &Context<Self>) {
        tracing::info!("Received request for local server status");

        let status = self.query_local_server_status().await;
        status.send_signal_to_dart();

        tracing::info!(
            "Local server status sent to Flutter UI (running: {}, connections: {}, services: {})",
            status.is_running,
            status.active_connections,
            status.registered_services
        );
    }
}

#[async_trait]
impl Notifiable<RequestServiceConfigs> for PbMapperActor {
    async fn notify(&mut self, _msg: RequestServiceConfigs, _: &Context<Self>) {
        tracing::info!("Received request for service configurations");

        let store = self.load_service_configs();
        let mut services = Vec::new();

        // Sort configurations by creation time to ensure consistent ordering
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

        ServiceConfigsUpdate { services }.send_signal_to_dart();
        tracing::info!("Service configurations sent to Flutter UI");
    }
}

#[async_trait]
impl Notifiable<RequestServiceStatus> for PbMapperActor {
    async fn notify(&mut self, msg: RequestServiceStatus, _: &Context<Self>) {
        tracing::info!("Received request for service status: {}", msg.service_key);

        let (status, message) = self.calculate_service_status(&msg.service_key).await;

        ServiceStatusResponse {
            service_key: msg.service_key,
            status,
            message,
        }
        .send_signal_to_dart();
    }
}

#[async_trait]
impl Notifiable<DeleteServiceConfigRequest> for PbMapperActor {
    async fn notify(&mut self, msg: DeleteServiceConfigRequest, _: &Context<Self>) {
        tracing::info!(
            "Received request to delete service config: {}",
            msg.service_key
        );

        // Stop service if it's running
        if let Some(handle) = self.service_handles.remove(&msg.service_key) {
            handle.abort();
        }

        // Remove from registered services
        self.registered_services
            .write()
            .await
            .remove(&msg.service_key);

        // Delete configuration from file
        if let Err(e) = self.delete_service_config(&msg.service_key) {
            tracing::error!(
                "Failed to delete service config for {}: {}",
                msg.service_key,
                e
            );
        } else {
            tracing::info!(
                "Service config for {} deleted successfully",
                msg.service_key
            );
        }
    }
}

#[async_trait]
impl Notifiable<RequestClientConfigs> for PbMapperActor {
    async fn notify(&mut self, _msg: RequestClientConfigs, _: &Context<Self>) {
        tracing::info!("Received request for client configs");

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

        // Sort by created_at to ensure consistent ordering
        client_infos.sort_by_key(|info| info.created_at_ms);

        ClientConfigsUpdate {
            clients: client_infos,
        }
        .send_signal_to_dart();
    }
}

#[async_trait]
impl Notifiable<RequestClientStatus> for PbMapperActor {
    async fn notify(&mut self, msg: RequestClientStatus, _: &Context<Self>) {
        tracing::info!("Received request for client status: {}", msg.service_key);

        let (status, message) = self.calculate_client_status(&msg.service_key).await;

        ClientStatusResponse {
            service_key: msg.service_key,
            status,
            message,
        }
        .send_signal_to_dart();
    }
}

#[async_trait]
impl Notifiable<DeleteClientConfigRequest> for PbMapperActor {
    async fn notify(&mut self, msg: DeleteClientConfigRequest, _: &Context<Self>) {
        tracing::info!(
            "Received request to delete client config: {}",
            msg.service_key
        );

        // Stop client if it's running
        if let Some(handle) = self.client_handles.remove(&msg.service_key) {
            handle.abort();
        }

        // Delete configuration from file
        if let Err(e) = self.delete_client_config(&msg.service_key) {
            tracing::error!(
                "Failed to delete client config for {}: {}",
                msg.service_key,
                e
            );
        } else {
            tracing::info!("Client config for {} deleted successfully", msg.service_key);
        }
    }
}

impl PbMapperActor {
    fn get_config_file_path(&self) -> PathBuf {
        // Use the same directory logic as get_config_dir
        let config_dir = Self::get_config_dir(&self.app_directory_path);

        // Create directory if it doesn't exist
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

    fn load_config(&self) -> Result<AppConfig, String> {
        let config_path = self.get_config_file_path();
        if config_path.exists() {
            let contents = fs::read_to_string(config_path).map_err(|e| e.to_string())?;
            let config: AppConfig = serde_json::from_str(&contents).map_err(|e| e.to_string())?;
            Ok(config)
        } else {
            // Return default config if file doesn't exist
            Ok(AppConfig::default())
        }
    }

    fn save_config(&self) -> Result<(), String> {
        let config_path = self.get_config_file_path();
        let contents = serde_json::to_string_pretty(&self.config).map_err(|e| e.to_string())?;
        fs::write(config_path, contents).map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn send_config_status(&self) {
        ConfigStatusUpdate {
            server_address: self.config.server_address.clone(),
            keep_alive_enabled: self.config.keep_alive_enabled,
        }
        .send_signal_to_dart();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_file_path_generation() {
        let config_path = PbMapperActor::get_config_dir(&None);

        // Verify the path is not empty and ends with config.json
        assert!(!config_path.as_os_str().is_empty());
        assert!(config_path.file_name().unwrap() == "config.json");

        // Print the path for manual verification on different platforms
        println!("Config file path: {:?}", config_path);

        // Test that the parent directory exists or can be created
        if let Some(parent) = config_path.parent() {
            assert!(parent.exists() || std::fs::create_dir_all(parent).is_ok());
        }
    }

    #[test]
    fn test_config_serialization() {
        let config = AppConfig {
            server_address: "test:9999".to_string(),
            keep_alive_enabled: false,
        };

        // Test JSON serialization
        let json_str = serde_json::to_string_pretty(&config).unwrap();
        println!("Config JSON: {}", json_str);

        // Test JSON deserialization
        let loaded_config: AppConfig = serde_json::from_str(&json_str).unwrap();
        assert_eq!(loaded_config.server_address, config.server_address);
        assert_eq!(loaded_config.keep_alive_enabled, config.keep_alive_enabled);
    }

    #[test]
    fn test_config_load_save_integration() {
        // Test complete save/load cycle
        let test_config = AppConfig {
            server_address: "test.example.com:8888".to_string(),
            keep_alive_enabled: false,
        };

        // Create a temporary config path for testing
        let temp_dir = std::env::temp_dir();
        let temp_config_path = temp_dir.join("test_pb_mapper_config.json");

        // Save config
        let json_content = serde_json::to_string_pretty(&test_config).unwrap();
        std::fs::write(&temp_config_path, json_content).unwrap();

        // Load config
        let loaded_content = std::fs::read_to_string(&temp_config_path).unwrap();
        let loaded_config: AppConfig = serde_json::from_str(&loaded_content).unwrap();

        // Verify
        assert_eq!(loaded_config.server_address, test_config.server_address);
        assert_eq!(
            loaded_config.keep_alive_enabled,
            test_config.keep_alive_enabled
        );

        // Cleanup
        let _ = std::fs::remove_file(&temp_config_path);

        println!("Config save/load integration test passed!");
    }
}
