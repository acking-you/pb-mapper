//! Non-blocking status views and bounded asynchronous refresh scheduling for Flutter.
//!
//! ```text
//! UI read -> cached snapshot -> immediate response
//!              |
//!              +-- stale? -> one deduplicated network refresh -> cache + change event
//! force refresh -----------------------------------------> awaited network result
//! ```
//!
//! Service, client, and embedded-relay caches are independent. Refresh markers prevent
//! duplicate probes, while visible events are emitted only when the displayed state
//! actually changes.

use super::*;

impl PbMapperState {
    pub async fn get_config_status(&self) -> AppConfig {
        self.config.clone()
    }

    pub async fn update_config(
        &mut self,
        server_address: String,
        keep_alive: bool,
        msg_header_key: String,
    ) -> Result<(), CtlError> {
        let msg_header_key = normalize_msg_header_key(msg_header_key)?;
        self.config.server_address = server_address;
        self.config.keep_alive_enabled = keep_alive;
        self.config.msg_header_key = msg_header_key;
        self.apply_msg_header_key_env()?;
        self.save_config()?;
        self.reset_status_caches().await;
        Ok(())
    }

    pub async fn get_service_configs(&self) -> Vec<ServiceConfigInfo> {
        let store = self.load_service_configs();
        let mut services = Vec::new();

        let mut sorted_configs: Vec<_> = store.services.values().collect();
        sorted_configs.sort_by_key(|config| config.created_at);

        for config in sorted_configs {
            let (status, message) = self.calculate_service_status(&config.service_key).await;

            services.push(ServiceConfigInfo {
                service_key: config.service_key.clone(),
                local_address: config.local_address.clone(),
                protocol: config.protocol.clone(),
                enable_encryption: config.enable_encryption,
                enable_keep_alive: config.enable_keep_alive,
                status,
                status_message: message,
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
        let (status, message) = self.calculate_service_status(&service_key).await;
        ServiceStatusResponse {
            service_key,
            status,
            message,
        }
    }

    pub async fn get_client_configs(&self) -> Vec<ClientConfigInfo> {
        let store = self.load_client_configs();
        let mut client_infos = Vec::new();

        for (service_key, config) in store.clients.iter() {
            let (status, status_message) = self.calculate_client_status(service_key).await;

            client_infos.push(ClientConfigInfo {
                service_key: config.service_key.clone(),
                local_address: config.local_address.clone(),
                protocol: config.protocol.clone(),
                enable_keep_alive: config.enable_keep_alive,
                status,
                status_message,
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
        let (status, message) = self.calculate_client_status(&service_key).await;
        ClientStatusResponse {
            service_key,
            status,
            message,
        }
    }

    pub async fn get_local_server_status(&self) -> LocalServerStatus {
        let is_running = self.server_handle.is_some();
        if !is_running {
            let status = LocalServerStatus {
                is_running: false,
                active_connections: 0,
                registered_services: 0,
                uptime_seconds: 0,
            };
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

        tokio::spawn(async move {
            let mut status = LocalServerStatus {
                is_running: true,
                active_connections: 0,
                registered_services: 0,
                uptime_seconds: start_time
                    .and_then(|ts| SystemTime::now().duration_since(ts).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };

            if let Some(sender) = sender {
                let (response_sender, response_receiver) = tokio::sync::oneshot::channel();
                if sender.send(response_sender).is_ok() {
                    if let Ok(Ok(info)) =
                        tokio::time::timeout(Duration::from_millis(200), response_receiver).await
                    {
                        status.active_connections = info.active_connections;
                        status.registered_services = info.registered_services;
                        status.uptime_seconds = info.uptime_seconds;
                    }
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
            },
            Ok(Err(e)) => {
                tracing::warn!("Force refresh failed: {}", e);
                ServerStatusDetail {
                    server_available: false,
                    registered_services: Vec::new(),
                    server_map: String::new(),
                    active_connections: String::new(),
                    idle_connections: String::new(),
                }
            }
            Err(_) => {
                tracing::warn!("Force refresh timed out after {:?}", FORCE_REFRESH_TIMEOUT);
                ServerStatusDetail {
                    server_available: false,
                    registered_services: Vec::new(),
                    server_map: String::new(),
                    active_connections: String::new(),
                    idle_connections: String::new(),
                }
            }
        };

        Ok(detail)
    }

    // Cache service status to avoid blocking UI with network checks on every paint.
    async fn get_cached_service_status(&self, service_key: &str) -> (String, String) {
        if let Some(handle) = self.service_handles.get(service_key) {
            if handle.is_finished() {
                return (
                    "failed".to_string(),
                    "Service connection terminated".to_string(),
                );
            }

            let cached = {
                let cache = self.service_status_cache.read().await;
                cache.get(service_key).cloned()
            };

            let should_refresh = cached
                .as_ref()
                .map(|entry| entry.updated_at.elapsed() > STATUS_CACHE_TTL)
                .unwrap_or(true);

            if should_refresh {
                self.schedule_service_status_refresh(service_key).await;
            }

            if let Some(entry) = cached {
                return (entry.status, entry.message);
            }

            return (
                "retrying".to_string(),
                "Checking service status...".to_string(),
            );
        }

        (
            "stopped".to_string(),
            "Service is not registered".to_string(),
        )
    }

    // Cache client status to avoid blocking UI with network checks on every paint.
    async fn get_cached_client_status(&self, service_key: &str) -> (String, String) {
        if let Some(handle) = self.client_handles.get(service_key) {
            if handle.is_finished() {
                return (
                    "failed".to_string(),
                    "Client connection terminated".to_string(),
                );
            }

            let cached = {
                let cache = self.client_status_cache.read().await;
                cache.get(service_key).cloned()
            };

            let should_refresh = cached
                .as_ref()
                .map(|entry| entry.updated_at.elapsed() > STATUS_CACHE_TTL)
                .unwrap_or(true);

            if should_refresh {
                self.schedule_client_status_refresh(service_key).await;
            }

            if let Some(entry) = cached {
                return (entry.status, entry.message);
            }

            return (
                "retrying".to_string(),
                "Checking client status...".to_string(),
            );
        }

        ("stopped".to_string(), "Client is not connected".to_string())
    }

    pub(super) async fn schedule_service_status_refresh(&self, service_key: &str) {
        {
            let mut refreshing = self.service_status_refreshing.write().await;
            if refreshing.contains(service_key) {
                return;
            }
            refreshing.insert(service_key.to_string());
        }

        let server_addr = self.config.server_address.clone();
        let cache = self.service_status_cache.clone();
        let refreshing = self.service_status_refreshing.clone();
        let key = service_key.to_string();

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                STATUS_REFRESH_TIMEOUT,
                check_service_with_get_status(&server_addr, &key),
            )
            .await;

            let (status, message) = match result {
                Ok(Ok(true)) => (
                    "running".to_string(),
                    "Service is running normally".to_string(),
                ),
                Ok(Ok(false)) => (
                    "retrying".to_string(),
                    "Service is in retry connection loop".to_string(),
                ),
                Ok(Err(_)) | Err(_) => (
                    "failed".to_string(),
                    "Cannot connect to pb-server".to_string(),
                ),
            };

            let changed = {
                let mut cache = cache.write().await;
                let changed = cache
                    .get(&key)
                    .is_none_or(|entry| entry.status != status || entry.message != message);
                cache.insert(
                    key.clone(),
                    StatusCacheEntry {
                        status,
                        message,
                        updated_at: Instant::now(),
                    },
                );
                changed
            };
            // Only transitions the user can perceive. These run on a timer for
            // every configured entry, so emitting on every refresh would reload
            // the list several times a second for no visible reason.
            if changed {
                events::emit(events::ChangeKind::Services, Some(&key), Origin::Internal);
            }

            let mut refreshing = refreshing.write().await;
            refreshing.remove(&key);
        });
    }

    pub(super) async fn schedule_client_status_refresh(&self, service_key: &str) {
        {
            let mut refreshing = self.client_status_refreshing.write().await;
            if refreshing.contains(service_key) {
                return;
            }
            refreshing.insert(service_key.to_string());
        }

        let server_addr = self.config.server_address.clone();
        let cache = self.client_status_cache.clone();
        let refreshing = self.client_status_refreshing.clone();
        let key = service_key.to_string();

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                STATUS_REFRESH_TIMEOUT,
                check_service_with_get_status(&server_addr, &key),
            )
            .await;

            let (status, message) = match result {
                Ok(Ok(true)) => (
                    "running".to_string(),
                    "Client is connected normally".to_string(),
                ),
                Ok(Ok(false)) => (
                    "retrying".to_string(),
                    "Client is in retry connection loop".to_string(),
                ),
                Ok(Err(_)) | Err(_) => (
                    "failed".to_string(),
                    "Cannot connect to pb-server".to_string(),
                ),
            };

            let changed = {
                let mut cache = cache.write().await;
                let changed = cache
                    .get(&key)
                    .is_none_or(|entry| entry.status != status || entry.message != message);
                cache.insert(
                    key.clone(),
                    StatusCacheEntry {
                        status,
                        message,
                        updated_at: Instant::now(),
                    },
                );
                changed
            };
            // Only transitions the user can perceive. These run on a timer for
            // every configured entry, so emitting on every refresh would reload
            // the list several times a second for no visible reason.
            if changed {
                events::emit(events::ChangeKind::Clients, Some(&key), Origin::Internal);
            }

            let mut refreshing = refreshing.write().await;
            refreshing.remove(&key);
        });
    }

    async fn calculate_service_status(&self, service_key: &str) -> (String, String) {
        self.get_cached_service_status(service_key).await
    }

    async fn calculate_client_status(&self, service_key: &str) -> (String, String) {
        self.get_cached_client_status(service_key).await
    }
}
