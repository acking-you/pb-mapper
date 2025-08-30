use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use messages::prelude::{Actor, Address, Context, Notifiable};
use rinf::{DartSignal, RustSignal};
use robius_directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::task::{JoinHandle, JoinSet};
use tokio_with_wasm::alias as tokio;

use crate::signals::*;
use pb_mapper::common::config::{PB_MAPPER_KEEP_ALIVE, get_pb_mapper_server, get_sockaddr};
use pb_mapper::common::listener::{TcpListenerProvider, UdpListenerProvider};
use pb_mapper::common::message::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper::common::stream::got_one_socket_addr;
use pb_mapper::common::stream::{TcpStreamProvider, UdpStreamProvider};
use pb_mapper::local::client::run_client_side_cli;
use pb_mapper::local::client::status::get_status;
use pb_mapper::local::server::run_server_side_cli;
use pb_mapper::pb_server::run_server;
use pb_mapper::utils::addr::each_addr;

#[derive(Serialize, Deserialize, Clone)]
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
    server_start_time: Option<SystemTime>,
    registered_services: Arc<RwLock<HashMap<String, ServiceInfo>>>,
    active_connections: Arc<RwLock<HashMap<String, ConnectionInfo>>>,
    service_handles: HashMap<String, JoinHandle<()>>,
    client_handles: HashMap<String, JoinHandle<()>>,
    config: AppConfig,
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
    pub fn new(self_addr: Address<Self>) -> Self {
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

        // Spawn periodic status updates
        owned_tasks.spawn(Self::periodic_status_updates(self_addr));

        // Load or create default configuration
        let config = Self::load_config().unwrap_or_default();

        Self {
            server_handle: None,
            server_start_time: None,
            registered_services: Arc::new(RwLock::new(HashMap::new())),
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            service_handles: HashMap::new(),
            client_handles: HashMap::new(),
            config,
            _owned_tasks: owned_tasks,
        }
    }

    /// Fetch real status data from pb-mapper server
    async fn fetch_real_status(&self) -> Result<(Vec<String>, RemoteIdData), String> {
        let server_addr = self.config.server_address.clone();

        // First get keys (list of registered services)
        let services = match self.get_server_keys(&server_addr).await {
            Ok(keys) => keys,
            Err(e) => return Err(format!("Failed to get server keys: {}", e)),
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
            .map_err(|e| format!("Invalid server address {}: {}", server_addr, e))?;

        let mut stream = each_addr(socket_addr, TcpStream::connect)
            .await
            .map_err(|e| format!("Failed to connect to server: {}", e))?;

        let status_resp = get_status(&mut stream, PbConnStatusReq::Keys)
            .await
            .map_err(|e| format!("Failed to get status: {}", e))?;

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
            .map_err(|e| format!("Invalid server address {}: {}", server_addr, e))?;

        let mut stream = each_addr(socket_addr, TcpStream::connect)
            .await
            .map_err(|e| format!("Failed to connect to server: {}", e))?;

        let status_resp = get_status(&mut stream, PbConnStatusReq::RemoteId)
            .await
            .map_err(|e| format!("Failed to get status: {}", e))?;

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
        while let Some(_) = receiver.recv().await {
            let _ = self_addr.notify(RequestServerStatus).await;
        }
    }

    async fn periodic_status_updates(mut self_addr: Address<Self>) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            // Send internal status update trigger
            let _ = self_addr.notify(InternalStatusUpdate).await;
            // Also request server status periodically
            let _ = self_addr.notify(RequestServerStatus).await;
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

        let handle = tokio::spawn(async move {
            run_server((ip_addr, port)).await;
        });

        self.server_handle = Some(handle);
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
        if let Some(handle) = self.server_handle.take() {
            handle.abort();
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
            return Err(format!("Service '{}' is already registered", service_key));
        }

        if enable_keep_alive {
            unsafe {
                std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
            }
        }

        let local_sock_addr =
            get_sockaddr(&local_address).map_err(|e| format!("Invalid local address: {}", e))?;
        let remote_sock_addr = get_pb_mapper_server(Some(&self.config.server_address))
            .map_err(|e| format!("Invalid server address: {}", e))?;

        tracing::info!(
            "Registering service '{}' with protocol {}",
            service_key,
            protocol
        );

        let key_clone = service_key.clone();
        let handle = if protocol.to_uppercase() == "TCP" {
            tokio::spawn(async move {
                run_server_side_cli::<TcpStreamProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    enable_encryption,
                )
                .await;
            })
        } else {
            tokio::spawn(async move {
                run_server_side_cli::<UdpStreamProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    enable_encryption,
                )
                .await;
            })
        };

        self.service_handles.insert(service_key.clone(), handle);

        let service_info = ServiceInfo {
            service_key: service_key.clone(),
            protocol,
            local_address,
            status: "Registered".to_string(),
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
            message: format!("Service '{}' registered successfully", service_key),
        }
        .send_signal_to_dart();

        tracing::info!("Service '{}' registered successfully", service_key);
        Ok(())
    }

    async fn unregister_service_internal(&mut self, service_key: String) -> Result<(), String> {
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
                message: format!("Service '{}' unregistered successfully", service_key),
            }
            .send_signal_to_dart();

            tracing::info!("Service '{}' unregistered successfully", service_key);
            Ok(())
        } else {
            Err(format!("Service '{}' is not registered", service_key))
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
            return Err(format!(
                "Client for service '{}' is already connected",
                service_key
            ));
        }

        if enable_keep_alive {
            unsafe {
                std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
            }
        }

        let local_sock_addr =
            get_sockaddr(&local_address).map_err(|e| format!("Invalid local address: {}", e))?;
        let remote_sock_addr = get_pb_mapper_server(Some(&self.config.server_address))
            .map_err(|e| format!("Invalid server address: {}", e))?;

        tracing::info!(
            "Connecting to service '{}' with protocol {}",
            service_key,
            protocol
        );

        let key_clone = service_key.clone();
        let handle = if protocol.to_uppercase() == "TCP" {
            tokio::spawn(async move {
                run_client_side_cli::<TcpListenerProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                )
                .await;
            })
        } else {
            tokio::spawn(async move {
                run_client_side_cli::<UdpListenerProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                )
                .await;
            })
        };

        self.client_handles.insert(service_key.clone(), handle);

        let connection_info = ConnectionInfo {
            service_key: service_key.clone(),
            client_id: format!("client-{}", service_key),
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
            status: format!("Connected to service '{}'", service_key),
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
                status: format!("Disconnected from service '{}'", service_key),
            }
            .send_signal_to_dart();

            tracing::info!("Disconnected from service '{}'", service_key);
            Ok(())
        } else {
            Err(format!("Service '{}' is not connected", service_key))
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
                status: format!("Error: {}", e),
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
                msg.service_key,
                msg.local_address,
                msg.protocol,
                msg.enable_encryption,
                msg.enable_keep_alive,
            )
            .await
        {
            tracing::error!("Failed to register service: {}", e);
            ServiceStatusUpdate {
                message: format!("Error: {}", e),
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
                message: format!("Error: {}", e),
            }
            .send_signal_to_dart();
        }
    }
}

#[async_trait]
impl Notifiable<ConnectServiceRequest> for PbMapperActor {
    async fn notify(&mut self, msg: ConnectServiceRequest, _: &Context<Self>) {
        if let Err(e) = self
            .connect_service_internal(
                msg.service_key,
                msg.local_address,
                msg.protocol,
                msg.enable_keep_alive,
            )
            .await
        {
            tracing::error!("Failed to connect to service: {}", e);
            ClientConnectionStatus {
                status: format!("Error: {}", e),
            }
            .send_signal_to_dart();
        }
    }
}

#[async_trait]
impl Notifiable<DisconnectServiceRequest> for PbMapperActor {
    async fn notify(&mut self, msg: DisconnectServiceRequest, _: &Context<Self>) {
        if let Err(e) = self.disconnect_service_internal(msg.service_key).await {
            tracing::error!("Failed to disconnect service: {}", e);
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
impl Notifiable<InternalStatusUpdate> for PbMapperActor {
    async fn notify(&mut self, _msg: InternalStatusUpdate, _: &Context<Self>) {
        // Handle periodic status updates
        self.send_status_update().await;
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
                    message: format!("Failed to save configuration: {}", e),
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

impl PbMapperActor {
    fn get_config_file_path() -> PathBuf {
        // Use robius-directories for cross-platform config directory support
        // This crate supports Linux, Windows, macOS, Android, and other platforms
        let config_dir = if let Some(proj_dirs) = ProjectDirs::from("dev", "pb-mapper", "pb-mapper")
        {
            // Use project-specific config directory (recommended approach)
            proj_dirs.config_dir().to_path_buf()
        } else if let Some(user_dirs) = UserDirs::new() {
            // Fallback to user home directory with .config subdirectory
            user_dirs.home_dir().join(".config").join("pb-mapper")
        } else {
            // Final fallback to current directory
            tracing::warn!("Could not determine config directory, using current directory");
            PathBuf::from(".")
        };

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

    fn load_config() -> Result<AppConfig, String> {
        let config_path = Self::get_config_file_path();
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
        let config_path = Self::get_config_file_path();
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
        let config_path = PbMapperActor::get_config_file_path();

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
