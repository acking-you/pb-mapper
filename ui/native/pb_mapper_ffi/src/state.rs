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

use parking_lot::Mutex as SyncMutex;

use pb_mapper_client::sdk::{Client, RelayTransport};
use pb_mapper_core::checksum::{Credential, get_process_credential};

use crate::error::{CtlError, sdk_error};

mod configuration;
#[cfg(any(target_os = "macos", test))]
mod container;
mod runtime;
mod status;
mod stored;
mod tunnel;
mod views;

use runtime::EmbeddedRelay;
use status::LocalStatusCache;
pub use status::{force_refresh_server_status, get_server_status_detail, get_service_conns};
pub(crate) use stored::map_when_needed;
pub use stored::{
    AppConfig, ClientConfigData, ClientConfigStore, DirectOptions, ServiceConfigData,
    ServiceConfigStore,
};
pub(crate) use tunnel::TunnelHandle;
use tunnel::TunnelRuntime;
pub use tunnel::{ConnectSpec, ShareSpec, connect_service, register_service};
pub use views::{
    ClientConfigInfo, LocalServerStatus, PathInfo, RelayTunnels, ServerStatusDetail,
    ServiceConfigInfo, ServiceConnInfo, TunnelState, TunnelStatusResponse,
};

pub struct PbMapperState {
    embedded_relay: Option<EmbeddedRelay>,
    /// The client this window's own questions to the relay go through, kept
    /// for as long as the relay settings it was made from stay as they are:
    /// a QUIC connection, and the relay's resolved address, then outlive
    /// one question. Tunnels share both with it.
    relay_client: SyncMutex<Option<(AppConfig, Client)>>,
    /// What the relay said about QUIC for these settings: asked again only by
    /// a forced check (Settings' Test) or a change of settings, not every
    /// periodic one, since a relay from before QUIC logs the question.
    quic_offer: Option<(AppConfig, Option<u16>)>,
    service_runtime: HashMap<String, TunnelRuntime>,
    client_runtime: HashMap<String, TunnelRuntime>,
    config: AppConfig,
    config_dir: PathBuf,
    app_directory_path: Option<String>,
    /// Shared with the background read that refreshes it.
    local_server_status: Arc<SyncMutex<LocalStatusCache>>,
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

/// A client of the relay at `server` over `transport`, as `credential`, in
/// `namespace` when that is not the credential's own. Clients of one relay
/// and carrier share the process's connections to it.
pub(crate) fn relay_client_for(
    server: &str,
    credential: Credential,
    keep_alive: bool,
    namespace: Option<u64>,
    transport: RelayTransport,
) -> Result<Client, CtlError> {
    Client::from_credential(server, credential, keep_alive, namespace)
        .with_relay_transport(transport)
        .map_err(sdk_error)
}

#[cfg(test)]
mod tests;
