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

/// A live `register` tunnel: local service published on the relay.
pub struct Registration {
    inner: LiveTunnel,
    key: String,
}

impl Registration {
    pub(crate) fn new(inner: LiveTunnel, key: String) -> Self {
        Self { inner, key }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn status(&self) -> TunnelStatus {
        self.inner.status()
    }

    /// Subscribe to status changes. Useful for N-API event bridges.
    pub fn subscribe(&self) -> watch::Receiver<TunnelStatus> {
        self.inner.subscribe()
    }

    pub async fn wait_ready(&self) -> Result<()> {
        self.inner.wait_ready().await
    }

    pub async fn wait_ready_timeout(&self, timeout: Duration) -> Result<()> {
        self.inner.wait_ready_timeout(timeout).await
    }

    pub async fn stop(&self) -> Result<()> {
        self.inner.stop().await
    }
}

/// A live `connect` tunnel: local listener forwarding to a registered service.
pub struct Connection {
    inner: LiveTunnel,
    key: String,
}

impl Connection {
    pub(crate) fn new(inner: LiveTunnel, key: String) -> Self {
        Self { inner, key }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn status(&self) -> TunnelStatus {
        self.inner.status()
    }

    pub fn subscribe(&self) -> watch::Receiver<TunnelStatus> {
        self.inner.subscribe()
    }

    pub async fn wait_ready(&self) -> Result<()> {
        self.inner.wait_ready().await
    }

    pub async fn wait_ready_timeout(&self, timeout: Duration) -> Result<()> {
        self.inner.wait_ready_timeout(timeout).await
    }

    pub async fn stop(&self) -> Result<()> {
        self.inner.stop().await
    }
}
