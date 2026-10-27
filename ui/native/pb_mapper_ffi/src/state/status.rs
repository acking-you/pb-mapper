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

use super::*;

impl PbMapperState {
    pub async fn get_config_status(&self) -> AppConfig {
        self.config.clone()
    }

    pub fn isolated_admin_key(&self) -> Option<String> {
        let path = self.config_dir.join("auth").join("admin.key");
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
            && *config == self.config
        {
            return Ok(client.clone());
        }
        let credential = get_process_credential().map_err(|error| {
            CtlError::invalid_argument(format!("This computer has no usable credential: {error}"))
        })?;
        let client = Client::from_credential(
            &self.config.server_address,
            credential,
            self.config.keep_alive_enabled,
            None,
        )
        .with_relay_transport(self.config.relay_transport)
        .map_err(sdk_error)?;
        *cached = Some((self.config.clone(), client.clone()));
        Ok(client)
    }

    pub async fn update_config(
        &mut self,
        server_address: String,
        keep_alive: bool,
        msg_header_key: String,
        relay_transport: RelayTransport,
    ) -> Result<(), CtlError> {
        let msg_header_key = normalize_msg_header_key(msg_header_key)?;
        let previous = self.config.clone();
        let candidate = AppConfig {
            server_address,
            keep_alive_enabled: keep_alive,
            msg_header_key,
            relay_transport,
        };
        self.write_config_file(&candidate)?;
        self.config = candidate;
        if let Err(error) = self.apply_msg_header_key_env() {
            self.config = previous.clone();
            let _ = self.write_config_file(&previous);
            let _ = self.apply_msg_header_key_env();
            return Err(error);
        }
        self.reset_status_caches().await;
        Ok(())
    }

    pub async fn get_service_configs(&self) -> Vec<ServiceConfigInfo> {
        let store = self.load_service_configs();
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
                status: view.status.to_string(),
                status_message: view.message,
                path: view.path,
                relay_transport: view.transport,
                created_at_ms: config
                    .created_at
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                updated_at_ms: SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            });
        }

        services
    }

    pub async fn get_service_status(&self, service_key: String) -> ServiceStatusResponse {
        let view = self.service_view(&service_key);
        ServiceStatusResponse {
            service_key,
            status: view.status.to_string(),
            message: view.message,
        }
    }

    pub async fn get_client_configs(&self) -> Vec<ClientConfigInfo> {
        let store = self.load_client_configs();
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
                status: view.status.to_string(),
                status_message: view.message,
                path: view.path,
                relay_transport: view.transport,
                created_at_ms: config
                    .created_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                updated_at_ms: config
                    .created_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            });
        }

        client_infos.sort_by_key(|info| info.created_at_ms);
        client_infos
    }

    pub async fn get_client_status(&self, service_key: String) -> ClientStatusResponse {
        let view = self.client_view(&service_key);
        ClientStatusResponse {
            service_key,
            status: view.status.to_string(),
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

    pub async fn get_local_server_status(&self) -> LocalServerStatus {
        // One that ended on its own is not running, whatever is left of it.
        let is_running = self
            .server_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished());
        if !is_running {
            let status = LocalServerStatus::default();
            {
                let mut cache = self.local_server_status_cache.write().await;
                *cache = status.clone();
            }
            {
                let mut last_update = self.local_server_status_last_update.write().await;
                *last_update = Some(Instant::now());
            }
            return status;
        }

        let should_refresh = {
            let last_update = self.local_server_status_last_update.read().await;
            cache_is_stale(*last_update, STATUS_CACHE_TTL)
        };

        if should_refresh {
            self.schedule_local_server_status_refresh();
        }

        let cache = self.local_server_status_cache.read().await;
        cache.clone()
    }

    fn schedule_local_server_status_refresh(&self) {
        if self
            .local_server_status_refreshing
            .swap(true, Ordering::AcqRel)
        {
            return;
        }

        let sender = self.server_status_sender.clone();
        let cache = self.local_server_status_cache.clone();
        let last_update = self.local_server_status_last_update.clone();
        let refreshing = self.local_server_status_refreshing.clone();
        let start_time = self.server_start_time;
        let port = self.server_port;
        let quic_port = self.server_quic_port;

        tokio::spawn(async move {
            let mut status = LocalServerStatus {
                is_running: true,
                port,
                quic_port,
                uptime_seconds: start_time
                    .and_then(|ts| SystemTime::now().duration_since(ts).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                ..LocalServerStatus::default()
            };

            if let Some(sender) = sender {
                let (response_sender, response_receiver) = tokio::sync::oneshot::channel();
                if sender.send(response_sender).is_ok()
                    && let Ok(Ok(info)) =
                        tokio::time::timeout(Duration::from_millis(200), response_receiver).await
                {
                    status.active_connections = info.active_connections;
                    status.registered_services = info.registered_services;
                    status.uptime_seconds = info.uptime_seconds;
                }
            }

            {
                let mut cache = cache.write().await;
                *cache = status;
            }
            {
                let mut last_update = last_update.write().await;
                *last_update = Some(Instant::now());
            }
            refreshing.store(false, Ordering::Release);
        });
    }

    /// The periodic check: the relay's holdings, and what it said about QUIC
    /// for these settings, asking only if it has not said yet.
    pub async fn get_server_status_detail(&self) -> Result<ServerStatusDetail, CtlError> {
        self.refresh_server_status(false).await
    }

    /// The connections the server holds for one key, from the protocol's own
    /// structured query rather than the Debug dump in `server_map`.
    pub async fn get_service_conns(
        &self,
        service_key: String,
    ) -> Result<Vec<ServiceConnInfo>, CtlError> {
        let server_addr = self.config.server_address.clone();
        let client = self.relay_client()?;
        match tokio::time::timeout(FORCE_REFRESH_TIMEOUT, client.service_status(&service_key)).await
        {
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

    /// Perform a blocking status refresh — waits for the actual network result
    /// instead of returning stale cache — and asks the relay about QUIC again.
    pub async fn force_refresh_server_status(&self) -> Result<ServerStatusDetail, CtlError> {
        self.refresh_server_status(true).await
    }

    /// Ask the relay what it holds. What it says about QUIC is asked only
    /// when `force`d or not yet known for these settings.
    async fn refresh_server_status(&self, force: bool) -> Result<ServerStatusDetail, CtlError> {
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
        let client = match self.relay_client() {
            Ok(client) => client,
            Err(error) => return Ok(unavailable(error.to_string())),
        };
        let known = self
            .quic_offer
            .lock()
            .as_ref()
            .filter(|(config, _)| *config == self.config)
            .map(|(_, port)| *port);

        let (answer, mut asked) = fetch_real_status(&client, force || known.is_none()).await;
        // Over QUIC, a relay that does not answer at all may still answer
        // over TCP, and then it is UDP that does not get through. Asked here,
        // on that failure only, so the window can say what to open; not of a
        // relay already known to offer no QUIC, which the refusal says.
        let quic_unanswered = self.config.relay_transport == RelayTransport::Quic
            && matches!(&answer, Err(error) if matches!(
                error.code(),
                ErrorCode::ServerUnreachable | ErrorCode::Timeout
            ));
        if quic_unanswered && asked.is_none() && known != Some(None) {
            asked = client.relay_quic_port().await.ok();
        }
        if let Some(port) = asked {
            *self.quic_offer.lock() = Some((self.config.clone(), port));
        }
        let quic = asked.or(known);
        let detail = match answer {
            Ok((services, remote_id, latency)) => ServerStatusDetail {
                server_available: true,
                registered_services: services,
                server_map: remote_id.server_map,
                active_connections: remote_id.active,
                idle_connections: remote_id.idle,
                error: None,
                latency_ms: Some(latency.as_millis() as u64),
                quic_port: quic.flatten(),
                quic_known: quic.is_some(),
            },
            Err(e) => {
                tracing::warn!("Relay status refresh failed: {}", e);
                let error = match asked {
                    Some(Some(port)) if quic_unanswered => format!(
                        "The relay answers over TCP, but QUIC on UDP port {port} does not get \
                         through. Open UDP {port} on the relay's firewall and on this network, \
                         or choose TCP"
                    ),
                    _ => e.to_string(),
                };
                ServerStatusDetail {
                    quic_port: quic.flatten(),
                    quic_known: quic.is_some(),
                    ..unavailable(error)
                }
            }
        };

        Ok(detail)
    }
}
