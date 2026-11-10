//! What this crate's tests share: a temporary directory that removes itself,
//! a handle rooted in one, and reading an export's answer.

use std::ffi::{CStr, c_char};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::handle::PbMapperHandle;
use crate::state::PbMapperState;

/// A new, empty directory of its own under the system's temporary one,
/// removed with everything in it when this is dropped, a panicking test's
/// included.
pub(crate) struct TempRoot(PathBuf);

impl TempRoot {
    pub(crate) fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "pb-mapper-ffi-{name}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create a temporary directory");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A handle as `pb_mapper_create` and `pb_mapper_set_app_dir(root)` leave
/// it. Building its state writes the process credential, so the caller
/// holds `PROCESS_CREDENTIAL_TEST_LOCK`.
pub(crate) fn test_handle(root: &Path) -> PbMapperHandle {
    PbMapperHandle {
        desktop: Default::default(),
        tunnel_events: Default::default(),
        runtime: tokio::runtime::Runtime::new().expect("start a runtime"),
        state: Arc::new(tokio::sync::Mutex::new(PbMapperState::new(Some(
            root.display().to_string(),
        )))),
        control_shutdown: Default::default(),
    }
}

/// An export's answer as JSON, freed as the caller of the export must.
pub(crate) fn take_reply(text: *mut c_char) -> serde_json::Value {
    assert!(!text.is_null(), "an answer must serialise");
    // SAFETY: a non-null answer is a NUL-terminated string this library
    // allocated, read here before it is freed.
    let value = serde_json::from_slice(unsafe { CStr::from_ptr(text) }.to_bytes())
        .expect("answers are JSON");
    // SAFETY: it came from this library and is freed once.
    unsafe { crate::logging::pb_mapper_free_string(text) };
    value
}
