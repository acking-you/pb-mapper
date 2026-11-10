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
//!
//! ```text
//! stored        the bytes on disk: config.json, services.json, clients.json
//! views         the JSON the window reads
//! tunnel        a tunnel's setup, its SDK handle, and its view
//! configuration loading and saving what `stored` describes
//! runtime       the embedded relay, and committing and stopping tunnels
//! status        the window's reads, and the questions to the relay
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Instant, SystemTime};

use parking_lot::Mutex as SyncMutex;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use pb_mapper_auth::AuthRuntime;
use pb_mapper_client::sdk::{Client, PortMapping};
use pb_mapper_core::checksum::{Credential, get_process_credential};
use pb_mapper_server::ServerStatusInfo;

use crate::error::CtlError;

mod configuration;
mod runtime;
mod status;
mod stored;
mod tunnel;
mod views;

pub use status::{force_refresh_server_status, get_server_status_detail, get_service_conns};
pub use stored::{
    AppConfig, ClientConfigData, ClientConfigStore, DirectOptions, ServiceConfigData,
    ServiceConfigStore,
};
pub(crate) use tunnel::TunnelHandle;
use tunnel::TunnelRuntime;
pub use tunnel::{connect_service, register_service};
pub use views::{
    ClientConfigInfo, ClientStatusResponse, LocalServerStatus, PathInfo, RelayTunnels,
    ServerStatusDetail, ServiceConfigInfo, ServiceConnInfo, ServiceStatusResponse,
};

/// The port mapping a C caller names: 0 off, 1 when needed, 2 always.
pub fn port_mapping_from_c(raw: std::ffi::c_int) -> Result<PortMapping, CtlError> {
    match raw {
        0 => Ok(PortMapping::Off),
        1 => Ok(PortMapping::Auto),
        2 => Ok(PortMapping::Always),
        _ => Err(CtlError::invalid_argument(format!(
            "port_mapping must be 0, 1 or 2, got {raw}"
        ))),
    }
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
    quic_offer: Option<(AppConfig, Option<u16>)>,
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
    /// Keys currently being set up. See [`KeyClaim`](tunnel::KeyClaim). Separate sets because a
    /// key can legitimately be registered and connected to at the same time.
    registering: Arc<SyncMutex<HashSet<String>>>,
    connecting: Arc<SyncMutex<HashSet<String>>>,
}

/// This computer's credential, or `INVALID_ARGUMENT` saying it has none,
/// which no retry can mend.
pub(crate) fn process_credential() -> Result<Credential, CtlError> {
    get_process_credential().map_err(|error| {
        CtlError::invalid_argument(format!("This computer has no usable credential: {error}"))
    })
}

#[cfg(test)]
// The crate denies these so a panic never reaches a Flutter caller through the
// FFI boundary. In a test a panic *is* the failure report.
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests;
