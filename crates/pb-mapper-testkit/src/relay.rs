//! A `pb-mapper server` on a reserved loopback port, with its own auth state.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use pb_mapper_auth::{
    ADMIN_KEY_ID, AuthContext, AuthRuntime, IssuedTemporaryKey, KeyId, TemporaryKeyMetadata,
};
use pb_mapper_client::client::status::get_status_with_credential;
use pb_mapper_core::checksum::{Credential, parse_credential};
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper_server::run_server_on_listener;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

use crate::tunnel::{Tunnel, TunnelSpec};
use crate::{READY_TIMEOUT, admin_key_bytes, auth_config, init_test_env};

/// One relay: a listener, an [`AuthRuntime`], and the server loop over both.
///
/// The runtime is kept, not moved into the server, for two reasons. Tests need it
/// to issue, renew, and revoke credentials without going through the admin wire
/// protocol; and dropping every clone closes the actor's command channel, which
/// makes it cancel every outstanding lease — so a live relay has to hold one.
pub struct Relay {
    addr: SocketAddr,
    auth: AuthRuntime,
    admin: AuthContext,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
    state_dir: PathBuf,
}

impl Relay {
    /// Start a relay. `label` only shows up in the state directory's name.
    pub async fn start(label: &str) -> Self {
        init_test_env();
        let config = auth_config(label);
        let _ = std::fs::remove_dir_all(&config.state_dir);
        let state_dir = config.state_dir.clone();

        // Pre-bound, so nothing can take the port between reserving it and the
        // relay's own bind.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let auth = AuthRuntime::start(admin_key_bytes(), config).await.unwrap();
        let admin = auth
            .authenticate_presented(ADMIN_KEY_ID, &admin_key_bytes())
            .unwrap();

        let shutdown = CancellationToken::new();
        let server_auth = auth.clone();
        let server_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            if let Err(error) =
                run_server_on_listener(listener, server_shutdown, None, false, server_auth).await
            {
                tracing::error!("relay stopped: {error}");
            }
        });

        Self {
            addr,
            auth,
            admin,
            shutdown,
            task,
            state_dir,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The administrator context, for auth calls this type does not wrap.
    pub fn admin_context(&self) -> &AuthContext {
        &self.admin
    }

    pub fn auth(&self) -> &AuthRuntime {
        &self.auth
    }

    /// Issue a temporary credential and return it ready to authenticate with.
    ///
    /// The key ID doubles as the credential's namespace, which is what makes
    /// `namespace: None` on the register and connect paths resolve to the
    /// credential's own namespace.
    pub async fn issue_credential(&self, ttl: Duration, label: &str) -> (KeyId, Credential) {
        let issued = self.issue(ttl, label).await;
        let credential = parse_credential(&issued.credential).unwrap();
        (issued.metadata.key_id, credential)
    }

    /// Issue a temporary credential, keeping the lifecycle metadata.
    pub async fn issue(&self, ttl: Duration, label: &str) -> IssuedTemporaryKey {
        self.auth
            .issue(&self.admin, ttl, Some(label.to_string()))
            .await
            .unwrap()
    }

    /// Extend a credential's lifetime. The credential text does not change.
    pub async fn renew(&self, key_id: KeyId, ttl: Duration) -> IssuedTemporaryKey {
        self.auth.renew(&self.admin, key_id, ttl).await.unwrap()
    }

    /// Revoke a credential, cancelling its lease and every connection under it.
    pub async fn revoke(&self, key_id: KeyId) -> TemporaryKeyMetadata {
        self.auth.revoke(&self.admin, key_id).await.unwrap()
    }

    /// Build a tunnel — echo server, `register`, and `connect` — against this relay.
    ///
    /// Returns once traffic has made the full round trip.
    pub async fn start_tunnel(&self, spec: TunnelSpec) -> Tunnel {
        Tunnel::start(self, spec).await
    }

    /// Poll the `Keys` status until `register` has published `service_key`.
    pub async fn wait_for_registration(
        &self,
        service_key: &str,
        credential: Credential,
        namespace: Option<u64>,
    ) {
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut last_error = String::from("no attempt completed");
        while Instant::now() < deadline {
            match self.registered_keys(credential, namespace).await {
                Ok(keys) => {
                    if keys.iter().any(|key| key == service_key) {
                        return;
                    }
                    last_error = format!("relay reports keys {keys:?}");
                }
                Err(error) => last_error = error,
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("`{service_key}` was never registered: {last_error}");
    }

    /// The service names visible to `credential` in `namespace`.
    ///
    /// `Keys` is namespace-scoped and answers with bare service names, so a
    /// temporary credential sees exactly its own namespace with no extra work.
    pub async fn registered_keys(
        &self,
        credential: Credential,
        namespace: Option<u64>,
    ) -> Result<Vec<String>, String> {
        let mut stream = timeout(Duration::from_secs(1), TcpStream::connect(self.addr))
            .await
            .map_err(|_| "status connect timed out".to_string())?
            .map_err(|error| format!("status connect failed: {error}"))?;
        let response = timeout(
            Duration::from_secs(1),
            get_status_with_credential(&mut stream, PbConnStatusReq::Keys, namespace, &credential),
        )
        .await
        .map_err(|_| "status request timed out".to_string())?
        .map_err(|error| format!("status request failed: {error}"))?;
        match response {
            PbConnStatusResp::Keys(keys) => Ok(keys),
            other => Err(format!("unexpected status response: {other:?}")),
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}
