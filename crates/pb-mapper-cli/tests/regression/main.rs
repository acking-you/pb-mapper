//! The relay's own behavior against hand-written frames (`relay`), each case
//! on an in-process relay of its own; one case runs a real `connect` and
//! `register` against it, in this process.
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
//! On Windows a child process inherits every inheritable socket open in its
//! parent when it starts, and holds it until it exits: a connection its owner
//! dropped stays open, then resets, and a dropped listener keeps its port.
//! Tokio's sockets were inheritable until mio 1.2.1; the lock file now has a
//! later mio, but a case here that waits for a close must not depend on what
//! every dependency's sockets do, so no case here starts a process.
//!
//! The process credential is this binary's (`credential::auth_config`), and
//! nothing here uses what in the testkit runs `init_test_env` (`Relay`,
//! `Tunnel`): that would also install the testkit's tracing in this binary.

// An integration test: a failed `unwrap` is a failed test, which is the report
// this target exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../e2e/bound_listener.rs"]
mod bound_listener;
mod credential;
mod fixtures;
mod relay;
