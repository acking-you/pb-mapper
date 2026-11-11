//! Non-blocking status views for Flutter.
//!
//! ```text
//! tunnel read   -> its SDK handle's status and diagnostics -> immediate response
//! relay read    -> cached snapshot; stale? -> one deduplicated refresh
//! force refresh ---------------------------------------> awaited network result
//! ```
//!
//! A tunnel's state is what its own worker reports, the same worker the CLI
//! runs, so nothing here asks the relay about it; `watch_tunnel` emits the
//! change events. Only the embedded relay's figures are cached.
//!
//! A question to the relay takes the state lock only for the settings and
//! client it asks with, and again, briefly, to keep what it learned about
//! QUIC; it asks without it, as a tunnel's setup does: the
//! window checks the relay every few seconds, and a relay that is slow or
//! silent would otherwise hold every other call for as long as the check
//! waits.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;

use pb_mapper_client::sdk::{Client, RelayTransport, RemoteId};
use pb_mapper_core::time::saturating_millis;

use super::runtime::EmbeddedRelay;
use super::stored::normalize_msg_header_key;
use super::tunnel::{TunnelHandle, TunnelView};
use super::{
    AppConfig, ClientConfigInfo, LocalServerStatus, PbMapperState, ServerStatusDetail,
    ServiceConfigInfo, ServiceConnInfo, TunnelStatusResponse, process_credential, relay_client_for,
};
use crate::error::{CtlError, ErrorCode, sdk_error};

const STATUS_CACHE_TTL: Duration = Duration::from_secs(2);
const FORCE_REFRESH_TIMEOUT: Duration = Duration::from_millis(5000);
/// How long the embedded relay has to report its figures.
const LOCAL_STATUS_TIMEOUT: Duration = Duration::from_millis(200);

/// The embedded relay's figures as last read, when, and whether a read is
/// under way, kept together so that they change together.
#[derive(Default)]
pub(super) struct LocalStatusCache {
    pub(super) status: LocalServerStatus,
    pub(super) last_update: Option<Instant>,
    pub(super) refreshing: bool,
}

impl LocalStatusCache {
    /// Takes `status` as read now.
    pub(super) fn record(&mut self, status: LocalServerStatus) {
        self.status = status;
        self.last_update = Some(Instant::now());
    }
}

/// What the relay answered: its services, the routing table, and how long
/// the services took to come back.
type RelayAnswer = (Vec<String>, RemoteId, Duration);

/// What the relay holds, asked over the carrier this computer uses within
/// [`FORCE_REFRESH_TIMEOUT`]; and with `ask_quic`, beside it and over TCP
/// whichever carrier that is, what the relay says about QUIC: the UDP port it
/// offers it on, or `None` for none, or nothing when it did not answer.
async fn fetch_real_status(
    client: &Client,
    ask_quic: bool,
) -> (Result<RelayAnswer, CtlError>, Option<Option<u16>>) {
    let answer = async {
        let keys = async {
            let started = Instant::now();
            (client.list_keys().await, started.elapsed())
        };
        let ((keys_result, latency), remote_id_result) = tokio::join!(keys, client.remote_id());
        // The reason as it is: Settings' Test shows it to the user, after its
        // own "The relay didn't answer".
        let services = keys_result.map_err(sdk_error)?;
        let remote_id = remote_id_result.unwrap_or_else(|e| {
            tracing::warn!("Failed to get remote-id data: {}, using empty data", e);
            RemoteId {
                server_map: String::new(),
                active: String::new(),
                idle: String::new(),
            }
        });
        Ok((services, remote_id, latency))
    };
    let answer = async {
        tokio::time::timeout(FORCE_REFRESH_TIMEOUT, answer)
            .await
            .unwrap_or_else(|_| {
                Err(CtlError::timeout(format!(
                    "{} did not answer within {} seconds",
                    client.server(),
                    FORCE_REFRESH_TIMEOUT.as_secs()
                )))
            })
    };
    // Bounded by the SDK's own timeout; outside the one above, so a TCP
    // question that stalls cannot make a relay that answered look silent.
    let quic = async {
        if !ask_quic {
            return None;
        }
        client
            .relay_quic_port()
            .await
            .inspect_err(|e| tracing::debug!("The relay did not say whether it offers QUIC: {e}"))
            .ok()
    };
    tokio::join!(answer, quic)
}

/// `at` in whole milliseconds since the Unix epoch, as
/// [`unix_millis`](pb_mapper_core::time::unix_millis) reads
/// the clock now.
fn unix_millis_at(at: SystemTime) -> u64 {
    saturating_millis(at.duration_since(UNIX_EPOCH).unwrap_or_default())
}

fn cache_is_stale(last_update: Option<Instant>, ttl: Duration) -> bool {
    match last_update {
        Some(ts) => ts.elapsed() > ttl,
        None => true,
    }
}

impl PbMapperState {
    /// The share (`share`) or connection running under `key`.
    pub(crate) fn running_tunnel(
        &self,
        share: bool,
        key: &str,
    ) -> Result<&Arc<TunnelHandle>, CtlError> {
        let runtimes = if share {
            &self.service_runtime
        } else {
            &self.client_runtime
        };
        runtimes
            .get(key)
            .map(|runtime| &runtime.handle)
            .ok_or_else(|| CtlError::not_found(format!("'{key}' is not running")))
    }
    pub fn get_config_status(&self) -> AppConfig {
        self.config.clone()
    }

    pub fn isolated_admin_key(&self) -> Option<String> {
        let path = pb_mapper_auth::admin_key_path(&self.auth_state_dir());
        std::fs::read_to_string(path).ok().and_then(|raw| {
            let key = raw.trim().to_string();
            (!key.is_empty()).then_some(key)
        })
    }

    /// The client this window's questions to the relay go through, made
    /// again only when the relay settings change. See `relay_client`.
    pub(crate) fn relay_client(&self) -> Result<Client, CtlError> {
        let mut cached = self.relay_client.lock();
        if let Some((config, client)) = cached.as_ref()
            && config.same_relay(&self.config)
        {
            return Ok(client.clone());
        }
        let client = relay_client_for(
            &self.config.server_address,
            process_credential()?,
            self.config.keep_alive_enabled,
            None,
            self.config.relay_transport,
        )?;
        *cached = Some((self.config.clone(), client.clone()));
        Ok(client)
    }

    /// Save `candidate` as this computer's config, its credential checked
    /// and in effect for the process. What is cached about the relay, its
    /// client and what it said about QUIC, is kept with the settings it was
    /// learned under, so a change of relay settings leaves it unused; the
    /// embedded relay's figures do not depend on them.
    pub fn update_config(&mut self, candidate: AppConfig) -> Result<(), CtlError> {
        let candidate = AppConfig {
            msg_header_key: normalize_msg_header_key(candidate.msg_header_key)?,
            ..candidate
        };
        let previous = self.config.clone();
        self.write_config_file(&candidate)?;
        self.config = candidate;
        if let Err(error) = self.apply_msg_header_key_env() {
            self.config = previous.clone();
            let _ = self.write_config_file(&previous);
            let _ = self.apply_msg_header_key_env();
            return Err(error);
        }
        Ok(())
    }

    pub fn get_service_configs(&self) -> Result<Vec<ServiceConfigInfo>, CtlError> {
        let store = self.load_service_configs()?;
        let mut services = Vec::new();

        let mut sorted_configs: Vec<_> = store.services.values().collect();
        sorted_configs.sort_by_key(|config| config.created_at);

        for config in sorted_configs {
            let view = self.service_view(&config.service_key);

            services.push(ServiceConfigInfo {
                service_key: config.service_key.clone(),
                local_address: config.local_address.clone(),
                protocol: config.protocol.clone(),
                enable_encryption: config.enable_encryption,
                enable_keep_alive: config.enable_keep_alive,
                allow_direct: config.allow_direct,
                port_mapping: config.port_mapping,
                status: view.status,
                status_message: view.message,
                path: view.path,
                relay_transport: view.transport,
                created_at_ms: unix_millis_at(config.created_at),
                updated_at_ms: unix_millis_at(config.updated_at.unwrap_or(config.created_at)),
            });
        }

        Ok(services)
    }

    pub fn get_service_status(&self, service_key: String) -> TunnelStatusResponse {
        let view = self.service_view(&service_key);
        TunnelStatusResponse {
            service_key,
            status: view.status,
            message: view.message,
        }
    }

    pub fn get_client_configs(&self) -> Result<Vec<ClientConfigInfo>, CtlError> {
        let store = self.load_client_configs()?;
        let mut client_infos = Vec::new();

        for (service_key, config) in store.clients.iter() {
            let view = self.client_view(service_key);

            client_infos.push(ClientConfigInfo {
                service_key: config.service_key.clone(),
                local_address: config.local_address.clone(),
                protocol: config.protocol.clone(),
                enable_keep_alive: config.enable_keep_alive,
                namespace: config.namespace,
                routed_namespace: self
                    .client_runtime
                    .get(service_key)
                    .map(|runtime| runtime.pin.routed_namespace()),
                allow_direct: config.allow_direct,
                port_mapping: config.port_mapping,
                status: view.status,
                status_message: view.message,
                path: view.path,
                relay_transport: view.transport,
                created_at_ms: unix_millis_at(config.created_at),
                updated_at_ms: unix_millis_at(config.updated_at.unwrap_or(config.created_at)),
            });
        }

        client_infos.sort_by_key(|info| info.created_at_ms);
        Ok(client_infos)
    }

    pub fn get_client_status(&self, service_key: String) -> TunnelStatusResponse {
        let view = self.client_view(&service_key);
        TunnelStatusResponse {
            service_key,
            status: view.status,
            message: view.message,
        }
    }

    fn service_view(&self, service_key: &str) -> TunnelView {
        match self.service_runtime.get(service_key) {
            Some(runtime) => runtime.handle.view(),
            None => TunnelView::stopped("Service is not registered"),
        }
    }

    fn client_view(&self, service_key: &str) -> TunnelView {
        match self.client_runtime.get(service_key) {
            Some(runtime) => runtime.handle.view(),
            None => TunnelView::stopped("Client is not connected"),
        }
    }

    /// The embedded relay's figures as last read, starting a read in the
    /// background when they are stale. Spawns onto the runtime it is called
    /// in.
    pub fn get_local_server_status(&self) -> LocalServerStatus {
        // One that ended on its own is not running, whatever is left of it.
        let Some(relay) = self
            .embedded_relay
            .as_ref()
            .filter(|relay| !relay.task.is_finished())
        else {
            let status = LocalServerStatus::default();
            self.local_server_status.lock().record(status.clone());
            return status;
        };

        let (status, refresh) = {
            let mut cache = self.local_server_status.lock();
            let refresh = cache_is_stale(cache.last_update, STATUS_CACHE_TTL)
                && !std::mem::replace(&mut cache.refreshing, true);
            (cache.status.clone(), refresh)
        };
        if refresh {
            self.refresh_local_server_status(relay);
        }
        status
    }

    /// Reads `relay`'s figures into the cache, in the background; the caller
    /// has marked the cache as refreshing.
    fn refresh_local_server_status(&self, relay: &EmbeddedRelay) {
        let sender = relay.status_requests.clone();
        let cache = self.local_server_status.clone();
        let started_at = relay.started_at;
        let port = relay.port;
        let quic_port = relay.quic_port;

        tokio::spawn(async move {
            let mut status = LocalServerStatus {
                is_running: true,
                port: Some(port),
                quic_port,
                uptime_seconds: SystemTime::now()
                    .duration_since(started_at)
                    .map_or(0, |d| d.as_secs()),
                ..LocalServerStatus::default()
            };

            let (response_sender, response_receiver) = tokio::sync::oneshot::channel();
            if sender.send(response_sender).is_ok()
                && let Ok(Ok(info)) =
                    tokio::time::timeout(LOCAL_STATUS_TIMEOUT, response_receiver).await
            {
                status.active_connections = info.active_connections;
                status.registered_services = info.registered_services;
                status.uptime_seconds = info.uptime_seconds;
            }

            let mut cache = cache.lock();
            cache.record(status);
            cache.refreshing = false;
        });
    }

    /// What the relay said about QUIC for the current settings, if it has.
    fn known_quic_offer(&self) -> Option<Option<u16>> {
        self.quic_offer
            .as_ref()
            .filter(|(config, _)| config.same_relay(&self.config))
            .map(|(_, port)| *port)
    }

    /// Keeps what the relay said about QUIC under `config`, the settings it
    /// was asked under, unless they have changed since: an answer about the
    /// old settings must not replace what the new ones were told. Two checks
    /// under the same settings may land in either order; both are the same
    /// relay's answer a moment apart, and the next forced check asks again.
    fn remember_quic_offer(&mut self, config: AppConfig, port: Option<u16>) {
        if config.same_relay(&self.config) {
            self.quic_offer = Some((config, port));
        }
    }
}

/// The periodic check: the relay's holdings, and what it said about QUIC for
/// these settings, asking only if it has not said yet.
pub async fn get_server_status_detail(state: &Arc<Mutex<PbMapperState>>) -> ServerStatusDetail {
    refresh_server_status(state, false).await
}

/// Perform a blocking status refresh — waits for the actual network result
/// instead of returning stale cache — and asks the relay about QUIC again.
pub async fn force_refresh_server_status(state: &Arc<Mutex<PbMapperState>>) -> ServerStatusDetail {
    refresh_server_status(state, true).await
}

/// The connections the server holds for one key, from the protocol's own
/// structured query rather than the Debug dump in `server_map`.
pub async fn get_service_conns(
    state: &Arc<Mutex<PbMapperState>>,
    service_key: String,
) -> Result<Vec<ServiceConnInfo>, CtlError> {
    let (server_addr, client) = {
        let state = state.lock().await;
        (state.config.server_address.clone(), state.relay_client()?)
    };
    match tokio::time::timeout(FORCE_REFRESH_TIMEOUT, client.service_status(&service_key)).await {
        Ok(result) => result
            .map(|connections| {
                connections
                    .into_iter()
                    .map(|c| ServiceConnInfo {
                        conn_id: c.conn_id,
                        generation: c.generation,
                        protocol_version: c.protocol_version,
                        healthy: c.healthy,
                        last_rx_age_ms: c.last_rx_age_ms,
                    })
                    .collect()
            })
            .map_err(sdk_error),
        Err(_) => Err(CtlError::timeout(format!(
            "Timed out asking {server_addr} about {service_key}"
        ))),
    }
}

/// Ask the relay what it holds. What it says about QUIC is asked only when
/// `force`d or not yet known for these settings.
async fn refresh_server_status(
    state: &Arc<Mutex<PbMapperState>>,
    force: bool,
) -> ServerStatusDetail {
    let unavailable = |error: String| ServerStatusDetail {
        server_available: false,
        registered_services: Vec::new(),
        server_map: String::new(),
        active_connections: String::new(),
        idle_connections: String::new(),
        error: Some(error),
        latency_ms: None,
        quic_port: None,
        quic_known: false,
    };
    // The settings the question is asked under, and the client and what is
    // known about QUIC that go with them.
    let (config, client, known) = {
        let state = state.lock().await;
        (
            state.config.clone(),
            state.relay_client(),
            state.known_quic_offer(),
        )
    };
    let client = match client {
        Ok(client) => client,
        Err(error) => return unavailable(error.to_string()),
    };

    let (answer, mut asked) = fetch_real_status(&client, force || known.is_none()).await;
    // Over QUIC, a relay that does not answer at all may still answer
    // over TCP, and then it is UDP that does not get through; and one that
    // says it offers no QUIC may have offered it when last asked. Asked
    // here, on those failures only, so the window can say what to open and
    // shows no port the relay has stopped offering; not of a relay already
    // known to offer no QUIC, which the refusal says.
    let quic_unanswered = config.relay_transport == RelayTransport::Quic
        && matches!(&answer, Err(error) if matches!(
            error.code(),
            ErrorCode::ServerUnreachable | ErrorCode::Timeout | ErrorCode::QuicUnavailable
        ));
    if quic_unanswered && asked.is_none() && known != Some(None) {
        asked = client.relay_quic_port().await.ok();
    }
    if let Some(port) = asked {
        state.lock().await.remember_quic_offer(config, port);
    }
    let quic = asked.or(known);
    match answer {
        Ok((services, remote_id, latency)) => ServerStatusDetail {
            server_available: true,
            registered_services: services,
            server_map: remote_id.server_map,
            active_connections: remote_id.active,
            idle_connections: remote_id.idle,
            error: None,
            latency_ms: Some(saturating_millis(latency)),
            quic_port: quic.flatten(),
            quic_known: quic.is_some(),
        },
        Err(e) => {
            tracing::warn!("Relay status refresh failed: {}", e);
            let error = match asked {
                // Shown after "The relay isn't answering:", so it leads
                // with what does not get through.
                Some(Some(port)) if quic_unanswered => format!(
                    "QUIC on UDP port {port} does not get through, though the relay \
                     answers over TCP. Open UDP {port} on the relay's firewall and on this \
                     network, or choose TCP"
                ),
                _ => e.to_string(),
            };
            ServerStatusDetail {
                quic_port: quic.flatten(),
                quic_known: quic.is_some(),
                ..unavailable(error)
            }
        }
    }
}
