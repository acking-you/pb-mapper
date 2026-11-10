//! End-to-end cases that can share one process.
//!
//! Cargo builds and links each `tests/*.rs` file as a binary of its own, so a
//! case lives here unless it changes state the whole process shares:
//!
//! - `regression`, `regression_client` and `regression_cli` set the process
//!   credential themselves, the second sets client timings for its whole
//!   process, and the third starts processes, which on Windows inherit every
//!   socket open at the time (see `regression/main.rs`);
//! - `lease_reclaim` shortens the lease of every relay in its process;
//! - `network_recovery` sets the health-check interval and announces network
//!   changes;
//! - `quic_retired_carrier` announces a network change, which retires every
//!   QUIC carrier in the process;
//! - `quic_relay` rotates the root key, which rewrites the process credential
//!   that the framed drivers here checksum with.

// An integration test: a failed `unwrap` is a failed test, which is the report
// this target exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod bound_listener;
mod data_compatibility;
#[cfg(feature = "p2p")]
mod direct;
#[cfg(all(feature = "p2p", feature = "relay-quic"))]
mod key_namespaces;
mod sdk;
mod temporary_credential;
mod tunnel_matrix;
