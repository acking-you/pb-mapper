//! The session object: [`Client`].
//!
//! Everything a caller starts goes through here. `register` and `connect` are
//! the same shape — resolve the local endpoint, open a status channel, spawn the
//! transport's worker — so that lifecycle lives in one place (`TunnelWorker`,
//! in `worker`) and each call site is left with only the worker it invokes.

use crate::endpoint::RelayEndpoint;
use crate::relay::CarrierId;
use std::sync::{Arc, RwLock};

use pb_mapper_core::checksum::{Credential, parse_credential};
use pb_mapper_core::config::control_io_timeout;
use pb_mapper_protocol::command::{PbConnStatusReq, PbConnStatusResp};
use tokio::net::{TcpListener, TcpStream};
use uni_stream::stream::{TcpStreamProvider, UdpListenerProvider, UdpStreamProvider};

use snafu::ResultExt;

use super::Error;
use super::admin::Admin;
use super::error::{IoSnafu, Result, StatusSnafu};
use super::handle::{Connection, Registration};
use super::types::{
    ClientConfig, ConnectRequest, RegisterRequest, RemoteId, ServiceConnection, Transport,
};
use super::worker::TunnelWorker;
use super::{ConnectOptions, LanTransport, PathPolicy, RegisterOptions};
use crate::client::status::get_status_with_credential;
use crate::client::{
    LocalTcpListenerProvider, run_client_side_cli_recovering_with_path, run_owned_stream,
};
use crate::server::{ServerTunnelOptions, run_server_side_cli_recovering};

pub(crate) struct ClientInner {
    pub(crate) server: String,
    pub(crate) endpoint: RelayEndpoint,
    /// Shared with the clients on the other carrier made from it: one
    /// session, which a root rotation moves as a whole.
    pub(crate) credential: Arc<RwLock<Credential>>,
    pub(crate) keep_alive: bool,
    pub(crate) namespace: Option<u64>,
}

impl ClientInner {
    /// The credential this session authenticates with now.
    pub(crate) fn credential(&self) -> Credential {
        *self
            .credential
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Authenticate this session's later requests with `credential`.
    pub(crate) fn set_credential(&self, credential: Credential) {
        *self
            .credential
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = credential;
    }
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
                credential: Arc::new(RwLock::new(credential)),
                keep_alive,
                namespace,
            }),
        }
    }

    /// Select the relay carrier for subsequent work. Existing handles retain their carrier.
    /// TCP remains the default. QUIC requires the relay-quic feature and a QUIC-enabled relay.
    ///
    /// The client returned is the same session on that carrier, as a clone
    /// is: it shares this one's credential, so a root rotation through
    /// either moves both.
    pub fn with_relay_transport(&self, transport: super::RelayTransport) -> Result<Self> {
        if transport == self.relay_transport() {
            return Ok(self.clone());
        }
        let endpoint = self
            .inner
            .endpoint
            .with_transport(transport)
            .ok_or_else(|| Error::invalid_config("SDK built without relay-quic"))?;
        Ok(Self {
            inner: Arc::new(ClientInner {
                server: self.inner.server.clone(),
                endpoint,
                credential: Arc::clone(&self.inner.credential),
                keep_alive: self.inner.keep_alive,
                namespace: self.inner.namespace,
            }),
        })
    }
    pub fn relay_transport(&self) -> super::RelayTransport {
        self.inner.endpoint.transport()
    }
    /// Sample current QUIC counters without a background observer or per-packet work.
    /// Clients of the same relay and carrier in this process share one pool.
    pub async fn relay_diagnostics(&self) -> super::RelayDiagnostics {
        self.inner.endpoint.relay_diagnostics().await
    }

    /// [`Self::relay_diagnostics`] at once, then every `every` (100 ms to an
    /// hour), for as long as this client or a clone of it is kept.
    pub fn subscribe_relay_metrics(&self, every: std::time::Duration) -> RelayMetricsStream {
        RelayMetricsStream {
            client: Arc::downgrade(&self.inner),
            every: crate::diagnostics::sample_period(every),
            due: None,
        }
    }

    /// The UDP port the relay offers QUIC on, or `None` when it offers none:
    /// one started without a QUIC listener, or one from before QUIC. Asked
    /// over TCP whatever this client's carrier, so a TCP client can learn
    /// whether QUIC would work before switching to it.
    pub async fn relay_quic_port(&self) -> Result<Option<u16>> {
        let timeout = status_io_timeout();
        tokio::time::timeout(timeout, self.inner.endpoint.quic_port(self.credential()))
            .await
            .unwrap_or(Err(
                std::io::Error::from(std::io::ErrorKind::TimedOut).into()
            ))
            .map_err(|error| Error::from_connect(error, self.inner.server.clone()))
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

    /// The namespace this client's credential owns: a temporary key's id, or
    /// 0 for the administrator key. Unlike [`Self::namespace`], which an
    /// administrator sets to reach into another, it tells credentials apart:
    /// what something made through this client, such as a remembered desktop
    /// grant, is scoped to.
    pub fn credential_namespace(&self) -> u64 {
        self.credential().key_id()
    }

    pub(crate) fn credential(&self) -> Credential {
        self.inner.credential()
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

    /// Publish a local service on the relay under `request.key`, with
    /// [`RegisterOptions::default`]: direct sessions are allowed when the SDK
    /// is built with `p2p`.
    ///
    /// Returns as soon as the worker is spawned; the handle's
    /// [`Registration::wait_ready`] is what waits for the relay to accept it.
    pub async fn register(&self, request: RegisterRequest) -> Result<Registration> {
        self.register_with_options(request, RegisterOptions::default())
            .await
    }

    /// Publish with an idle deadline for each forwarded TCP data connection.
    ///
    /// Traffic in either direction resets the deadline; control connections and
    /// other registrations are unaffected. Zero disables the idle deadline.
    /// UDP requests are rejected because datagram lifetime has a separate policy.
    /// Durations that cannot be represented as a timer deadline are rejected.
    pub async fn register_with_idle_timeout(
        &self,
        request: RegisterRequest,
        idle_timeout: std::time::Duration,
    ) -> Result<Registration> {
        if request.transport.is_datagram() {
            return Err(Error::invalid_config(
                "a registration idle deadline requires TCP",
            ));
        }
        if tokio::time::Instant::now()
            .checked_add(idle_timeout)
            .is_none()
        {
            return Err(Error::invalid_config("idle timeout exceeds timer range"));
        }
        self.register_inner(
            request,
            RegisterOptions {
                idle_timeout: Some(idle_timeout),
                ..Default::default()
            },
        )
        .await
    }

    /// Publish a service with explicit options, such as
    /// `allow_direct: false` to keep every session on the relay.
    pub async fn register_with_options(
        &self,
        request: RegisterRequest,
        options: RegisterOptions,
    ) -> Result<Registration> {
        if options.allow_direct && !cfg!(feature = "p2p") {
            return Err(Error::invalid_config("built without p2p support"));
        }
        options.port_mapping.check_supported()?;
        if let Some(timeout) = options.idle_timeout
            && (request.transport.is_datagram()
                || tokio::time::Instant::now().checked_add(timeout).is_none())
        {
            return Err(Error::invalid_config(
                "idle deadline requires TCP and a representable duration",
            ));
        }
        self.register_inner(request, options).await
    }

    async fn register_inner(
        &self,
        request: RegisterRequest,
        options: RegisterOptions,
    ) -> Result<Registration> {
        let RegisterOptions {
            allow_direct,
            idle_timeout,
            port_mapping,
            lan_first,
            lan_tcp,
        } = options;
        if request.key.trim().is_empty() {
            return Err(Error::invalid_config("service key is required"));
        }
        let tunnel = ServerTunnelOptions {
            need_codec: request.codec,
            is_datagram: request.transport.is_datagram(),
            keep_alive: self.inner.keep_alive,
            namespace: self.inner.namespace,
            force_namespace: request.force_namespace,
        };
        let worker =
            TunnelWorker::prepare(&self.inner.endpoint, &request.key, &request.local_addr).await?;
        let credential = self.credential();
        let direct = worker.direct_policy(
            allow_direct,
            port_mapping,
            lan_first,
            LanTransport::Quic,
            lan_tcp,
        );
        let handle = match request.transport {
            Transport::Tcp => worker.spawn(move |context| {
                run_server_side_cli_recovering::<TcpStreamProvider>(
                    context.local_addr,
                    context.remote_addr,
                    context.key,
                    tunnel,
                    Some(context.status),
                    Some(credential),
                    context.shutdown,
                    context.diagnostics,
                    idle_timeout,
                    direct,
                )
            }),
            Transport::Udp => worker.spawn(move |context| {
                run_server_side_cli_recovering::<UdpStreamProvider>(
                    context.local_addr,
                    context.remote_addr,
                    context.key,
                    tunnel,
                    Some(context.status),
                    Some(credential),
                    context.shutdown,
                    context.diagnostics,
                    idle_timeout,
                    direct,
                )
            }),
        };
        Ok(Registration::new(handle, request.key))
    }

    /// Subscribe to a registered service and forward it from a local listener,
    /// with [`ConnectOptions::default`]: direct first with relay fallback when
    /// the SDK is built with `p2p`.
    ///
    /// Returns as soon as the worker is spawned; the handle's
    /// [`Connection::wait_ready`] is what waits for the local listener to bind
    /// and the relay to confirm the service.
    pub async fn connect(&self, request: ConnectRequest) -> Result<Connection> {
        self.connect_with_options(request, ConnectOptions::default())
            .await
    }

    /// Open one owned TCP session without reserving or exposing a forwarding port.
    /// Keep the returned connection alive for the lifetime of the stream. Setup
    /// uses the same direct commit/fallback rules as `connect`; after setup no
    /// application bytes are replayed and a closed stream is never reconnected.
    pub async fn connect_tcp(
        &self,
        key: impl Into<String>,
        options: ConnectOptions,
    ) -> Result<(TcpStream, Connection)> {
        let key = key.into();
        if key.trim().is_empty() || (options.path == PathPolicy::Auto && !cfg!(feature = "p2p")) {
            return Err(Error::invalid_config(
                "invalid service key or unavailable direct support",
            ));
        }
        options.port_mapping.check_supported()?;
        let (stream, accepted) = loopback_pair().await.context(IoSnafu)?;
        let worker = TunnelWorker::prepare(&self.inner.endpoint, &key, "127.0.0.1:0").await?;
        let credential = self.credential();
        let keep_alive = self.inner.keep_alive;
        let namespace = self.inner.namespace;
        let direct = worker.direct_policy(
            options.path == PathPolicy::Auto,
            options.port_mapping,
            options.lan_first,
            options.lan_transport,
            false,
        );
        let handle = worker.spawn(move |context| {
            run_owned_stream(
                accepted,
                context.remote_addr,
                context.key,
                keep_alive,
                namespace,
                context.status,
                credential,
                context.shutdown,
                context.diagnostics,
                direct,
            )
        });
        Ok((stream, Connection::new(handle, key)))
    }

    /// Select direct-first or the relay-only protocol for this tunnel only.
    pub async fn connect_with_options(
        &self,
        request: ConnectRequest,
        options: ConnectOptions,
    ) -> Result<Connection> {
        if options.path == PathPolicy::Auto && !cfg!(feature = "p2p") {
            return Err(Error::invalid_config("built without p2p support"));
        }
        options.port_mapping.check_supported()?;
        let is_datagram = request.transport.is_datagram();
        if request.key.trim().is_empty() {
            return Err(Error::invalid_config("service key is required"));
        }
        let worker =
            TunnelWorker::prepare(&self.inner.endpoint, &request.key, &request.local_addr).await?;
        let credential = self.credential();
        let keep_alive = self.inner.keep_alive;
        let namespace = self.inner.namespace;
        let direct = worker.direct_policy(
            options.path == PathPolicy::Auto,
            options.port_mapping,
            options.lan_first,
            options.lan_transport,
            false,
        );
        let handle = match request.transport {
            Transport::Tcp => worker.spawn(move |context| {
                run_client_side_cli_recovering_with_path::<LocalTcpListenerProvider>(
                    context.local_addr,
                    context.remote_addr,
                    context.key,
                    keep_alive,
                    namespace,
                    Some(context.status),
                    Some(credential),
                    context.shutdown,
                    context.diagnostics,
                    is_datagram,
                    direct,
                )
            }),
            Transport::Udp => worker.spawn(move |context| {
                run_client_side_cli_recovering_with_path::<UdpListenerProvider>(
                    context.local_addr,
                    context.remote_addr,
                    context.key,
                    keep_alive,
                    namespace,
                    Some(context.status),
                    Some(credential),
                    context.shutdown,
                    context.diagnostics,
                    is_datagram,
                    direct,
                )
            }),
        };
        Ok(Connection::new(handle, request.key))
    }

    /// Service keys visible to this credential's namespace.
    pub async fn list_keys(&self) -> Result<Vec<String>> {
        match self.status_request(PbConnStatusReq::Keys).await? {
            PbConnStatusResp::Keys(keys) => Ok(keys),
            other => Err(Error::unexpected("keys status", &other)),
        }
    }

    /// Endpoint observations; stale or conflicting reports do not control sessions.
    pub async fn sessions(&self) -> Result<Vec<pb_mapper_protocol::direct::SessionObservation>> {
        match self.status_request(PbConnStatusReq::Sessions).await? {
            PbConnStatusResp::Sessions(sessions) => Ok(sessions),
            other => Err(Error::unexpected("sessions status", &other)),
        }
    }

    pub async fn service_status(&self, key: impl Into<String>) -> Result<Vec<ServiceConnection>> {
        let key = key.into();
        match self
            .status_request(PbConnStatusReq::Service {
                direct_query: None,
                key,
            })
            .await?
        {
            PbConnStatusResp::Service { connections, .. } => Ok(connections
                .into_iter()
                .map(ServiceConnection::from)
                .collect()),
            other => Err(Error::unexpected("service status", &other)),
        }
    }

    pub async fn remote_id(&self) -> Result<RemoteId> {
        RemoteId::from_status(self.status_request(PbConnStatusReq::RemoteId).await?)
    }

    async fn status_request(&self, request: PbConnStatusReq) -> Result<PbConnStatusResp> {
        let credential = self.credential();
        let io_timeout = status_io_timeout();
        let endpoint = &self.inner.endpoint;
        let ask = async |carrier: &mut Option<CarrierId>| {
            let mut stream = endpoint
                .connect(credential, carrier)
                .await
                .map_err(|error| Error::from_connect(error, self.inner.server.clone()))?;
            get_status_with_credential(
                &mut stream,
                request.clone(),
                self.inner.namespace,
                &credential,
            )
            .await
            .context(StatusSnafu)
        };
        // Outside the deadline, so that a query it cuts short is charged to
        // the connection the query went over too.
        let mut carrier = None;
        let result = tokio::time::timeout(io_timeout, async {
            let _permit = endpoint.control_permit().await;
            match ask(&mut carrier).await {
                // A pooled QUIC connection whose credential the relay has
                // since cancelled takes no new streams, so the refusal never
                // arrives on it. A status query is a read: asked once more on
                // a fresh connection, it gets the relay's verdict.
                Err(error)
                    if error.is_carrier_failure()
                        && endpoint.transport() == super::RelayTransport::Quic =>
                {
                    endpoint.carrier_failed(carrier);
                    ask(&mut carrier).await
                }
                result => result,
            }
        })
        .await
        .unwrap_or(Err(Error::TimedOut {
            timeout: io_timeout,
        }));
        match &result {
            Ok(_) => endpoint.protocol_succeeded(),
            Err(error) if error.is_carrier_failure() => endpoint.carrier_failed(carrier),
            // The relay answered: the carrier works, whatever it said.
            Err(_) => {}
        }
        result
    }
}

/// A relay carrier's [`RelayDiagnostics`](super::RelayDiagnostics), sampled
/// as time passes: from [`Client::subscribe_relay_metrics`].
///
/// Each sample is taken when it is returned: nothing runs in the background
/// and nothing queues. It does not keep its client alive; once every clone
/// of that client is gone, the stream ends at its next tick.
pub struct RelayMetricsStream {
    client: std::sync::Weak<ClientInner>,
    every: std::time::Duration,
    /// When the next sample is due; the first is due at once.
    due: Option<tokio::time::Instant>,
}

impl RelayMetricsStream {
    /// The next sample: the first at once, then one a period after the
    /// last. `None` once the client is gone. Cancel-safe.
    pub async fn recv(&mut self) -> Option<super::RelayDiagnostics> {
        if let Some(due) = self.due {
            tokio::time::sleep_until(due).await;
        }
        let client = self.client.upgrade()?;
        self.due = Some(tokio::time::Instant::now() + self.every);
        Some(client.endpoint.relay_diagnostics().await)
    }
}

/// Two connected loopback TCP sockets with Nagle off: the end
/// [`Client::connect_tcp`] hands its caller, and the end its worker forwards.
async fn loopback_pair() -> std::io::Result<(TcpStream, TcpStream)> {
    // Retain the bound socket until our own connection has been accepted;
    // there is no reserve/drop/rebind race or reusable external listener.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let stream = TcpStream::connect(listener.local_addr()?).await?;
    let peer = stream.local_addr()?;
    let accepted = loop {
        let (candidate, address) = listener.accept().await?;
        if address == peer {
            break candidate;
        }
    };
    drop(listener);
    stream.set_nodelay(true)?;
    accepted.set_nodelay(true)?;
    Ok((stream, accepted))
}

/// The I/O bound of a status query. Queries share admission with recovery: a
/// stalled status consumer must not occupy that budget for the legacy
/// 30-second control I/O timeout.
fn status_io_timeout() -> std::time::Duration {
    control_io_timeout().min(std::time::Duration::from_secs(5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_core::test_support::TEST_ADMIN_KEY_BYTES;

    #[test]
    fn admin_requires_administrator_credential() {
        let admin = Client::from_credential(
            "127.0.0.1:7666",
            Credential::Admin(TEST_ADMIN_KEY_BYTES),
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

    /// A client on the other carrier is the same session, as a clone is: a
    /// root rotation through either moves both, the relay accepting only the
    /// new key from then on.
    #[cfg(feature = "relay-quic")]
    #[test]
    fn a_carrier_sibling_shares_its_clients_credential() {
        let tcp = Client::from_credential(
            "127.0.0.1:7666",
            Credential::Admin(TEST_ADMIN_KEY_BYTES),
            false,
            None,
        );
        let quic = tcp
            .with_relay_transport(super::super::RelayTransport::Quic)
            .unwrap();
        let rotated = Credential::Admin([7; 32]);
        quic.inner.set_credential(rotated);
        assert_eq!(tcp.credential(), rotated);
    }

    /// A stream whose relay refuses every connection until its setup
    /// deadline fails with why, not only with the deadline: the reason a
    /// `connect_tcp` caller gets names the refusal the operating system gave.
    #[tokio::test]
    async fn an_unreachable_relay_fails_connect_tcp_with_its_cause() {
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = closed.local_addr().unwrap();
        drop(closed);
        let refused = TcpStream::connect(relay).await.unwrap_err().to_string();
        let client = Client::from_credential(
            relay.to_string(),
            Credential::Admin(TEST_ADMIN_KEY_BYTES),
            false,
            None,
        );
        let (_stream, connection) = client
            .connect_tcp(
                "unreachable",
                ConnectOptions {
                    path: PathPolicy::RelayOnly,
                    ..ConnectOptions::default()
                },
            )
            .await
            .unwrap();
        let ready = connection
            .wait_ready_timeout(std::time::Duration::from_secs(30))
            .await;
        let Err(Error::TunnelFailed { reason }) = ready else {
            panic!("expected a failed tunnel, got {ready:?}");
        };
        assert!(reason.contains(&refused), "{reason}");
    }
}
