//! Runtime lifecycle for the embedded relay, registered services, and local clients.
//!
//! ```text
//! start relay: bind listener -> initialize app-local auth -> spawn -> mark running
//! register:    resolved addresses -> spawn control pool -> retain JoinHandle
//! connect:     preflight local bind -> spawn listener ----> retain JoinHandle
//! stop:        cancel relay + abort owned tunnel tasks + clear runtime maps
//! ```
//!
//! Readiness is published only after both listener binding and authentication
//! initialization succeed, preventing the UI from displaying a phantom running relay.

use super::*;

impl PbMapperState {
    pub async fn start_server(
        &mut self,
        port: u16,
        enable_keep_alive: bool,
    ) -> Result<(), CtlError> {
        if self.server_handle.is_some() {
            return Err(CtlError::already_exists("Server is already running"));
        }

        let ip_addr = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
        let bind_addr = std::net::SocketAddr::new(ip_addr, port);

        let listener = TcpListener::bind(bind_addr).await.map_err(|e| {
            CtlError::address_in_use(format!("Failed to bind server on {bind_addr}: {e}"))
        })?;
        let auth_config = AuthConfig {
            state_dir: self.config_dir.join("auth"),
            ..AuthConfig::default()
        };
        let auth = AuthRuntime::from_process(auth_config)
            .await
            .map_err(|error| {
                CtlError::io(format!(
                    "Failed to initialize relay authentication: {error}"
                ))
            })?;

        tracing::info!("Starting pb-mapper server on {}:{}", ip_addr, port);

        let shutdown_token = CancellationToken::new();
        let shutdown_token_clone = shutdown_token.clone();

        let (status_sender, status_receiver) = tokio::sync::mpsc::unbounded_channel();

        let handle = tokio::spawn(async move {
            if let Err(e) = run_server_on_listener(
                listener,
                shutdown_token_clone,
                Some(status_receiver),
                enable_keep_alive,
                auth,
            )
            .await
            {
                tracing::error!("pb-mapper server stopped with error: {e}");
            }
        });

        self.server_handle = Some(handle);
        self.server_shutdown_token = Some(shutdown_token);
        self.server_status_sender = Some(status_sender);
        self.server_start_time = Some(SystemTime::now());

        {
            let mut cache = self.local_server_status_cache.write().await;
            *cache = LocalServerStatus {
                is_running: true,
                active_connections: 0,
                registered_services: 0,
                uptime_seconds: 0,
            };
        }
        {
            let mut last_update = self.local_server_status_last_update.write().await;
            *last_update = Some(Instant::now());
        }

        tracing::info!("pb-mapper server started successfully");
        Ok(())
    }

    pub async fn stop_server(&mut self) -> Result<(), CtlError> {
        if let (Some(handle), Some(shutdown_token)) =
            (self.server_handle.take(), self.server_shutdown_token.take())
        {
            self.server_status_sender = None;

            shutdown_token.cancel();

            let shutdown_timeout = tokio::time::Duration::from_secs(5);

            match tokio::time::timeout(shutdown_timeout, handle).await {
                Ok(_) => {
                    tracing::info!("Server shutdown gracefully");
                }
                Err(_) => {
                    tracing::warn!("Server shutdown timed out, may not have closed gracefully");
                }
            }

            self.server_start_time = None;

            {
                let mut cache = self.local_server_status_cache.write().await;
                *cache = LocalServerStatus {
                    is_running: false,
                    active_connections: 0,
                    registered_services: 0,
                    uptime_seconds: 0,
                };
            }
            {
                let mut last_update = self.local_server_status_last_update.write().await;
                *last_update = Some(Instant::now());
            }

            for (_, handle) in self.service_handles.drain() {
                handle.abort();
            }

            for (_, handle) in self.client_handles.drain() {
                handle.abort();
            }

            self.registered_services.write().await.clear();
            self.active_connections.write().await.clear();

            tracing::info!("pb-mapper server stopped, all services and connections terminated");
            Ok(())
        } else {
            Err(CtlError::not_found("Server is not running"))
        }
    }

    pub(super) async fn finish_register(&mut self, commit: RegisterCommit) -> Result<(), CtlError> {
        let RegisterCommit {
            service_key,
            local_address,
            protocol,
            enable_encryption,
            enable_keep_alive,
            local_sock_addr,
            remote_sock_addr,
        } = commit;

        if let Some(previous) = self.service_handles.remove(&service_key) {
            tracing::warn!(
                "Service '{service_key}' is already registered, replacing existing handle"
            );
            // Dropping a `JoinHandle` does not stop the task. Without this the
            // replaced tunnel kept running and retrying, with nothing left
            // holding a handle able to abort it.
            previous.abort();
        }

        tracing::info!(
            "Registering service '{}' with protocol {}, local address {}, server address {}",
            service_key,
            protocol,
            local_address,
            self.config.server_address
        );

        self.save_service_config(
            &service_key,
            &local_address,
            &protocol,
            enable_encryption,
            enable_keep_alive,
        )
        .map_err(|e| CtlError::io(format!("Failed to save service configuration: {e}")))?;

        let key_clone = service_key.clone();
        let service_key_for_status = service_key.clone();

        let callback: StatusCallback = Box::new(move |status: &str| {
            tracing::info!(
                "Service {} status update: {}",
                service_key_for_status,
                status
            );
        });

        let handle = if protocol.to_uppercase() == "TCP" {
            tokio::spawn(async move {
                let _ = run_server_side_cli_with_callback::<TcpStreamProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    ServerTunnelOptions {
                        need_codec: enable_encryption,
                        is_datagram: false,
                        keep_alive: enable_keep_alive,
                        namespace: None,
                        force_namespace: false,
                    },
                    Some(callback),
                )
                .await;
            })
        } else {
            tokio::spawn(async move {
                let _ = run_server_side_cli_with_callback::<UdpStreamProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    ServerTunnelOptions {
                        need_codec: enable_encryption,
                        is_datagram: true,
                        keep_alive: enable_keep_alive,
                        namespace: None,
                        force_namespace: false,
                    },
                    Some(callback),
                )
                .await;
            })
        };

        self.service_handles.insert(service_key.clone(), handle);

        {
            let mut cache = self.service_status_cache.write().await;
            cache.insert(
                service_key.clone(),
                StatusCacheEntry {
                    status: "retrying".to_string(),
                    message: "Connecting to pb-mapper server...".to_string(),
                    updated_at: Instant::now(),
                },
            );
        }
        self.schedule_service_status_refresh(&service_key).await;

        let service_info = ServiceInfo {
            service_key: service_key.clone(),
            protocol,
            local_address,
            status: "Registering".to_string(),
        };

        self.registered_services
            .write()
            .await
            .insert(service_key.clone(), service_info);

        tracing::info!("Service '{}' registration initiated", service_key);
        Ok(())
    }

    pub async fn unregister_service(&mut self, service_key: String) -> Result<(), CtlError> {
        if let Some(handle) = self.service_handles.remove(&service_key) {
            handle.abort();
        }

        if self
            .registered_services
            .write()
            .await
            .remove(&service_key)
            .is_some()
        {
            tracing::info!("Service '{}' unregistered successfully", service_key);
            Ok(())
        } else {
            Err(CtlError::not_found(format!(
                "Service '{service_key}' is not registered"
            )))
        }
    }

    pub async fn delete_service_config_and_stop(
        &mut self,
        service_key: String,
    ) -> Result<(), CtlError> {
        if let Some(handle) = self.service_handles.remove(&service_key) {
            handle.abort();
        }

        self.registered_services.write().await.remove(&service_key);

        self.delete_service_config(&service_key)
    }

    pub(super) async fn finish_connect(&mut self, commit: ConnectCommit) -> Result<(), CtlError> {
        let ConnectCommit {
            service_key,
            local_address,
            protocol,
            enable_keep_alive,
            local_sock_addr,
            remote_sock_addr,
        } = commit;

        if let Some(previous) = self.client_handles.remove(&service_key) {
            tracing::warn!(
                "Client for service '{service_key}' is already connected, replacing handle"
            );
            // As in `finish_register`: dropping the handle leaves the old
            // client's retry loop running with nothing able to stop it.
            previous.abort();
        }

        let protocol_upper = protocol.to_uppercase();

        tracing::info!(
            "Connecting to service '{}' with protocol {}, local address {}, server address {}",
            service_key,
            protocol,
            local_address,
            self.config.server_address
        );

        let key_clone = service_key.clone();

        let status_callback: ClientStatusCallback = {
            let service_key_for_callback = service_key.clone();
            Box::new(move |status: &str| {
                tracing::info!("Client {} status: {}", service_key_for_callback, status);
            })
        };

        let handle = if protocol_upper == "TCP" {
            tokio::spawn(async move {
                run_client_side_cli_with_callback::<TcpListenerProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    enable_keep_alive,
                    Some(status_callback),
                )
                .await;
            })
        } else {
            tokio::spawn(async move {
                run_client_side_cli_with_callback::<UdpListenerProvider, _>(
                    local_sock_addr,
                    remote_sock_addr,
                    key_clone.into(),
                    enable_keep_alive,
                    Some(status_callback),
                )
                .await;
            })
        };

        self.client_handles.insert(service_key.clone(), handle);

        {
            let mut cache = self.client_status_cache.write().await;
            cache.insert(
                service_key.clone(),
                StatusCacheEntry {
                    status: "retrying".to_string(),
                    message: "Connecting to pb-mapper server...".to_string(),
                    updated_at: Instant::now(),
                },
            );
        }
        self.schedule_client_status_refresh(&service_key).await;

        let connection_info = ConnectionInfo {
            service_key: service_key.clone(),
            client_id: format!("client-{service_key}"),
            status: "Connected".to_string(),
        };

        self.active_connections
            .write()
            .await
            .insert(service_key.clone(), connection_info);

        // Persist here rather than at the FFI boundary, so a connection made
        // from a terminal is remembered exactly like one made from the window.
        // `finish_register` has always done this; leaving it out here meant a
        // CLI `connect` started a client that never appeared in the list.
        if let Err(e) =
            self.save_client_config(&service_key, &local_address, &protocol, enable_keep_alive)
        {
            // The client is up either way, so this is a warning and not a
            // failure: losing the config costs the entry after a restart.
            tracing::warn!("Failed to save client config for '{service_key}': {e}");
        }

        tracing::info!("Connected to service '{}' successfully", service_key);
        Ok(())
    }

    /// Claims a service key for a registration. See [`KeyClaim`].
    pub(super) fn claim_registering(&self, service_key: &str) -> Result<KeyClaim, CtlError> {
        claim_key(&self.registering, service_key, "being registered")
    }

    /// Claims a service key for a client connection. See [`KeyClaim`].
    pub(super) fn claim_connecting(&self, service_key: &str) -> Result<KeyClaim, CtlError> {
        claim_key(&self.connecting, service_key, "being connected")
    }

    pub async fn disconnect_service(&mut self, service_key: String) -> Result<(), CtlError> {
        // Aborting the task is the part that matters: it is what stops the
        // retry loop still dialling in the background.
        let aborted = match self.client_handles.remove(&service_key) {
            Some(handle) => {
                handle.abort();
                true
            }
            None => false,
        };

        let was_listed = self
            .active_connections
            .write()
            .await
            .remove(&service_key)
            .is_some();

        // Reported failure only when there was nothing to stop. It used to key
        // off the bookkeeping map alone, so a client whose task had been
        // aborted could still be reported as "not connected" — an error for an
        // operation that had in fact just done its job.
        if aborted || was_listed {
            tracing::info!("Disconnected from service '{}'", service_key);
            Ok(())
        } else {
            Err(CtlError::not_found(format!(
                "Service '{service_key}' is not connected"
            )))
        }
    }

    pub async fn delete_client_config_and_stop(
        &mut self,
        service_key: String,
    ) -> Result<(), CtlError> {
        if let Some(handle) = self.client_handles.remove(&service_key) {
            handle.abort();
        }

        self.active_connections.write().await.remove(&service_key);

        self.delete_client_config(&service_key)
    }
}
