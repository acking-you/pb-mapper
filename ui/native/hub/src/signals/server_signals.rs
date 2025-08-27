use rinf::{DartSignal, RustSignal, SignalPiece};
use serde::{Deserialize, Serialize};

// Server Management Signals
#[derive(Deserialize, DartSignal)]
pub struct StartServerRequest {
    pub port: u16,
    pub enable_ipv6: bool,
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
    pub server_address: String,
    pub enable_keep_alive: bool,
}

#[derive(Serialize, RustSignal)]
pub struct ServiceStatusUpdate {
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

// Client Connection Signals
#[derive(Deserialize, DartSignal)]
pub struct ConnectServiceRequest {
    pub service_key: String,
    pub local_address: String,
    pub protocol: String,
    pub server_address: String,
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

// Configuration Signals
#[derive(Deserialize, DartSignal)]
pub struct RequestConfig;

#[derive(Deserialize, DartSignal)]
pub struct UpdateConfigRequest {
    pub server_address: String,
    pub enable_keep_alive: bool,
}

#[derive(Serialize, RustSignal)]
pub struct ConfigStatusUpdate {
    pub server_address: String,
    pub keep_alive_enabled: bool,
}

// Log Signals
#[derive(Serialize, RustSignal)]
pub struct LogMessage {
    pub level: String,
    pub message: String,
    pub timestamp: u64,
}
