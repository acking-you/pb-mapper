//! Readers that JS pulls one item at a time: tunnel events, tunnel metrics
//! and relay metrics. Nothing queues on the JS side while no one reads.

use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pb_mapper_client::sdk;

use crate::diagnostics::{JsRelayDiagnostics, JsTunnelEvent, JsTunnelMetrics};

/// One `next()` of a reader: `receive`, unless the reader was closed first,
/// which resolves with `null`, or `timeout_ms` passed, a rejection naming
/// `what` that loses nothing, `receive` being cancel-safe.
async fn read_next<T>(
    closed: &tokio_util::sync::CancellationToken,
    timeout_ms: Option<u32>,
    what: &str,
    receive: impl std::future::Future<Output = Result<Option<T>>>,
) -> Result<Option<T>> {
    let receive = async {
        match timeout_ms {
            Some(ms) => tokio::time::timeout(Duration::from_millis(ms.into()), receive)
                .await
                .map_err(|_| Error::from_reason(format!("no {what} within {ms} ms")))?,
            None => receive.await,
        }
    };
    tokio::select! {
        biased;
        () = closed.cancelled() => Ok(None),
        result = receive => result,
    }
}

/// A reader's period: `everyMs`, one second when absent.
pub(crate) fn period(every_ms: Option<u32>) -> Duration {
    Duration::from_millis(every_ms.map_or(1_000, u64::from))
}

/// A tunnel's events, read one at a time: the outcomes it kept, then every
/// event as it happens. Nothing queues on the JS side while no one reads.
#[napi]
pub struct TunnelEvents {
    pub(crate) events: tokio::sync::Mutex<sdk::TunnelEvents>,
    pub(crate) closed: tokio_util::sync::CancellationToken,
}

#[napi]
impl TunnelEvents {
    /// The next event, or `null` once the tunnel has stopped or `close()` was
    /// called. Rejects when this reader fell more than 128 events behind
    /// (read `diagnostics()` and keep reading), and when `timeoutMs` passes
    /// first, which loses no event. Calls are served one at a time.
    #[napi]
    pub async fn next(&self, timeout_ms: Option<u32>) -> Result<Option<JsTunnelEvent>> {
        read_next(&self.closed, timeout_ms, "tunnel event", async {
            match self.events.lock().await.recv().await {
                Some(Ok(event)) => Ok(Some(event.into())),
                Some(Err(lagged)) => Err(Error::from_reason(format!(
                    "{lagged}; read diagnostics() and keep reading"
                ))),
                None => Ok(None),
            }
        })
        .await
    }

    /// Stop reading: a pending `next()` resolves with `null`, as do later ones.
    #[napi]
    pub fn close(&self) {
        self.closed.cancel();
    }
}

/// A tunnel's metrics, sampled as they are read: one at once, then one a
/// period and one whenever a session opens or closes or the tunnel changes,
/// never two within 100 ms. Each is taken when `next()` resolves, so nothing
/// queues and a slow reader gets fewer, fresher samples.
#[napi]
pub struct TunnelMetricsReader {
    pub(crate) stream: tokio::sync::Mutex<sdk::TunnelMetricsStream>,
    pub(crate) closed: tokio_util::sync::CancellationToken,
}

#[napi]
impl TunnelMetricsReader {
    /// The next sample, or `null` once the tunnel has stopped (after one
    /// last sample) or `close()` was called. Rejects when `timeoutMs` passes
    /// first, which loses nothing. Calls are served one at a time.
    #[napi]
    pub async fn next(&self, timeout_ms: Option<u32>) -> Result<Option<JsTunnelMetrics>> {
        read_next(&self.closed, timeout_ms, "tunnel metrics", async {
            Ok(self.stream.lock().await.recv().await.map(Into::into))
        })
        .await
    }

    /// Stop reading: a pending `next()` resolves with `null`, as do later ones.
    #[napi]
    pub fn close(&self) {
        self.closed.cancel();
    }
}

/// A relay carrier's diagnostics, sampled as they are read: one at once,
/// then one a period, for as long as the client is kept.
#[napi]
pub struct RelayMetricsReader {
    pub(crate) stream: tokio::sync::Mutex<sdk::RelayMetricsStream>,
    pub(crate) closed: tokio_util::sync::CancellationToken,
}

#[napi]
impl RelayMetricsReader {
    /// The next sample, or `null` once the client is gone or `close()` was
    /// called. Rejects when `timeoutMs` passes first. Calls are served one
    /// at a time.
    #[napi]
    pub async fn next(&self, timeout_ms: Option<u32>) -> Result<Option<JsRelayDiagnostics>> {
        read_next(&self.closed, timeout_ms, "relay metrics", async {
            Ok(self.stream.lock().await.recv().await.map(Into::into))
        })
        .await
    }

    /// Stop reading: a pending `next()` resolves with `null`, as do later ones.
    #[napi]
    pub fn close(&self) {
        self.closed.cancel();
    }
}
