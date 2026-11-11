//! The process credential and the authentication state every relay of the
//! `regression*` binaries runs on. `regression_client` and `regression_cli`
//! include this file by path.

use std::path::PathBuf;
use std::sync::LazyLock;

use pb_mapper_auth::AuthConfig;
use pb_mapper_core::checksum::set_process_msg_header_key;
use pb_mapper_testkit::TEST_ADMIN_KEY;

/// The process credential, written once per binary before any relay or client
/// here reads it: the hand-written frames in these cases checksum with it.
///
/// `AuthRuntime::from_process` and `AuthRuntime::start` write it again at every
/// relay start, which these binaries cannot avoid; every write is the same key.
fn init_process_credential() {
    static CREDENTIAL: LazyLock<()> =
        LazyLock::new(|| set_process_msg_header_key(Some(TEST_ADMIN_KEY)).unwrap());
    *CREDENTIAL
}

/// A fresh authentication state directory, with the process credential in place.
pub(crate) fn auth_config() -> AuthConfig {
    init_process_credential();
    pb_mapper_testkit::auth_config("regression")
}

/// An authentication state directory, removed when the case ends, passed or not.
pub(crate) struct StateDir(pub(crate) PathBuf);

impl Drop for StateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
