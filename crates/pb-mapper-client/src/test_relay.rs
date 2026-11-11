//! A loopback relay for this crate's own tests, which cannot use
//! `pb-mapper-testkit`: the testkit depends on this crate.

use std::net::SocketAddr;

use pb_mapper_auth::{AuthConfig, AuthRuntime};
use pb_mapper_core::checksum::{Credential, gen_random_key};
use pb_mapper_core::test_support::TempPath;
use pb_mapper_server::DirectServerConfig;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

/// A relay on 127.0.0.1 with an administrator key and a state directory of
/// its own. Dropping it stops the relay, then removes the directory, also
/// when the test panics.
pub(crate) struct TestRelay {
    pub(crate) credential: Credential,
    pub(crate) addr: SocketAddr,
    // Before `_state`: fields drop in order, and the relay goes first.
    _task: AbortOnDropHandle<()>,
    _state: TempPath,
}

impl TestRelay {
    /// Start one, coordinating direct sessions where `direct` says. `label`
    /// names its state directory.
    pub(crate) async fn start(label: &str, direct: Option<DirectServerConfig>) -> Self {
        let credential = Credential::Admin(gen_random_key());
        let state = TempPath::new(label);
        let auth = AuthRuntime::start(
            *credential.key(),
            AuthConfig {
                state_dir: state.to_path_buf(),
                ..AuthConfig::default()
            },
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = pb_mapper_server::run_server_on_listener_with_direct(
            listener,
            CancellationToken::new(),
            None,
            false,
            auth,
            direct,
        );
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let _ = server.await;
        }));
        Self {
            credential,
            addr,
            _task: task,
            _state: state,
        }
    }
}
