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
    /// Run a relay on `port`. With `quic_port` it also takes QUIC there, on
    /// a UDP port of its own: the TCP port's UDP twin carries direct sessions.
    pub async fn start_server(
        &mut self,
        port: u16,
        enable_keep_alive: bool,
        quic_port: Option<u16>,
    ) -> Result<(), CtlError> {
        if self
            .server_handle
            .as_ref()
            .is_some_and(|h| !h.is_finished())
        {
            return Err(CtlError::already_exists("Server is already running"));
        }
        self.reap_finished_server().await;

        let ip_addr = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
        let bind_addr = std::net::SocketAddr::new(ip_addr, port);

        // Refused here, in words, rather than by a relay task that dies on
        // its own while the window shows it running.
        let quic = match quic_port {
            Some(quic_port) if quic_port == port => {
                return Err(CtlError::invalid_argument(format!(
                    "QUIC needs a UDP port of its own; {port} carries direct sessions"
                )));
            }
            Some(quic_port) => {
                let listen = std::net::SocketAddr::new(ip_addr, quic_port);
                std::net::UdpSocket::bind(listen).map(drop).map_err(|e| {
                    CtlError::address_in_use(format!(
                        "UDP port {quic_port} for QUIC is unavailable: {e}"
                    ))
                })?;
                Some(QuicServerConfig {
                    listen,
                    advertise_port: 0,
                })
            }
            None => None,
        };

        let listener = TcpListener::bind(bind_addr).await.map_err(|e| {
            CtlError::address_in_use(format!("Failed to bind server on {bind_addr}: {e}"))
        })?;
        let auth_config = AuthConfig {
            state_dir: self.config_dir.join("auth"),
            ..AuthConfig::default()
        };
        let auth = AuthRuntime::from_isolated_state(auth_config)
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

        // Direct sessions, on the same port over UDP, as `pb-mapper server`
        // offers them by default: tunnels through this relay may then go
        // direct. A UDP port someone else holds leaves it relaying only.
        let direct_listen = std::net::SocketAddr::new(ip_addr, port);
        let direct = match std::net::UdpSocket::bind(direct_listen) {
            Ok(socket) => {
                drop(socket);
                Some(DirectServerConfig {
                    listen: direct_listen,
                    // Only the bound port is announced: each client pairs it
                    // with the relay address it dialled, which is right
                    // wherever the UDP port is reached like the TCP one.
                    advertise: std::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
                })
            }
            Err(error) => {
                tracing::warn!("UDP port {port} unavailable, relaying only: {error}");
                None
            }
        };

        self.server_auth = Some(auth.clone());
        let handle = tokio::spawn(async move {
            if let Err(e) = run_server_on_listener_with_options(
                listener,
                shutdown_token_clone,
                Some(status_receiver),
                enable_keep_alive,
                auth,
                RelayServerOptions { direct, quic },
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
        self.server_port = Some(port);
        self.server_quic_port = quic_port;

        {
            let mut cache = self.local_server_status_cache.write().await;
            *cache = LocalServerStatus {
                is_running: true,
                port: Some(port),
                quic_port,
                ..LocalServerStatus::default()
            };
        }
        {
            let mut last_update = self.local_server_status_last_update.write().await;
            *last_update = Some(Instant::now());
        }

        tracing::info!("pb-mapper server started successfully");
        Ok(())
    }

    /// Clear what a relay that ended on its own left behind — a listener
    /// lost to another process, say — so that it can be started again.
    async fn reap_finished_server(&mut self) {
        if !self.server_handle.as_ref().is_some_and(|h| h.is_finished()) {
            return;
        }
        self.server_handle = None;
        self.server_shutdown_token = None;
        self.server_status_sender = None;
        if let Some(auth) = self.server_auth.take() {
            auth.shutdown_actor().await;
        }
        self.server_start_time = None;
        self.server_port = None;
        self.server_quic_port = None;
        *self.local_server_status_cache.write().await = LocalServerStatus::default();
    }

    pub async fn stop_server(&mut self) -> Result<(), CtlError> {
        if let (Some(handle), Some(shutdown_token)) =
            (self.server_handle.take(), self.server_shutdown_token.take())
        {
            self.server_status_sender = None;

            shutdown_token.cancel();

            let shutdown_timeout = tokio::time::Duration::from_secs(5);

            let mut handle = handle;
            match tokio::time::timeout(shutdown_timeout, &mut handle).await {
                Ok(_) => {
                    tracing::info!("Server shutdown gracefully");
                }
                Err(_) => {
                    if let Some(auth) = self.server_auth.as_ref()
                        && let Err(error) = auth.abort_actor().await
                    {
                        tracing::warn!(
                            "timed out waiting for the authentication actor to drop: {error}"
                        );
                    }
                    handle.abort();
                    let _ = handle.await;
                    tracing::warn!("Server shutdown timed out; aborted the relay task");
                }
            }
            if let Some(auth) = self.server_auth.take() {
                auth.shutdown_actor().await;
            }

            self.server_start_time = None;
            self.server_port = None;
            self.server_quic_port = None;

            {
                let mut cache = self.local_server_status_cache.write().await;
                *cache = LocalServerStatus::default();
            }
            {
                let mut last_update = self.local_server_status_last_update.write().await;
                *last_update = Some(Instant::now());
            }

            self.service_runtime.clear();
            self.client_runtime.clear();

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
            allow_direct,
            port_mapping,
            handle,
            pin,
        } = commit;

        tracing::info!(
            "Registering service '{}' with protocol {}, local address {}, server address {}",
            service_key,
            protocol,
            local_address,
            pin.relay
        );

        self.save_service_config(
            &service_key,
            &local_address,
            &protocol,
            enable_encryption,
            enable_keep_alive,
            DirectOptions {
                allow_direct,
                port_mapping,
            },
        )
        .map_err(|e| CtlError::io(format!("Failed to save service configuration: {e}")))?;

        let handle = Arc::new(TunnelHandle::Share(handle));
        watch_tunnel(&handle, events::ChangeKind::Services, &service_key);
        if self
            .service_runtime
            .insert(service_key.clone(), TunnelRuntime { handle, pin })
            .is_some()
        {
            tracing::warn!("Service '{service_key}' was already registered; replaced it");
        }

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

    /// This computer's tunnels on `relay`, sorted by what rotating its
    /// administrator key away from `retired` leaves them. Each keeps the
    /// credential it started with, whatever the configuration says now. A
    /// tunnel is on `relay` if it was started under that address, or dials
    /// any address `endpoint` resolves to: the same relay may have been
    /// written another way (a hostname, its IP) when it started.
    pub fn tunnels_on(
        &self,
        relay: &str,
        endpoint: Option<&ResolvedAddrs>,
        retired: Credential,
    ) -> RelayTunnels {
        let mut found = RelayTunnels::default();
        for (runtimes, restart) in [
            (&self.service_runtime, &mut found.shares),
            (&self.client_runtime, &mut found.connections),
        ] {
            for (key, runtime) in runtimes {
                if !runtime.pin.is_on(relay, endpoint) {
                    continue;
                }
                if runtime.pin.credential == retired {
                    restart.push(key.clone());
                } else {
                    found.stranded.push(key.clone());
                }
            }
        }
        found.shares.sort();
        found.connections.sort();
        found.stranded.sort();
        found
    }

    /// This computer's tunnels on `relay` under `credential` that reach it
    /// over another carrier than `transport`: the ones to start again for
    /// them to move to it. A tunnel on another relay or credential is not
    /// this change's to move.
    pub fn on_other_carrier(
        &self,
        relay: &str,
        endpoint: Option<&ResolvedAddrs>,
        credential: Credential,
        transport: RelayTransport,
    ) -> RelayTunnels {
        let mut found = RelayTunnels::default();
        for (runtimes, moving) in [
            (&self.service_runtime, &mut found.shares),
            (&self.client_runtime, &mut found.connections),
        ] {
            for (key, runtime) in runtimes {
                if runtime.pin.is_on(relay, endpoint)
                    && runtime.pin.credential == credential
                    && runtime.handle.diagnostics().relay_transport != transport
                {
                    moving.push(key.clone());
                }
            }
        }
        found.shares.sort();
        found.connections.sort();
        found
    }

    pub async fn unregister_service(&mut self, service_key: String) -> Result<(), CtlError> {
        abort_runtime(&mut self.service_runtime, &service_key);

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
        abort_runtime(&mut self.service_runtime, &service_key);

        self.registered_services.write().await.remove(&service_key);

        self.delete_service_config(&service_key)
    }

    pub(super) async fn finish_connect(&mut self, commit: ConnectCommit) -> Result<(), CtlError> {
        let ConnectCommit {
            service_key,
            local_address,
            protocol,
            enable_keep_alive,
            allow_direct,
            port_mapping,
            handle,
            pin,
        } = commit;

        tracing::info!(
            "Connecting to service '{}' with protocol {}, local address {}, server address {}",
            service_key,
            protocol,
            local_address,
            pin.relay
        );

        let namespace = pin.namespace;
        let handle = Arc::new(TunnelHandle::Connect(handle));
        watch_tunnel(&handle, events::ChangeKind::Clients, &service_key);
        if self
            .client_runtime
            .insert(service_key.clone(), TunnelRuntime { handle, pin })
            .is_some()
        {
            tracing::warn!("Client for service '{service_key}' was already running; replaced it");
        }

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
        if let Err(e) = self.save_client_config(
            &service_key,
            &local_address,
            &protocol,
            enable_keep_alive,
            namespace,
            DirectOptions {
                allow_direct,
                port_mapping,
            },
        ) {
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
        let aborted = abort_runtime(&mut self.client_runtime, &service_key);

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
        abort_runtime(&mut self.client_runtime, &service_key);

        self.active_connections.write().await.remove(&service_key);

        self.delete_client_config(&service_key)
    }
}

/// Refresh on SDK transitions. A lag invalidates the snapshot too.
/// Receivers do not keep the tunnel alive; no polling timer is needed.
fn watch_tunnel(handle: &Arc<TunnelHandle>, kind: events::ChangeKind, key: &str) {
    let mut status = handle.subscribe();
    let mut diagnostics = handle.subscribe_diagnostics();
    let key = key.to_string();
    tokio::spawn(async move {
        let mut status_open = true;
        loop {
            let live = tokio::select! {
                changed = status.changed(), if status_open => {
                    status_open = changed.is_ok();
                    true
                },
                event = diagnostics.recv() => !matches!(event, Err(tokio::sync::broadcast::error::RecvError::Closed)),
            };
            events::emit(kind, Some(&key), Origin::Internal);
            if !live {
                return;
            }
        }
    });
}
