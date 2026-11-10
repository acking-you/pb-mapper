//! Shared serialisation for tests that mutate process-global credential state,
//! and a scratch directory for this crate's own file tests.

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

/// A directory of its own under the system temporary directory, removed on
/// drop, for this crate's file tests.
#[cfg(test)]
pub(crate) struct Scratch(pub(crate) std::path::PathBuf);

#[cfg(test)]
impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        let suffix: u64 = rand::RngExt::random(&mut rand::rng());
        let path = std::env::temp_dir().join(format!(
            "pb-mapper-core-{name}-{}-{suffix:016x}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// The names directly inside it, sorted.
    pub(crate) fn entries(&self) -> Vec<std::ffi::OsString> {
        let mut names = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
