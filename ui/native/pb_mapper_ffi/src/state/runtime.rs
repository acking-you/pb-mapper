//! Runtime lifecycle for the embedded relay, registered services, and local clients.
//!
//! ```text
//! start relay: bind listener -> initialize app-local auth -> spawn -> mark running
//! register:    claim key -> resolve -> preflight -> SDK registration -> commit
//! connect:     claim key -> resolve -> bind check -> SDK connection -> commit
//! stop:        cancel the relay; drop the tunnels' handles, which stops them
//! ```
//!
//! Readiness is published only after both listener binding and authentication
//! initialization succeed, preventing the UI from displaying a phantom running relay.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use pb_mapper_auth::{AuthConfig, AuthRuntime};
use pb_mapper_client::sdk::{LanTransport, RelayTransport};
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::config::ResolvedAddrs;
use pb_mapper_server::{
    DirectServerConfig, QuicServerConfig, RelayServerOptions, ServerStatusInfo,
    run_server_on_listener_with_options,
};

use super::tunnel::{
    ConnectCommit, KeyClaim, RegisterCommit, TunnelHandle, TunnelRuntime, claim_key, stop_tunnel,
    watch_tunnel,
};
use super::{LocalServerStatus, PbMapperState, RelayTunnels};
use crate::error::CtlError;
use crate::events;

/// How long a stopping relay has to wind down before its task is aborted.
const SERVER_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The relay this computer runs itself, while it runs: each part exists
/// exactly as long as the others.
pub(super) struct EmbeddedRelay {
    pub(super) task: JoinHandle<()>,
    auth: AuthRuntime,
    shutdown: CancellationToken,
    /// Where the status read asks the relay for its figures.
    pub(super) status_requests: UnboundedSender<oneshot::Sender<ServerStatusInfo>>,
    pub(super) started_at: SystemTime,
    pub(super) port: u16,
    pub(super) quic_port: Option<u16>,
}

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
            .embedded_relay
            .as_ref()
            .is_some_and(|relay| !relay.task.is_finished())
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
            state_dir: self.auth_state_dir(),
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
        let direct = match DirectServerConfig::probe_relay_port(ip_addr, port) {
            Ok(direct) => Some(direct),
            Err(error) => {
                tracing::warn!("UDP port {port} unavailable, relaying only: {error}");
                None
            }
        };

        let task = tokio::spawn({
            let auth = auth.clone();
            async move {
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
            }
        });

        self.embedded_relay = Some(EmbeddedRelay {
            task,
            auth,
            shutdown: shutdown_token,
            status_requests: status_sender,
            started_at: SystemTime::now(),
            port,
            quic_port,
        });

        self.local_server_status.lock().record(LocalServerStatus {
            is_running: true,
            port: Some(port),
            quic_port,
            ..LocalServerStatus::default()
        });

        tracing::info!("pb-mapper server started successfully");
        Ok(())
    }

    /// Where the embedded relay keeps its authentication state, its
    /// administrator key among it.
    pub(super) fn auth_state_dir(&self) -> PathBuf {
        self.config_dir.join("auth")
    }

    /// Clear what a relay that ended on its own left behind — a listener
    /// lost to another process, say — so that it can be started again.
    async fn reap_finished_server(&mut self) {
        let Some(relay) = self
            .embedded_relay
            .take_if(|relay| relay.task.is_finished())
        else {
            return;
        };
        let EmbeddedRelay {
            task,
            auth,
            shutdown,
            status_requests,
            started_at: _,
            port: _,
            quic_port: _,
        } = relay;
        drop((task, shutdown, status_requests));
        auth.shutdown_actor().await;
        self.local_server_status.lock().status = LocalServerStatus::default();
    }

    pub async fn stop_server(&mut self) -> Result<(), CtlError> {
        let Some(EmbeddedRelay {
            mut task,
            auth,
            shutdown,
            status_requests,
            started_at: _,
            port: _,
            quic_port: _,
        }) = self.embedded_relay.take()
        else {
            return Err(CtlError::not_found("Server is not running"));
        };
        drop(status_requests);

        shutdown.cancel();

        match tokio::time::timeout(SERVER_STOP_TIMEOUT, &mut task).await {
            Ok(_) => {
                tracing::info!("Server shutdown gracefully");
            }
            Err(_) => {
                if let Err(error) = auth.abort_actor().await {
                    tracing::warn!(
                        "timed out waiting for the authentication actor to drop: {error}"
                    );
                }
                task.abort();
                let _ = task.await;
                tracing::warn!("Server shutdown timed out; aborted the relay task");
            }
        }
        auth.shutdown_actor().await;

        self.local_server_status
            .lock()
            .record(LocalServerStatus::default());

        self.service_runtime.clear();
        self.client_runtime.clear();

        tracing::info!("pb-mapper server stopped, all services and connections terminated");
        Ok(())
    }

    pub(super) fn finish_register(&mut self, commit: RegisterCommit) -> Result<(), CtlError> {
        let RegisterCommit { spec, handle, pin } = commit;

        tracing::info!(
            "Registering service '{}' with protocol {}, local address {}, server address {}",
            spec.service_key,
            spec.protocol,
            spec.local_address,
            pin.relay
        );

        self.save_service_config(&spec)
            .map_err(|e| CtlError::io(format!("Failed to save service configuration: {e}")))?;

        let service_key = spec.service_key;
        let handle = Arc::new(TunnelHandle::Share(handle));
        watch_tunnel(&handle, events::ChangeKind::Services, &service_key);
        if self
            .service_runtime
            .insert(service_key.clone(), TunnelRuntime { handle, pin })
            .is_some()
        {
            tracing::warn!("Service '{service_key}' was already registered; replaced it");
        }

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
    /// them to move to it.
    pub fn on_other_carrier(
        &self,
        relay: &str,
        endpoint: Option<&ResolvedAddrs>,
        credential: Credential,
        transport: RelayTransport,
    ) -> RelayTunnels {
        self.started_otherwise(relay, endpoint, credential, |runtime| {
            runtime.handle.diagnostics().relay_transport != transport
        })
    }

    /// This computer's tunnels on `relay` under `credential` that may go
    /// direct and started with LAN-first other than `lan_first`: the ones to
    /// start again for it to reach them. One kept on the relay never tries a
    /// local network, so no start changes it.
    pub fn on_other_lan_first(
        &self,
        relay: &str,
        endpoint: Option<&ResolvedAddrs>,
        credential: Credential,
        lan_first: bool,
    ) -> RelayTunnels {
        self.started_otherwise(relay, endpoint, credential, |runtime| {
            runtime.pin.allow_direct && runtime.handle.diagnostics().lan_first != lan_first
        })
    }

    /// This computer's connections on `relay` under `credential` that may
    /// go direct and started asking a shared local network for another
    /// carrier than `lan_transport`: the ones to start again for it to reach
    /// them. A share serves whichever carrier its consumers ask for, so no
    /// start changes it.
    pub fn on_other_lan_transport(
        &self,
        relay: &str,
        endpoint: Option<&ResolvedAddrs>,
        credential: Credential,
        lan_transport: LanTransport,
    ) -> RelayTunnels {
        let mut found = self.started_otherwise(relay, endpoint, credential, |runtime| {
            runtime.pin.allow_direct && runtime.handle.diagnostics().lan_transport != lan_transport
        });
        found.shares.clear();
        found
    }

    /// This computer's tunnels on `relay` under `credential` that `differs`
    /// picks. A start takes the relay and credential configured now, so a
    /// tunnel on another relay or credential is never named: starting it
    /// again would move it there, away from whoever reaches it where it is.
    fn started_otherwise(
        &self,
        relay: &str,
        endpoint: Option<&ResolvedAddrs>,
        credential: Credential,
        differs: impl Fn(&TunnelRuntime) -> bool,
    ) -> RelayTunnels {
        let mut found = RelayTunnels::default();
        for (runtimes, named) in [
            (&self.service_runtime, &mut found.shares),
            (&self.client_runtime, &mut found.connections),
        ] {
            for (key, runtime) in runtimes {
                if runtime.pin.is_on(relay, endpoint)
                    && runtime.pin.credential == credential
                    && differs(runtime)
                {
                    named.push(key.clone());
                }
            }
        }
        found.shares.sort();
        found.connections.sort();
        found
    }

    pub fn unregister_service(&mut self, service_key: String) -> Result<(), CtlError> {
        if stop_tunnel(&mut self.service_runtime, &service_key) {
            tracing::info!("Service '{}' unregistered successfully", service_key);
            Ok(())
        } else {
            Err(CtlError::not_found(format!(
                "Service '{service_key}' is not registered"
            )))
        }
    }

    pub fn delete_service_config_and_stop(&mut self, service_key: String) -> Result<(), CtlError> {
        stop_tunnel(&mut self.service_runtime, &service_key);
        self.delete_service_config(&service_key)
    }

    pub(super) fn finish_connect(&mut self, commit: ConnectCommit) -> Result<(), CtlError> {
        let ConnectCommit { spec, handle, pin } = commit;

        tracing::info!(
            "Connecting to service '{}' with protocol {}, local address {}, server address {}",
            spec.service_key,
            spec.protocol,
            spec.local_address,
            pin.relay
        );

        let service_key = &spec.service_key;
        let handle = Arc::new(TunnelHandle::Connect(handle));
        watch_tunnel(&handle, events::ChangeKind::Clients, service_key);
        if self
            .client_runtime
            .insert(service_key.clone(), TunnelRuntime { handle, pin })
            .is_some()
        {
            tracing::warn!("Client for service '{service_key}' was already running; replaced it");
        }

        // Persist here rather than at the FFI boundary, so a connection made
        // from a terminal is remembered exactly like one made from the window.
        // `finish_register` has always done this; leaving it out here meant a
        // CLI `connect` started a client that never appeared in the list.
        if let Err(e) = self.save_client_config(&spec) {
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

    /// Stops the connection under `service_key`, which also stops its retry
    /// loop still dialling in the background; failing only when there was
    /// nothing to stop.
    pub fn disconnect_service(&mut self, service_key: String) -> Result<(), CtlError> {
        if stop_tunnel(&mut self.client_runtime, &service_key) {
            tracing::info!("Disconnected from service '{}'", service_key);
            Ok(())
        } else {
            Err(CtlError::not_found(format!(
                "Service '{service_key}' is not connected"
            )))
        }
    }

    pub fn delete_client_config_and_stop(&mut self, service_key: String) -> Result<(), CtlError> {
        stop_tunnel(&mut self.client_runtime, &service_key);
        self.delete_client_config(&service_key)
    }
}
