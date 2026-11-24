//! How much the helper's pipes hold, and its service relays, at once. A
//! session's answers, and the units and other records its worker pushes
//! between them, cross two pipes: from the worker to the service on the
//! worker's private pipe, and on to the app on the helper pipe.
//! `examples/pipe_relay_bench.rs` includes this file, so it measures what
//! ships.

/// What a helper pipe lets wait in each direction (its in and out buffer
/// sizes): a write that fits beside what already waits completes at once,
/// before its reader takes it, while one that does not waits until the reader
/// has made room. Windows holds what waits in nonpaged pool; each pipe has at
/// most 8 instances.
pub(crate) const PIPE_QUOTA: u32 = 256 * 1024;

/// The service's relay buffer toward the app, which carries the worker's
/// answers and its pushes. While a write to the app is in flight the relay
/// reads on into the rest of it, so the next write takes all it gathered.
pub(crate) const RELAY_TO_APP: usize = 256 * 1024;

/// The service's relay buffer toward the worker, which carries the app's
/// requests, each a JSON record of at most 8 KiB.
pub(crate) const RELAY_TO_WORKER: usize = 64 * 1024;
