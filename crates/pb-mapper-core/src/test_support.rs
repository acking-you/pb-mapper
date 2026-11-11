//! What the tests of this crate and the crates above it share: the
//! administrator key they authenticate with, a temporary path of their own,
//! and the lock around the process credential.
//!
//! Everything here is unconditionally `pub` rather than `#[cfg(test)]`: a
//! test-only item is not visible to another crate's tests, because each crate
//! compiles its own test configuration. Crates below `pb-mapper-testkit`
//! cannot use the testkit, which depends on them, so this is their home.

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// The administrator key every test relay and test client uses, as the bytes
/// a relay starts with.
pub const TEST_ADMIN_KEY_BYTES: [u8; 32] = *b"0123456789abcdefghijklmnopqrstuv";

/// [`TEST_ADMIN_KEY_BYTES`] as the text a key file or the process credential
/// holds.
pub const TEST_ADMIN_KEY: &str = match std::str::from_utf8(&TEST_ADMIN_KEY_BYTES) {
    Ok(key) => key,
    Err(_) => panic!("the test administrator key is ASCII"),
};

/// Serialises tests that set or clear the process credential.
///
/// The credential is process-global, so two such tests running on different
/// runner threads would see each other's writes. Every test that calls
/// `set_process_msg_header_key` — in this crate and in the auth, protocol, and
/// server crates — takes this first.
///
/// It lives here, next to the state it guards.
pub static PROCESS_CREDENTIAL_TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// A path under the system temporary directory that no other test, in this
/// process or another, uses. It is not created: a test makes a directory or a
/// file there, and whichever it is goes when this drops, also when the test
/// fails partway.
#[derive(Debug)]
pub struct TempPath(PathBuf);

impl TempPath {
    /// `pb-mapper-{label}-{pid}-{16 random hex digits}`.
    pub fn new(label: &str) -> Self {
        let suffix: u64 = rand::RngExt::random(&mut rand::rng());
        Self(std::env::temp_dir().join(format!(
            "pb-mapper-{label}-{}-{suffix:016x}",
            std::process::id()
        )))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Deref for TempPath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        // Best effort: what is left is only a stray entry in the temporary
        // directory, and a destructor must not panic.
        if std::fs::remove_dir_all(&self.0).is_err() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}
