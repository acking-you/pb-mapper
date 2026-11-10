//! A `pb-mapper server` on a reserved loopback port, with its own auth state.

use std::net::SocketAddr;
use std::time::Duration;

use pb_mapper_auth::{
    ADMIN_KEY_ID, AuthConfig, AuthContext, AuthRuntime, IssuedTemporaryKey, KeyId,
    TemporaryKeyMetadata,
};
use pb_mapper_client::client::status::get_status_with_credential;
use pb_mapper_core::checksum::{Credential, parse_credential};
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};
use pb_mapper_server::{
    DirectServerConfig, QuicServerConfig, RelayServerOptions, run_server_on_listener_with_options,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

use crate::poll::poll_until;
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
    quic: Option<QuicServerConfig>,
    auth: Option<AuthRuntime>,
    admin: AuthContext,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
    /// Kept whole, so a restart reopens the same state under the same limits.
    config: AuthConfig,
}

impl Relay {
    /// Start a relay. `label` only shows up in the state directory's name.
    pub async fn start(label: &str) -> Self {
        Self::start_with_direct(label, None).await
    }

    pub async fn start_with_direct(label: &str, direct: Option<DirectServerConfig>) -> Self {
        Self::start_with_options(label, RelayServerOptions { direct, quic: None }).await
    }

    pub async fn start_with_quic(label: &str) -> Self {
        Self::start_with_options(
            label,
            RelayServerOptions {
                direct: None,
                quic: Some(QuicServerConfig {
                    listen: "127.0.0.1:0".parse().unwrap(),
                    advertise_port: 0,
                }),
            },
        )
        .await
    }

    pub async fn start_with_options(label: &str, options: RelayServerOptions) -> Self {
        init_test_env();
        let config = auth_config(label);
        let _ = std::fs::remove_dir_all(&config.state_dir);

        // Pre-bound, so nothing can take the port between reserving it and the
        // relay's own bind.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let auth = AuthRuntime::start(admin_key_bytes(), config.clone())
            .await
            .unwrap();
        let admin = authenticate_admin(&auth);
        let shutdown = CancellationToken::new();
        let quic = options.quic.clone();
        let task = spawn_server(listener, shutdown.clone(), auth.clone(), options);

        Self {
            addr,
            quic,
            auth: Some(auth),
            admin,
            shutdown,
            task,
            config,
        }
    }

    pub async fn stop_server(&mut self) {
        self.shutdown.cancel();
        let _ = (&mut self.task).await;
    }

    /// Reopen the same persisted auth state and TCP address after a shutdown.
    pub async fn restart_server(&mut self, direct: Option<DirectServerConfig>) {
        let quic = self.quic.clone();
        self.restart_server_with_options(RelayServerOptions { direct, quic })
            .await;
    }

    /// [`Self::restart_server`], with listeners of its own: a relay brought
    /// back without the QUIC it had, say.
    pub async fn restart_server_with_options(&mut self, options: RelayServerOptions) {
        self.quic = options.quic.clone();
        if !self.task.is_finished() {
            self.stop_server().await;
        }
        if let Some(auth) = self.auth.take() {
            auth.shutdown_actor().await;
        }
        // Every handle holds the state directory's lock, and the stopped
        // relay's last ones go with tasks that finish a few milliseconds
        // after it returns: wait for the lock, not for a fixed time.
        let deadline = Instant::now() + Duration::from_secs(5);
        let auth = loop {
            match AuthRuntime::start(admin_key_bytes(), self.config.clone()).await {
                Err(failure)
                    if failure.code == "auth_state_locked" && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                result => break result.unwrap(),
            }
        };
        self.admin = authenticate_admin(&auth);
        let listener = TcpListener::bind(self.addr).await.unwrap();
        self.shutdown = CancellationToken::new();
        self.task = spawn_server(listener, self.shutdown.clone(), auth.clone(), options);
        self.auth = Some(auth);
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The administrator context, for auth calls this type does not wrap.
    pub fn admin_context(&self) -> &AuthContext {
        &self.admin
    }

    pub fn auth(&self) -> &AuthRuntime {
        self.auth
            .as_ref()
            .expect("relay authentication is initialized")
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
        self.auth()
            .issue(&self.admin, ttl, Some(label.to_string()))
            .await
            .unwrap()
    }

    /// Reserve a namespace that keys can then be issued into.
    pub async fn allocate_namespace(&self) -> u64 {
        self.auth().allocate_namespace(&self.admin).await.unwrap()
    }

    /// Issue a temporary credential into `namespace`, ready to authenticate with.
    pub async fn issue_credential_in(
        &self,
        namespace: u64,
        ttl: Duration,
        label: &str,
    ) -> (KeyId, Credential) {
        let issued = self
            .auth()
            .issue_in_namespace(&self.admin, namespace, ttl, Some(label.to_string()))
            .await
            .unwrap();
        (
            issued.metadata.key_id,
            parse_credential(&issued.credential).unwrap(),
        )
    }

    /// Set a credential's lifetime. The credential text does not change.
    pub async fn renew(&self, key_id: KeyId, ttl: Duration) -> IssuedTemporaryKey {
        self.auth().renew(&self.admin, key_id, ttl).await.unwrap()
    }

    /// Revoke a credential, cancelling its lease and every connection under it.
    pub async fn revoke(&self, key_id: KeyId) -> TemporaryKeyMetadata {
        self.auth().revoke(&self.admin, key_id).await.unwrap()
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
        let registered = poll_until(
            READY_TIMEOUT,
            Duration::from_millis(20),
            move || async move {
                let keys = self.registered_keys(credential, namespace).await?;
                if keys.iter().any(|key| key == service_key) {
                    Ok(())
                } else {
                    Err(format!("relay reports keys {keys:?}"))
                }
            },
        )
        .await;
        if let Err(last_error) = registered {
            panic!("`{service_key}` was never registered: {last_error}");
        }
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
        let _ = std::fs::remove_dir_all(&self.config.state_dir);
    }
}

/// The server loop over `listener`, logging the error it stops on, if any.
fn spawn_server(
    listener: TcpListener,
    shutdown: CancellationToken,
    auth: AuthRuntime,
    options: RelayServerOptions,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) =
            run_server_on_listener_with_options(listener, shutdown, None, false, auth, options)
                .await
        {
            tracing::error!("relay stopped: {error}");
        }
    })
}

/// The administrator's context on `auth`, which a relay keeps for its helpers.
fn authenticate_admin(auth: &AuthRuntime) -> AuthContext {
    auth.authenticate_presented(ADMIN_KEY_ID, &admin_key_bytes())
        .unwrap()
}
