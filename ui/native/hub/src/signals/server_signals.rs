use rinf::{DartSignal, RustSignal, SignalPiece};
use serde::{Deserialize, Serialize};

// Server Management Signals
#[derive(Deserialize, DartSignal)]
pub struct StartServerRequest {
    pub port: u16,
    pub enable_keep_alive: bool,
}

#[derive(Deserialize, DartSignal)]
pub struct StopServerRequest;

#[derive(Serialize, RustSignal)]
pub struct ServerStatusUpdate {
    pub status: String,
    pub active_connections: u32,
    pub uptime: u64,
}

// Service Registration Signals
#[derive(Deserialize, DartSignal)]
pub struct RegisterServiceRequest {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_encryption: bool,
    pub enable_keep_alive: bool,
}

#[derive(Deserialize, DartSignal)]
pub struct UnregisterServiceRequest {
    pub service_key: String,
}

#[derive(Serialize, RustSignal)]
pub struct ServiceStatusUpdate {
    pub message: String,
}

#[derive(Serialize, RustSignal)]
pub struct ServiceRegistrationStatusUpdate {
    pub service_key: String,
    pub status: String, // "running", "retrying", "failed", "stopped"
    pub message: String,
}

#[derive(Serialize, RustSignal)]
pub struct RegisteredServicesUpdate {
    pub services: Vec<RegisteredServiceInfo>,
}

#[derive(Serialize, SignalPiece)]
pub struct RegisteredServiceInfo {
    pub service_key: String,
    pub protocol: String,
    pub local_address: String,
    pub status: String,
}

// Service Configuration Management Signals
#[derive(Deserialize, DartSignal)]
pub struct RequestServiceConfigs;

#[derive(Serialize, RustSignal)]
pub struct ServiceConfigsUpdate {
    pub services: Vec<ServiceConfigInfo>,
}

#[derive(Serialize, SignalPiece)]
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

#[derive(Deserialize, DartSignal)]
pub struct RequestServiceStatus {
    pub service_key: String,
}

#[derive(Serialize, RustSignal)]
pub struct ServiceStatusResponse {
    pub service_key: String,
    pub status: String,
    pub message: String,
}

#[derive(Deserialize, DartSignal)]
pub struct DeleteServiceConfigRequest {
    pub service_key: String,
}

// Client Connection Signals
#[derive(Deserialize, DartSignal)]
pub struct ConnectServiceRequest {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub enable_keep_alive: bool,
}

#[derive(Deserialize, DartSignal)]
pub struct DisconnectServiceRequest {
    pub service_key: String,
}

#[derive(Serialize, RustSignal)]
pub struct ClientConnectionStatus {
    pub status: String,
}

#[derive(Serialize, RustSignal)]
pub struct ActiveConnectionsUpdate {
    pub connections: Vec<ActiveConnectionInfo>,
}

#[derive(Serialize, SignalPiece)]
pub struct ActiveConnectionInfo {
    pub service_key: String,
    pub client_id: String,
    pub status: String,
}

// Client Configuration Management Signals
#[derive(Deserialize, DartSignal)]
pub struct RequestClientConfigs;

#[derive(Serialize, RustSignal)]
pub struct ClientConfigsUpdate {
    pub clients: Vec<ClientConfigInfo>,
}

#[derive(Serialize, SignalPiece)]
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

#[derive(Deserialize, DartSignal)]
pub struct RequestClientStatus {
    pub service_key: String,
}

#[derive(Serialize, RustSignal)]
pub struct ClientStatusResponse {
    pub service_key: String,
    pub status: String,
    pub message: String,
}

#[derive(Deserialize, DartSignal)]
pub struct DeleteClientConfigRequest {
    pub service_key: String,
}

// Configuration Signals
#[derive(Deserialize, DartSignal)]
pub struct RequestConfig;

#[derive(Deserialize, DartSignal)]
pub struct UpdateConfigRequest {
    pub server_address: String,
    pub enable_keep_alive: bool,
}

#[derive(Deserialize, DartSignal)]
pub struct SaveConfigRequest;

#[derive(Deserialize, DartSignal)]
pub struct LoadConfigRequest;

#[derive(Serialize, RustSignal)]
pub struct ConfigStatusUpdate {
    pub server_address: String,
    pub keep_alive_enabled: bool,
}

#[derive(Serialize, RustSignal)]
pub struct ConfigSaveResult {
    pub success: bool,
    pub message: String,
}

// Log Signals
#[derive(Serialize, RustSignal)]
pub struct LogMessage {
    pub level: String,
    pub message: String,
    pub timestamp: u64,
}

// Status Monitoring Signals
#[derive(Deserialize, DartSignal)]
pub struct RequestServerStatus;

#[derive(Deserialize, DartSignal)]
pub struct RequestLocalServerStatus;

#[derive(Serialize, RustSignal)]
pub struct LocalServerStatusUpdate {
    pub is_running: bool,
    pub active_connections: u32,
    pub registered_services: u32,
    pub uptime_seconds: u64,
}

#[derive(Serialize, RustSignal)]
pub struct ServerStatusDetailUpdate {
    pub server_available: bool,
    pub registered_services: Vec<String>,
    pub server_map: String,
    pub active_connections: String,
    pub idle_connections: String,
}
