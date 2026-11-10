//! The relay's own behavior against hand-written frames (`relay`), each case
//! on an in-process relay of its own.
//!
//! Three binaries share this file's modules, each for what it needs from its
//! process:
//!
//! - `regression` (this one) leaves the environment as it found it and
//!   starts no process, so a relay closing a connection is seen to close it;
//! - `regression_client` sets the client timings it needs once, before any
//!   case starts: `register` and `connect` against fake relays;
//! - `regression_cli` runs the `pb-mapper` binary.
//!
//! On Windows a child process inherits every socket open in its parent when
//! it starts (Tokio's sockets come from mio 1.1, which marks none
//! non-inheritable), and holds it until it exits: a connection its owner
//! dropped stays open, then resets, and a dropped listener keeps its port.
//! A case here that waits for a close would see neither, so no case here
//! starts a process.
//!
//! The process credential is this binary's (`credential::auth_config`), and
//! nothing here uses what in the testkit runs `init_test_env` (`Relay`,
//! `Tunnel`): that would also install the testkit's tracing in this binary.

// An integration test: a failed `unwrap` is a failed test, which is the report
// this target exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod credential;
mod fixtures;
mod relay;
