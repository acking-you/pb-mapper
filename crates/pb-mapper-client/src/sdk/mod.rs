//! Library API for talking to a deployed pb-mapper relay.
//!
//! A [`Client`] holds the relay address and a credential. From there the same
//! process can register local services, subscribe to them, inspect tenant
//! status, and — with an administrator credential — drive every admin RPC.

mod admin;
mod client;
mod error;
mod handle;
mod types;

pub use admin::{
    Admin, AuthStatusInfo, ConnectionInfo, ConnectionPage, IssuedKey, KeyListPage, KeyMetadata,
    ServiceInfo, ServicePage,
};
pub use client::{Client, ClientConfig, ConnectRequest, RegisterRequest};
pub use error::{Error, Result};
pub use handle::{Connection, Registration};
pub use types::{
    ConnectOptions, LegacyProtocol, PathPolicy, RegisterOptions, RemoteId, ServiceConnection,
    Transport, TunnelStatus,
};

/// The SDK version compiled into this process.
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");
pub use crate::diagnostics::{PathDiagnostics, RecoveryFailure, RecoveryPhase, TunnelDiagnostics};
pub use pb_mapper_protocol::direct::{
    ObservedReport, SessionObservation, SessionPath, SessionState,
};
