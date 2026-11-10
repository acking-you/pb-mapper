//! Regression cases that need a test binary of their own: they write the
//! environment at run time (`support::EnvOverrides`), and this binary writes
//! the process credential itself (`support::auth_config`).
//!
//! - `cli`: cases that run the `pb-mapper` binary;
//! - `relay`: the relay's behavior against hand-written frames;
//! - `client`: `register` and `connect` against fake relays.
//!
//! Nothing here uses what in the testkit runs `init_test_env` (`Relay`,
//! `Tunnel`): that would also install the testkit's tracing in this binary.

// An integration test: a failed `unwrap` is a failed test, which is the report
// this target exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod cli;
mod client;
mod relay;
mod support;
