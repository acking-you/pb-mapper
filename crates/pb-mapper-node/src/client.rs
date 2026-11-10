//! The `Client` class: a relay address and a credential, and the tunnels,
//! status queries and administrator handle reached through it.

use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pb_mapper_client::sdk::{self, ClientConfig, ConnectRequest, RegisterRequest};

use crate::admin::Admin;
use crate::convert::{
    as_opt_u64, invalid_argument, parse_lan_transport, parse_path, parse_port_mapping,
    parse_relay_transport, parse_transport, to_napi, transport_name,
};
use crate::diagnostics::JsRelayDiagnostics;
use crate::readers::{RelayMetricsReader, period};
use crate::tunnel::{Connection, Registration};

#[napi(object)]
pub struct JsClientConfig {
    /// Relay carrier: tcp (default) or quic, independent of the local service protocol.
    pub relay_transport: Option<String>,
    pub server: String,
    pub credential: String,
    pub keep_alive: Option<bool>,
    pub namespace: Option<i64>,
}

#[napi(object)]
pub struct JsRegisterRequest {
    /// Accept direct sessions from authorized consumers (default `true`).
    /// `false` keeps every session to this service on the relay.
    pub allow_direct: Option<bool>,
    /// When to ask the router (PCP, NAT-PMP or UPnP) to map the direct UDP
    /// port: "auto" once punching through fails (default), "always", or "off".
    /// Ignored when direct is not allowed.
    pub port_mapping: Option<String>,
    /// Probe a consumer's addresses on a local network shared with it first,
    /// and again 20 and 50 ms later (default `true`). `false` probes them in
    /// the order the consumer offered them. Ignored when direct is not
    /// allowed.
    pub lan_first: Option<bool>,
    /// Serve a consumer that asks for TCP within a local network shared with
    /// it (default `true`): a TCP listener opens the first time one asks,
    /// offered on LANs only. `false` leaves such a consumer on QUIC. Ignored
    /// when direct is not allowed.
    pub lan_tcp: Option<bool>,
    pub key: String,
    pub local_addr: String,
    pub transport: String,
    pub codec: Option<bool>,
    pub force_namespace: Option<bool>,
}

impl TryFrom<JsRegisterRequest> for RegisterRequest {
    type Error = Error;

    fn try_from(request: JsRegisterRequest) -> Result<Self> {
        Ok(Self {
            key: request.key,
            local_addr: request.local_addr,
            transport: parse_transport(&request.transport)?,
            codec: request.codec.unwrap_or(false),
            force_namespace: request.force_namespace.unwrap_or(false),
        })
    }
}

#[napi(object)]
pub struct JsConnectRequest {
    /// "auto" (default): direct first, relay fallback before any byte is sent.
    /// "relay-only": always use the relay.
    pub path: Option<String>,
    /// When to ask the router (PCP, NAT-PMP or UPnP) to map the direct UDP
    /// port: "auto" once punching through fails (default), "always", or "off".
    /// Ignored on the relay-only path.
    pub port_mapping: Option<String>,
    /// Try a local network shared with the publisher first (default `true`):
    /// probe its addresses there first, and once that network is known to be
    /// there, hold other paths back up to 150 ms for it. `false` dials every
    /// path as soon as it answers. Ignored on the relay-only path.
    pub lan_first: Option<bool>,
    /// What a path within a local network shared with the publisher runs
    /// over: "quic" (default), or "tcp", tried 300 ms ahead of QUIC and
    /// falling back to it. A path across the internet is always QUIC.
    /// Ignored on the relay-only path.
    pub lan_transport: Option<String>,
    pub key: String,
    pub local_addr: String,
    pub transport: String,
}

#[napi(object)]
pub struct JsServiceConnection {
    pub conn_id: u32,
    pub generation: i64,
    pub protocol_version: u32,
    pub healthy: bool,
    pub last_rx_age_ms: i64,
}

impl From<sdk::ServiceConnection> for JsServiceConnection {
    fn from(value: sdk::ServiceConnection) -> Self {
        Self {
            conn_id: value.conn_id,
            generation: value.generation as i64,
            protocol_version: u32::from(value.protocol_version),
            healthy: value.healthy,
            last_rx_age_ms: value.last_rx_age_ms as i64,
        }
    }
}

#[napi(object)]
pub struct JsRemoteId {
    pub server_map: String,
    pub active: String,
    pub idle: String,
}

#[napi]
pub struct Client {
    inner: sdk::Client,
}

#[napi]
impl Client {
    #[napi(constructor)]
    pub fn new(config: JsClientConfig) -> Result<Self> {
        let relay_transport = parse_relay_transport(config.relay_transport.as_deref())?;
        let inner = sdk::Client::new(ClientConfig {
            server: config.server,
            credential: config.credential,
            keep_alive: config.keep_alive.unwrap_or(false),
            namespace: as_opt_u64("namespace", config.namespace)?,
        })
        .map_err(to_napi)?;
        let inner = inner
            .with_relay_transport(relay_transport)
            .map_err(to_napi)?;
        Ok(Self { inner })
    }

    #[napi]
    pub async fn relay_diagnostics(&self) -> JsRelayDiagnostics {
        self.inner.relay_diagnostics().await.into()
    }

    /// `relayDiagnostics()` at once, then every `everyMs` (one second when
    /// absent; 100 ms to an hour), read one at a time.
    #[napi]
    pub fn subscribe_relay_metrics(&self, every_ms: Option<u32>) -> RelayMetricsReader {
        RelayMetricsReader::new(self.inner.subscribe_relay_metrics(period(every_ms)))
    }

    /// The relay carrier this client uses: `tcp` or `quic`.
    #[napi]
    pub fn relay_transport(&self) -> String {
        transport_name(self.inner.relay_transport())
    }

    /// The UDP port the relay offers QUIC on, or `null` when it offers none.
    /// Asked over TCP whatever this client's carrier.
    #[napi]
    pub async fn relay_quic_port(&self) -> Result<Option<u32>> {
        self.inner
            .relay_quic_port()
            .await
            .map(|port| port.map(u32::from))
            .map_err(to_napi)
    }

    #[napi]
    pub fn server(&self) -> String {
        self.inner.server().to_string()
    }

    /// Wake recovering tunnels after a host network or resume notification.
    #[napi]
    pub fn notify_network_change(&self) {
        self.inner.notify_network_change();
    }

    #[napi]
    pub async fn register(&self, request: JsRegisterRequest) -> Result<Registration> {
        register_tunnel(&self.inner, request, None).await
    }

    /// Publish TCP with an idle deadline in milliseconds; zero disables it.
    /// Traffic in either direction resets it. Other registrations are unaffected.
    #[napi]
    pub async fn register_with_idle_timeout(
        &self,
        request: JsRegisterRequest,
        idle_timeout_ms: f64,
    ) -> Result<Registration> {
        if !(0.0..=9_007_199_254_740_991.0).contains(&idle_timeout_ms)
            || idle_timeout_ms.fract() != 0.0
        {
            return Err(invalid_argument(
                "idleTimeoutMs must be a non-negative safe integer",
            ));
        }
        let idle_timeout = Duration::from_millis(idle_timeout_ms as u64);
        register_tunnel(&self.inner, request, Some(idle_timeout)).await
    }

    #[napi]
    pub async fn connect(&self, request: JsConnectRequest) -> Result<Connection> {
        let path = parse_path(request.path.as_deref())?;
        let connection = self
            .inner
            .connect_with_options(
                ConnectRequest {
                    key: request.key,
                    local_addr: request.local_addr,
                    transport: parse_transport(&request.transport)?,
                },
                sdk::ConnectOptions {
                    path,
                    port_mapping: parse_port_mapping(request.port_mapping.as_deref())?,
                    lan_first: request.lan_first.unwrap_or(true),
                    lan_transport: parse_lan_transport(request.lan_transport.as_deref())?,
                },
            )
            .await
            .map_err(to_napi)?;
        Ok(Connection::new(connection))
    }

    #[napi]
    pub async fn list_keys(&self) -> Result<Vec<String>> {
        self.inner.list_keys().await.map_err(to_napi)
    }

    #[napi]
    pub async fn service_status(&self, key: String) -> Result<Vec<JsServiceConnection>> {
        Ok(self
            .inner
            .service_status(key)
            .await
            .map_err(to_napi)?
            .into_iter()
            .map(JsServiceConnection::from)
            .collect())
    }

    /// Namespace-scoped endpoint observations, serialized without credentials.
    #[napi]
    pub async fn sessions_json(&self) -> Result<String> {
        serde_json::to_string(&self.inner.sessions().await.map_err(to_napi)?)
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    #[napi]
    pub async fn remote_id(&self) -> Result<JsRemoteId> {
        let remote = self.inner.remote_id().await.map_err(to_napi)?;
        Ok(JsRemoteId {
            server_map: remote.server_map,
            active: remote.active,
            idle: remote.idle,
        })
    }

    #[napi]
    pub fn admin(&self) -> Result<Admin> {
        Ok(Admin {
            inner: self.inner.admin().map_err(to_napi)?,
        })
    }
}

/// `register` and `registerWithIdleTimeout`, which differ only in
/// `idle_timeout`. Every option is spelled out, so one the SDK adds needs a
/// JS default here before this compiles.
async fn register_tunnel(
    client: &sdk::Client,
    request: JsRegisterRequest,
    idle_timeout: Option<Duration>,
) -> Result<Registration> {
    let options = sdk::RegisterOptions {
        allow_direct: request.allow_direct.unwrap_or(true),
        idle_timeout,
        port_mapping: parse_port_mapping(request.port_mapping.as_deref())?,
        lan_first: request.lan_first.unwrap_or(true),
        lan_tcp: request.lan_tcp.unwrap_or(true),
    };
    let registration = client
        .register_with_options(request.try_into()?, options)
        .await
        .map_err(to_napi)?;
    Ok(Registration::new(registration))
}
