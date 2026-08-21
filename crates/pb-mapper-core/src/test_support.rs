//! Shared serialisation for tests that mutate process-global credential state.

/// Serialises tests that set or clear the process credential.
///
/// The credential is process-global, so two such tests running on different
/// runner threads would see each other's writes. Every test that calls
/// `set_process_msg_header_key` — in this crate and in the auth, protocol, and
/// server crates — takes this first.
///
/// It lives here, next to the state it guards, and is unconditionally `pub`
/// rather than `#[cfg(test)]`: a test-only item is not visible to another
/// crate's tests, because each crate compiles its own test configuration.
pub static PROCESS_CREDENTIAL_TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
