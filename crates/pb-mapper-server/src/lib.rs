//! Relay server domain model and module wiring.
//!
//! ```text
//! TCP / QUIC listener -> connection authentication/dispatch -> ManagerTask queue
//!                                                         -> routing runtime
//! registered control connection <------ ConnTask ----+------> subscriber
//! ```
//!
//! `connection` owns per-socket protocol/authentication concerns, while `runtime`
//! serializes global routing maps and quotas. `task` holds the messages between
//! them, `routing` the routing model the runtime keeps, and `service_key` the
//! namespaced service keys both use. Service-side and client-side tunnel loops
//! remain isolated in `server` and `client`.

mod admin;
mod client;
mod connection;
pub mod direct;
mod error;
// Moved here from `common`: the routing runtime is its only caller.
pub mod manager;
#[cfg(feature = "relay-quic")]
mod quic;
mod routing;
mod runtime;
mod server;
mod service_key;
mod status;
mod task;

use std::net::SocketAddr;

pub use direct::DirectServerConfig;
pub use routing::{ImutableKey, ServerConnHealth, ServerConnInfo, ServerConnMap};
pub use runtime::{
    run_server, run_server_on_listener, run_server_on_listener_with_direct,
    run_server_on_listener_with_options, run_server_with_auth_config, run_server_with_shutdown,
};
pub use task::{ConnTask, ManagerTask};

// Paths the connection handlers and the direct coordinator name items by.
pub(crate) use error::Result;
pub(crate) use routing::server_conn_idle_timeout;
pub(crate) use service_key::{
    compose_service_key, scoped_service_key, split_scoped_service_key, validate_service_name,
};
pub(crate) use task::{ConnTaskReceiver, ConnTaskSender, ManagerTaskSender};

#[derive(Debug, Clone)]
pub struct ServerStatusInfo {
    pub active_connections: u32,
    pub registered_services: u32,
    pub uptime_seconds: u64,
}

/// Optional QUIC listener, separate from the direct STUN UDP port.
#[derive(Clone, Debug)]
pub struct QuicServerConfig {
    pub listen: SocketAddr,
    /// Zero advertises the bound port; set for a NAT port mapping.
    pub advertise_port: u16,
}

#[derive(Clone, Debug, Default)]
pub struct RelayServerOptions {
    pub direct: Option<DirectServerConfig>,
    pub quic: Option<QuicServerConfig>,
}
