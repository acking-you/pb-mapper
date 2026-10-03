//! The session object: [`Client`], its request types, and the tunnel spawner.
//!
//! Everything a caller starts goes through here. `register` and `connect` are
//! the same shape — resolve the local endpoint, open a status channel, spawn the
//! transport's worker — so that lifecycle lives in one place (`TunnelWorker`)
//! and each call site is left with only the worker it invokes.

use crate::diagnostics::{Diagnostics, RecoveryPhase};
use crate::endpoint::RelayEndpoint;
use std::sync::{Arc, RwLock};

use pb_mapper_core::checksum::{Credential, parse_credential};
use pb_mapper_core::config::{ResolvedAddrs, control_io_timeout, resolve_addrs_async};
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uni_stream::stream::{
    TcpListenerProvider, TcpStreamProvider, UdpListenerProvider, UdpStreamProvider,
};

use snafu::ResultExt;

use super::Error;
use super::admin::Admin;
use super::error::{AddressSnafu, ConnectSnafu, Result, StatusSnafu};
use super::handle::{Connection, LiveTunnel, Registration};
use super::types::{RemoteId, ServiceConnection, Transport, TunnelStatus};
use crate::client::run_client_side_cli_recovering;
use crate::client::status::get_status_with_credential;
use crate::server::{ServerTunnelOptions, StatusCallback, run_server_side_cli_recovering};

/// Configuration for a [`Client`] session.
#[derive(Clone)]
pub struct ClientConfig {
    /// Relay address (`host:port`).
    pub server: String,
    /// Administrator key (32 printable bytes) or a `pbmt1_` temporary credential.
    pub credential: String,
    pub keep_alive: bool,
    /// Administrator-only target namespace. Temporary credentials always use
    /// their own key id when this is `None`.
    pub namespace: Option<u64>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("server", &self.server)
            .field("credential", &"[redacted]")
            .field("keep_alive", &self.keep_alive)
            .field("namespace", &self.namespace)
            .finish()
    }
}

/// Register a local TCP/UDP service with the relay.
#[derive(Clone, Debug)]
pub struct RegisterRequest {
    pub key: String,
    pub local_addr: String,
    pub transport: Transport,
    pub codec: bool,
    pub force_namespace: bool,
}

/// Subscribe to a registered service and listen locally.
#[derive(Clone, Debug)]
pub struct ConnectRequest {
    pub key: String,
    pub local_addr: String,
    pub transport: Transport,
}

pub(crate) struct ClientInner {
    pub(crate) server: String,
    pub(crate) endpoint: RelayEndpoint,
    pub(crate) credential: RwLock<Credential>,
    pub(crate) keep_alive: bool,
    pub(crate) namespace: Option<u64>,
}

/// Session against one deployed relay.
///
/// The credential is per-client, not process-global. Two clients in the same
/// process may use different keys.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<ClientInner>,
}

impl Client {
    pub fn new(config: ClientConfig) -> Result<Self> {
        if config.server.trim().is_empty() {
            return Err(Error::invalid_config("server address is required"));
        }
        if config.credential.trim().is_empty() {
            return Err(Error::invalid_config("credential is required"));
        }
        let credential =
            parse_credential(config.credential.trim()).map_err(Error::invalid_config)?;
        Ok(Self::from_credential(
            config.server,
            credential,
            config.keep_alive,
            config.namespace,
        ))
    }

    /// Build a client from an already-parsed credential.
    pub fn from_credential(
        server: impl Into<String>,
        credential: Credential,
        keep_alive: bool,
        namespace: Option<u64>,
    ) -> Self {
        let server = server.into();
        let endpoint = RelayEndpoint::shared(&server);
        Self {
            inner: Arc::new(ClientInner {
                server,
                endpoint,
                credential: RwLock::new(credential),
                keep_alive,
                namespace,
            }),
        }
    }

    /// Notify workers after a host network/resume event. This does not mark services healthy.
    pub fn notify_network_change(&self) {
        self.inner.endpoint.notify_network_change();
    }

    pub fn server(&self) -> &str {
        &self.inner.server
    }

    pub fn namespace(&self) -> Option<u64> {
        self.inner.namespace
    }

    pub(crate) fn credential(&self) -> Credential {
        *self
            .inner
            .credential
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Administrator RPCs. Fails locally when the session is not an admin key.
    pub fn admin(&self) -> Result<Admin> {
        if !self.credential().is_admin() {
            return Err(Error::NotAdministrator);
        }
        Ok(Admin {
            inner: Arc::clone(&self.inner),
        })
    }

    /// Publish a local service on the relay under `request.key`.
    ///
    /// Returns as soon as the worker is spawned; the handle's
    /// [`Registration::wait_ready`] is what waits for the relay to accept it.
    pub async fn register(&self, request: RegisterRequest) -> Result<Registration> {
        if request.key.trim().is_empty() {
            return Err(Error::invalid_config("service key is required"));
        }
        let options = ServerTunnelOptions {
            need_codec: request.codec,
            is_datagram: request.transport.is_datagram(),
            keep_alive: self.inner.keep_alive,
            namespace: self.inner.namespace,
            force_namespace: request.force_namespace,
        };
        let worker = self
            .prepare_worker(&request.key, &request.local_addr)
            .await?;
        let credential = self.credential();
        let handle = match request.transport {
            Transport::Tcp => worker.spawn(move |context| {
                run_server_side_cli_recovering::<TcpStreamProvider>(
                    context.local_addr,
                    context.remote_addr,
                    context.key,
                    options,
                    Some(context.status_callback),
                    Some(credential),
                    context.shutdown,
                    context.diagnostics,
                )
            }),
            Transport::Udp => worker.spawn(move |context| {
                run_server_side_cli_recovering::<UdpStreamProvider>(
                    context.local_addr,
                    context.remote_addr,
                    context.key,
                    options,
                    Some(context.status_callback),
                    Some(credential),
                    context.shutdown,
                    context.diagnostics,
                )
            }),
        };
        Ok(Registration::new(handle, request.key))
    }

    /// Subscribe to a registered service and forward it from a local listener.
    ///
    /// Returns as soon as the worker is spawned; the handle's
    /// [`Connection::wait_ready`] is what waits for the local listener to bind
    /// and the relay to confirm the service.
    pub async fn connect(&self, request: ConnectRequest) -> Result<Connection> {
        if request.key.trim().is_empty() {
            return Err(Error::invalid_config("service key is required"));
        }
        let worker = self
            .prepare_worker(&request.key, &request.local_addr)
            .await?;
        let credential = self.credential();
        let keep_alive = self.inner.keep_alive;
        let namespace = self.inner.namespace;
        let handle = match request.transport {
            Transport::Tcp => worker.spawn(move |context| {
                run_client_side_cli_recovering::<TcpListenerProvider>(
                    context.local_addr,
                    context.remote_addr,
                    context.key,
                    keep_alive,
                    namespace,
                    Some(context.status_callback),
                    Some(credential),
                    context.shutdown,
                    context.diagnostics,
                )
            }),
            Transport::Udp => worker.spawn(move |context| {
                run_client_side_cli_recovering::<UdpListenerProvider>(
                    context.local_addr,
                    context.remote_addr,
                    context.key,
                    keep_alive,
                    namespace,
                    Some(context.status_callback),
                    Some(credential),
                    context.shutdown,
                    context.diagnostics,
                )
            }),
        };
        Ok(Connection::new(handle, request.key))
    }

    /// Service keys visible to this credential's namespace.
    pub async fn list_keys(&self) -> Result<Vec<String>> {
        match self.status_request(PbConnStatusReq::Keys).await? {
            PbConnStatusResp::Keys(keys) => Ok(keys),
            other => Err(Error::protocol(format!(
                "expected keys status, got {other:?}"
            ))),
        }
    }

    pub async fn service_status(&self, key: impl Into<String>) -> Result<Vec<ServiceConnection>> {
        let key = key.into();
        match self
            .status_request(PbConnStatusReq::Service { key })
            .await?
        {
            PbConnStatusResp::Service { connections, .. } => Ok(connections
                .into_iter()
                .map(ServiceConnection::from)
                .collect()),
            other => Err(Error::protocol(format!(
                "expected service status, got {other:?}"
            ))),
        }
    }

    pub async fn remote_id(&self) -> Result<RemoteId> {
        RemoteId::from_status(self.status_request(PbConnStatusReq::RemoteId).await?)
    }

    /// Resolve the local endpoint and set up a recovering relay worker.
    ///
    /// Shared by `register` and `connect`: the two differ only in which worker
    /// they hand the context to. Relay DNS remains retryable inside that worker.
    async fn prepare_worker(&self, key: &str, local_addr: &str) -> Result<TunnelWorker> {
        if !self.inner.endpoint.validate() {
            return Err(Error::invalid_config(
                "relay must be host:port or an IP socket address",
            ));
        }
        let local_addr = resolve(local_addr).await?;
        let remote_addr = self.inner.endpoint.clone();
        let (status_tx, status_rx) = watch::channel(TunnelStatus::Starting);
        Ok(TunnelWorker {
            local_addr,
            remote_addr,
            key: Arc::from(key),
            shutdown: CancellationToken::new(),
            status_tx,
            status_rx,
            diagnostics: Diagnostics::default(),
        })
    }

    async fn status_request(&self, request: PbConnStatusReq) -> Result<PbConnStatusResp> {
        let credential = self.credential();
        // Queries share admission with recovery. A stalled status consumer must
        // not occupy that budget for the legacy 30-second control I/O timeout.
        let io_timeout = control_io_timeout().min(std::time::Duration::from_secs(5));
        let endpoint = &self.inner.endpoint;
        let result = tokio::time::timeout(io_timeout, async {
            let _permit = endpoint.control_permit().await;
            let addresses = endpoint.addresses().await.context(ConnectSnafu {
                addr: self.inner.server.clone(),
            })?;
            let mut stream = crate::addr::connect_tcp(&addresses)
                .await
                .context(ConnectSnafu {
                    addr: self.inner.server.clone(),
                })?;
            get_status_with_credential(&mut stream, request, self.inner.namespace, &credential)
                .await
                .context(StatusSnafu)
        })
        .await
        .map_err(|_| Error::TimedOut {
            timeout: io_timeout,
        })?;
        if result.is_ok() {
            endpoint.protocol_succeeded();
        }
        result
    }
}

/// Resolve one endpoint to every address it names.
///
/// The list, not just its first entry: a worker handed the whole list dials each
/// candidate in turn, so a relay hostname with several records survives one of
/// them being unreachable.
async fn resolve(addr: &str) -> Result<ResolvedAddrs> {
    resolve_addrs_async(addr)
        .await
        .context(AddressSnafu { addr })
}

/// Mark a finished worker as `Stopped`, unless it already reported why it will
/// never come up. The status is a watch channel, so it keeps only the newest
/// value: overwriting a `Failed(reason)` that the worker set on its way out
/// would replace the only description of a permanent rejection the caller ever
/// gets, leaving `wait_ready` to report a bare stop instead.
fn settle_stopped(tx: &watch::Sender<TunnelStatus>) {
    tx.send_if_modified(|status| match status {
        TunnelStatus::Failed(_) => false,
        _ => {
            *status = TunnelStatus::Stopped;
            true
        }
    });
}

/// Bridge the worker's string status callback onto the handle's watch channel.
///
/// `StatusCallback` and `ClientStatusCallback` are the same boxed closure type,
/// so both tunnel directions share this.
fn watch_callback(tx: watch::Sender<TunnelStatus>) -> StatusCallback {
    Box::new(move |status: &str| {
        let _ = tx.send(TunnelStatus::from_callback(status));
    })
}

/// What a spawned tunnel worker is given: its resolved endpoints, its key, the
/// callback that publishes its status, and the token that stops it.
struct WorkerContext {
    local_addr: ResolvedAddrs,
    remote_addr: RelayEndpoint,
    key: Arc<str>,
    status_callback: StatusCallback,
    shutdown: CancellationToken,
    diagnostics: Diagnostics,
}

/// A tunnel resolved and wired up, waiting only for the transport-specific
/// worker that will drive it.
///
/// Register and connect, over TCP and UDP, are four workers with four different
/// signatures but one lifecycle: spawn, publish status, settle as `Stopped`.
/// This owns that lifecycle so each call site is left with just its own call.
struct TunnelWorker {
    local_addr: ResolvedAddrs,
    remote_addr: RelayEndpoint,
    key: Arc<str>,
    shutdown: CancellationToken,
    status_tx: watch::Sender<TunnelStatus>,
    status_rx: watch::Receiver<TunnelStatus>,
    diagnostics: Diagnostics,
}

impl TunnelWorker {
    fn spawn<F, Fut>(self, start: F) -> LiveTunnel
    where
        F: FnOnce(WorkerContext) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let Self {
            local_addr,
            remote_addr,
            key,
            shutdown,
            status_tx,
            status_rx,
            diagnostics,
        } = self;
        let worker_shutdown = shutdown.clone();
        let worker_diagnostics = diagnostics.clone();
        let endpoint = remote_addr.clone();
        let join = tokio::spawn(async move {
            start(WorkerContext {
                local_addr,
                remote_addr,
                key,
                status_callback: watch_callback(status_tx.clone()),
                shutdown: worker_shutdown,
                diagnostics: worker_diagnostics.clone(),
            })
            .await;
            worker_diagnostics.phase(RecoveryPhase::Stopped);
            settle_stopped(&status_tx);
        });
        LiveTunnel::new(shutdown, join, status_rx, endpoint, diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_debug_redacts_credentials() {
        let config = ClientConfig {
            server: "localhost:7666".into(),
            credential: "0123456789abcdefghijklmnopqrstuv".into(),
            keep_alive: false,
            namespace: None,
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains(&config.credential));
        assert!(debug.contains("[redacted]"));
    }

    #[test]
    fn admin_requires_administrator_credential() {
        let admin = Client::from_credential(
            "127.0.0.1:7666",
            Credential::Admin(*b"0123456789abcdefghijklmnopqrstuv"),
            false,
            None,
        );
        assert!(admin.admin().is_ok());

        let temporary = Client::from_credential(
            "127.0.0.1:7666",
            Credential::Temporary {
                key_id: 1,
                key: [0_u8; 32],
            },
            false,
            None,
        );
        assert!(matches!(temporary.admin(), Err(Error::NotAdministrator)));
    }
}
