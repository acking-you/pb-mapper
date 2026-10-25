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

    pub async fn update_config(
        &mut self,
        server_address: String,
        keep_alive: bool,
        msg_header_key: String,
    ) -> Result<(), CtlError> {
        let msg_header_key = normalize_msg_header_key(msg_header_key)?;
        let previous = self.config.clone();
        let candidate = AppConfig {
            server_address,
            keep_alive_enabled: keep_alive,
            msg_header_key,
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
        let is_running = self.server_handle.is_some();
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

        tokio::spawn(async move {
            let mut status = LocalServerStatus {
                is_running: true,
                port,
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

    pub async fn get_server_status_detail(&self) -> Result<ServerStatusDetail, CtlError> {
        self.force_refresh_server_status().await
    }

    /// The connections the server holds for one key, from the protocol's own
    /// structured query rather than the Debug dump in `server_map`.
    pub async fn get_service_conns(
        &self,
        service_key: String,
    ) -> Result<Vec<ServiceConnInfo>, CtlError> {
        let server_addr = self.config.server_address.clone();
        match tokio::time::timeout(
            FORCE_REFRESH_TIMEOUT,
            get_service_conns_with_addr(&server_addr, &service_key),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(CtlError::timeout(format!(
                "Timed out asking {server_addr} about {service_key}"
            ))),
        }
    }

    /// Perform a blocking status refresh — waits for the actual network result
    /// instead of returning stale cache.
    pub async fn force_refresh_server_status(&self) -> Result<ServerStatusDetail, CtlError> {
        let server_addr = self.config.server_address.clone();
        let started = Instant::now();
        let unavailable = |error: String| ServerStatusDetail {
            server_available: false,
            registered_services: Vec::new(),
            server_map: String::new(),
            active_connections: String::new(),
            idle_connections: String::new(),
            error: Some(error),
            latency_ms: None,
        };

        let detail = match tokio::time::timeout(
            FORCE_REFRESH_TIMEOUT,
            fetch_real_status_with_addr(&server_addr),
        )
        .await
        {
            Ok(Ok((services, remote_id_data))) => ServerStatusDetail {
                server_available: true,
                registered_services: services,
                server_map: remote_id_data.server_map,
                active_connections: remote_id_data.active,
                idle_connections: remote_id_data.idle,
                error: None,
                latency_ms: Some(started.elapsed().as_millis() as u64),
            },
            Ok(Err(e)) => {
                tracing::warn!("Force refresh failed: {}", e);
                unavailable(e.to_string())
            }
            Err(_) => {
                tracing::warn!("Force refresh timed out after {:?}", FORCE_REFRESH_TIMEOUT);
                unavailable(format!(
                    "{server_addr} did not answer within {} seconds",
                    FORCE_REFRESH_TIMEOUT.as_secs()
                ))
            }
        };

        Ok(detail)
    }
}
