use std::sync::{Arc, RwLock};

use pb_mapper_core::checksum::{Credential, parse_credential};
use pb_mapper_core::config::{control_io_timeout, get_sockaddr_async};
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};
use tokio::net::TcpStream;
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
use crate::client::status::get_status_with_credential;
use crate::client::{ClientStatusCallback, run_client_side_cli_with_shutdown};
use crate::server::{ServerTunnelOptions, StatusCallback, run_server_side_cli_with_shutdown};

/// Configuration for a [`Client`] session.
#[derive(Clone, Debug)]
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
        Self {
            inner: Arc::new(ClientInner {
                server: server.into(),
                credential: RwLock::new(credential),
                keep_alive,
                namespace,
            }),
        }
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

    pub async fn register(&self, request: RegisterRequest) -> Result<Registration> {
        if request.key.trim().is_empty() {
            return Err(Error::invalid_config("service key is required"));
        }
        let local_addr = resolve(&request.local_addr).await?;
        let remote_addr = resolve(&self.inner.server).await?;
        let credential = self.credential();
        let key = request.key.clone();
        let options = ServerTunnelOptions {
            need_codec: request.codec,
            is_datagram: request.transport.is_datagram(),
            keep_alive: self.inner.keep_alive,
            namespace: self.inner.namespace,
            force_namespace: request.force_namespace,
        };

        let (status_tx, status_rx) = watch::channel(TunnelStatus::Starting);
        let shutdown = CancellationToken::new();
        let worker_shutdown = shutdown.clone();
        let worker_key: std::sync::Arc<str> = key.clone().into();
        let handle = match request.transport {
            Transport::Tcp => tokio::spawn(async move {
                let callback = watch_callback(status_tx.clone());
                run_server_side_cli_with_shutdown::<TcpStreamProvider, _>(
                    local_addr,
                    remote_addr,
                    worker_key,
                    options,
                    Some(callback),
                    credential,
                    worker_shutdown,
                )
                .await;
                settle_stopped(&status_tx);
            }),
            Transport::Udp => tokio::spawn(async move {
                let callback = watch_callback(status_tx.clone());
                run_server_side_cli_with_shutdown::<UdpStreamProvider, _>(
                    local_addr,
                    remote_addr,
                    worker_key,
                    options,
                    Some(callback),
                    credential,
                    worker_shutdown,
                )
                .await;
                settle_stopped(&status_tx);
            }),
        };

        Ok(Registration::new(
            LiveTunnel::new(shutdown, handle, status_rx),
            key,
        ))
    }

    pub async fn connect(&self, request: ConnectRequest) -> Result<Connection> {
        if request.key.trim().is_empty() {
            return Err(Error::invalid_config("service key is required"));
        }
        let local_addr = resolve(&request.local_addr).await?;
        let remote_addr = resolve(&self.inner.server).await?;
        let credential = self.credential();
        let key = request.key.clone();
        let keep_alive = self.inner.keep_alive;
        let namespace = self.inner.namespace;

        let (status_tx, status_rx) = watch::channel(TunnelStatus::Starting);
        let shutdown = CancellationToken::new();
        let worker_shutdown = shutdown.clone();
        let worker_key: std::sync::Arc<str> = key.clone().into();
        let handle = match request.transport {
            Transport::Tcp => tokio::spawn(async move {
                let callback = client_watch_callback(status_tx.clone());
                run_client_side_cli_with_shutdown::<TcpListenerProvider, _>(
                    local_addr,
                    remote_addr,
                    worker_key,
                    keep_alive,
                    namespace,
                    Some(callback),
                    Some(credential),
                    worker_shutdown,
                )
                .await;
                settle_stopped(&status_tx);
            }),
            Transport::Udp => tokio::spawn(async move {
                let callback = client_watch_callback(status_tx.clone());
                run_client_side_cli_with_shutdown::<UdpListenerProvider, _>(
                    local_addr,
                    remote_addr,
                    worker_key,
                    keep_alive,
                    namespace,
                    Some(callback),
                    Some(credential),
                    worker_shutdown,
                )
                .await;
                settle_stopped(&status_tx);
            }),
        };

        Ok(Connection::new(
            LiveTunnel::new(shutdown, handle, status_rx),
            key,
        ))
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

    async fn status_request(&self, request: PbConnStatusReq) -> Result<PbConnStatusResp> {
        let addr = resolve(&self.inner.server).await?;
        let credential = self.credential();
        // The connect is inside the timeout, not just the exchange that follows it.
        // A relay that drops SYNs silently leaves `TcpStream::connect` waiting on
        // the OS timeout — minutes — so the SDK's own bound has to cover it, the
        // way the administrator path already does.
        let io_timeout = control_io_timeout();
        let mut stream = match tokio::time::timeout(io_timeout, TcpStream::connect(addr)).await {
            Ok(result) => result.context(ConnectSnafu {
                addr: addr.to_string(),
            })?,
            Err(_) => {
                return Err(Error::TimedOut {
                    timeout: io_timeout,
                });
            }
        };
        get_status_with_credential(&mut stream, request, self.inner.namespace, &credential)
            .await
            .context(StatusSnafu)
    }
}

async fn resolve(addr: &str) -> Result<std::net::SocketAddr> {
    get_sockaddr_async(addr)
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

fn watch_callback(tx: watch::Sender<TunnelStatus>) -> StatusCallback {
    Box::new(move |status: &str| {
        let _ = tx.send(TunnelStatus::from_callback(status));
    })
}

fn client_watch_callback(tx: watch::Sender<TunnelStatus>) -> ClientStatusCallback {
    Box::new(move |status: &str| {
        let _ = tx.send(TunnelStatus::from_callback(status));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
