//! The client side of pb-mapper: both ends of a tunnel, `register` and
//! `connect`, status and the administrator's calls, under the [`sdk`] module.
//!
//! The SDK to depend on is the `pb-mapper` crate, which re-exports [`sdk`]
//! and turns on what every entry point defaults to: a local network first,
//! then a direct path, then the relay. This crate alone is relay-only, as the
//! layering intends. Direct paths need the `p2p` feature: without it
//! `PathPolicy::default()` is `RelayOnly`, `RegisterOptions::default()`
//! refuses direct sessions, and asking for either is a configuration error.
//! With it, a path within a local network runs over QUIC, or over TCP where
//! the dialing end's `ConnectOptions::lan_transport` asks for it and the
//! publisher's `RegisterOptions::lan_tcp` serves it; a path across the
//! internet is always QUIC. `relay-quic` adds QUIC as the relay carrier, and
//! `port-mapping` the router mappings of `PortMapping`.

pub mod client;
pub mod sdk;
pub mod server;

mod addr;
mod diagnostics;
mod endpoint;
mod network;
mod recovery;

#[cfg(feature = "p2p")]
mod direct;

mod relay;
