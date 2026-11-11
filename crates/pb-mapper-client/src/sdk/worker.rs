//! The tunnel spawner: what `register` and `connect` hand a worker, and the
//! lifecycle every worker shares — spawn, publish status, settle as
//! `Stopped`.

use crate::diagnostics::Diagnostics;
use crate::endpoint::RelayEndpoint;
use std::net::SocketAddr;
use std::sync::Arc;

use pb_mapper_core::config::{ResolvedAddrs, resolve_addrs_async};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use snafu::ResultExt;

use super::Error;
use super::error::{AddressSnafu, Result};
use super::handle::LiveTunnel;
use super::types::TunnelStatus;
use super::{LanTransport, PortMapping};
use crate::direct_policy::DirectPolicy;
use crate::worker_status::StatusSink;

/// Resolve one endpoint to every address it names.
///
/// The list, not just its first entry: a worker handed the whole list dials each
/// candidate in turn, so a relay hostname with several records survives one of
/// them being unreachable.
async fn resolve(addr: &str) -> Result<ResolvedAddrs> {
    resolve_addrs_async(addr)
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
        TunnelStatus::Starting
        | TunnelStatus::Connected
        | TunnelStatus::Retrying
        | TunnelStatus::Stopped => {
            *status = TunnelStatus::Stopped;
            true
        }
    });
}

/// Publish what a worker of either tunnel direction reports on the handle's
/// watch channel.
fn watch_sink(tx: watch::Sender<TunnelStatus>) -> StatusSink {
    Box::new(move |status| {
        let _ = tx.send(TunnelStatus::from_worker(status));
    })
}

/// What a spawned tunnel worker is given: its resolved endpoints, its key, the
/// sink that publishes its status, and the token that stops it.
pub(super) struct WorkerContext {
    pub(super) local_addr: ResolvedAddrs,
    pub(super) remote_addr: RelayEndpoint,
    pub(super) key: Arc<str>,
    pub(super) status: StatusSink,
    pub(super) shutdown: CancellationToken,
    pub(super) diagnostics: Diagnostics,
    /// Where a connect tunnel tells its handle the address it listens on.
    pub(super) bound: watch::Sender<Option<SocketAddr>>,
}

/// A tunnel resolved and wired up, waiting only for the transport-specific
/// worker that will drive it.
///
/// Register and connect, over TCP and UDP, are four workers with four different
/// signatures but one lifecycle: spawn, publish status, settle as `Stopped`.
/// This owns that lifecycle so each call site is left with just its own call.
pub(super) struct TunnelWorker {
    local_addr: ResolvedAddrs,
    remote_addr: RelayEndpoint,
    key: Arc<str>,
    shutdown: CancellationToken,
    status_tx: watch::Sender<TunnelStatus>,
    status_rx: watch::Receiver<TunnelStatus>,
    diagnostics: Diagnostics,
}

impl TunnelWorker {
    /// Resolve the local endpoint and set up a recovering worker for the
    /// relay at `endpoint`.
    ///
    /// Shared by `register` and `connect`: the two differ only in which worker
    /// they hand the context to. Relay DNS remains retryable inside that worker.
    pub(super) async fn prepare(
        endpoint: &RelayEndpoint,
        key: &str,
        local_addr: &str,
    ) -> Result<Self> {
        if !endpoint.validate() {
            return Err(Error::invalid_config(
                "relay must be host:port or an IP socket address",
            ));
        }
        let local_addr = resolve(local_addr).await?;
        let remote_addr = endpoint.clone();
        let (status_tx, status_rx) = watch::channel(TunnelStatus::Starting);
        Ok(Self {
            local_addr,
            remote_addr,
            key: Arc::from(key),
            shutdown: CancellationToken::new(),
            status_tx,
            status_rx,
            diagnostics: Diagnostics::default(),
        })
    }

    /// How this tunnel's direct setups go, or `None` when it keeps to the
    /// relay: how it maps its direct endpoint's port and whether it tries a
    /// shared local network first, both noted in its diagnostics from the
    /// start either way; and what that network carries: what a consumer asks
    /// for, and whether a provider serves TCP.
    pub(super) fn direct_policy(
        &self,
        direct: bool,
        port_mapping: PortMapping,
        lan_first: bool,
        lan_transport: LanTransport,
        lan_tcp: bool,
    ) -> Option<DirectPolicy> {
        let port_mapping = if direct && cfg!(feature = "port-mapping") {
            port_mapping
        } else {
            PortMapping::Off
        };
        let lan_first = direct && lan_first;
        if port_mapping != PortMapping::Off {
            self.diagnostics.port_mapping_requested(
                port_mapping,
                crate::network::NetworkHints::shared().generation(),
            );
        }
        let lan_transport = if direct {
            lan_transport
        } else {
            LanTransport::Quic
        };
        self.diagnostics.prefer_lan(lan_first, lan_transport);
        direct.then_some(DirectPolicy {
            port_mapping,
            lan_first,
            lan_transport,
            lan_tcp,
        })
    }

    pub(super) fn spawn<F, Fut>(self, start: F) -> LiveTunnel
    where
        F: FnOnce(WorkerContext) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let Self {
            local_addr,
            remote_addr,
            key,
            shutdown,
            status_tx,
            status_rx,
            diagnostics,
        } = self;
        let worker_shutdown = shutdown.clone();
        let worker_diagnostics = diagnostics.clone();
        let endpoint = remote_addr.clone();
        let (bound, bound_rx) = watch::channel(None);
        let join = tokio::spawn(async move {
            start(WorkerContext {
                local_addr,
                remote_addr,
                key,
                status: watch_sink(status_tx.clone()),
                shutdown: worker_shutdown,
                diagnostics: worker_diagnostics.clone(),
                bound,
            })
            .await;
            worker_diagnostics.stop();
            settle_stopped(&status_tx);
        });
        LiveTunnel::new(shutdown, join, status_rx, bound_rx, endpoint, diagnostics)
    }
}
