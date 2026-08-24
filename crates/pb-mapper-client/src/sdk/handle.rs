//! Handles for a running tunnel: [`Registration`] and [`Connection`].
//!
//! Both wrap the same `LiveTunnel` — a cancellation token, the worker's join
//! handle, and the status channel it publishes to. A handle observes and stops
//! its tunnel; it never drives the traffic itself.

use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{Error, Result, TunnelStatus};

pub(crate) struct LiveTunnel {
    shutdown: CancellationToken,
    join: Mutex<Option<JoinHandle<()>>>,
    status: watch::Receiver<TunnelStatus>,
}

impl LiveTunnel {
    pub(crate) fn new(
        shutdown: CancellationToken,
        join: JoinHandle<()>,
        status: watch::Receiver<TunnelStatus>,
    ) -> Self {
        Self {
            shutdown,
            join: Mutex::new(Some(join)),
            status,
        }
    }

    pub(crate) fn status(&self) -> TunnelStatus {
        self.status.borrow().clone()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<TunnelStatus> {
        self.status.clone()
    }

    pub(crate) async fn wait_ready(&self) -> Result<()> {
        wait_for_connected(&mut self.status.clone()).await
    }

    pub(crate) async fn wait_ready_timeout(&self, timeout: Duration) -> Result<()> {
        match tokio::time::timeout(timeout, self.wait_ready()).await {
            Ok(result) => result,
            Err(_) => Err(Error::ReadyTimeout { timeout }),
        }
    }

    pub(crate) async fn stop(&self) -> Result<()> {
        self.shutdown.cancel();
        let handle = self
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(mut handle) = handle {
            match tokio::time::timeout(Duration::from_secs(5), &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(join_error)) if join_error.is_cancelled() => {}
                Ok(Err(join_error)) => {
                    return Err(Error::protocol(format!("tunnel task failed: {join_error}")));
                }
                Err(_) => {
                    handle.abort();
                    let _ = handle.await;
                }
            }
        }
        Ok(())
    }
}

impl Drop for LiveTunnel {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(handle) = self
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            handle.abort();
        }
    }
}

async fn wait_for_connected(status: &mut watch::Receiver<TunnelStatus>) -> Result<()> {
    loop {
        match status.borrow().clone() {
            TunnelStatus::Connected => return Ok(()),
            TunnelStatus::Failed(reason) => return Err(Error::TunnelFailed { reason }),
            TunnelStatus::Stopped => return Err(Error::Stopped),
            TunnelStatus::Starting | TunnelStatus::Retrying => {}
        }
        status.changed().await.map_err(|_| Error::Stopped)?;
    }
}

/// Declares one end of a live tunnel as a public handle.
///
/// [`Registration`] and [`Connection`] share a lifecycle down to the last method:
/// the same status, the same readiness wait, the same stop. They stay distinct
/// types so neither can be passed where the other is meant, and this macro is
/// what keeps the two from drifting apart.
macro_rules! tunnel_handle {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        pub struct $name {
            inner: LiveTunnel,
            key: String,
        }

        impl $name {
            pub(crate) fn new(inner: LiveTunnel, key: String) -> Self {
                Self { inner, key }
            }

            /// The service key this tunnel is bound to.
            pub fn key(&self) -> &str {
                &self.key
            }

            /// The latest status its worker reported.
            pub fn status(&self) -> TunnelStatus {
                self.inner.status()
            }

            /// Subscribe to status changes. Useful for N-API event bridges.
            pub fn subscribe(&self) -> watch::Receiver<TunnelStatus> {
                self.inner.subscribe()
            }

            /// Resolve once the tunnel is connected, or fails, or stops.
            pub async fn wait_ready(&self) -> Result<()> {
                self.inner.wait_ready().await
            }

            /// [`Self::wait_ready`], bounded by `timeout`.
            pub async fn wait_ready_timeout(&self, timeout: Duration) -> Result<()> {
                self.inner.wait_ready_timeout(timeout).await
            }

            /// Cancel the worker and wait for it to unwind.
            pub async fn stop(&self) -> Result<()> {
                self.inner.stop().await
            }
        }
    };
}

tunnel_handle!(
    /// A live `register` tunnel: a local service published on the relay.
    Registration
);

tunnel_handle!(
    /// A live `connect` tunnel: a local listener forwarding to a registered service.
    Connection
);
