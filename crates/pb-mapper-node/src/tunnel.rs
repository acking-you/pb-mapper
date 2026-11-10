//! The `Registration` and `Connection` classes: live tunnels, as JS holds them.

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pb_mapper_client::sdk::{self, TunnelStatus};

use crate::convert::{invalid_argument, status_label, to_napi};
use crate::diagnostics::{JsTunnelDiagnostics, JsTunnelMetrics};
use crate::readers::{TunnelEvents, TunnelMetricsReader, period};

/// The status stream behind `nextStatus`. Held per handle, so a change that
/// lands between two calls is reported by the second instead of missed.
type StatusFeed = tokio::sync::Mutex<tokio::sync::watch::Receiver<TunnelStatus>>;

/// A live `register` tunnel: a local service published on the relay.
#[napi]
pub struct Registration {
    inner: Arc<sdk::Registration>,
    status: StatusFeed,
}

/// A live `connect` tunnel: a local listener forwarding to a registered service.
#[napi]
pub struct Connection {
    inner: Arc<sdk::Connection>,
    status: StatusFeed,
}

/// Implements the JS surface of one tunnel handle.
///
/// [`Registration`] and [`Connection`] stay distinct types on both sides of the
/// boundary — neither may be passed where the other is meant — but their JS
/// surface is identical down to the last method, so it is written once here.
///
/// The structs themselves are declared above rather than generated: napi copies
/// doc comments into `index.d.ts`, and a comment forwarded through a macro
/// argument arrives there mangled.
///
/// Both hold the handle in an `Arc`, because napi passes `&self` to an
/// `async fn` and the returned future must not borrow the wrapper.
macro_rules! tunnel_class {
    ($name:ident wrapping $handle:ty) => {
        impl $name {
            pub(crate) fn new(inner: $handle) -> Self {
                Self {
                    status: tokio::sync::Mutex::new(inner.subscribe()),
                    inner: Arc::new(inner),
                }
            }
        }

        #[napi]
        impl $name {
            /// The service key this tunnel is bound to.
            #[napi]
            pub fn key(&self) -> String {
                self.inner.key().to_string()
            }

            /// The tunnel's latest status: `starting`, `connected`, `retrying`,
            /// `stopped`, or `failed:<reason>`.
            #[napi]
            pub fn status(&self) -> String {
                status_label(&self.inner.status())
            }

            /// Inspect the running SDK version, failure phase, bounded setup
            /// load, the direct attempt in flight, and recent events.
            #[napi]
            pub fn diagnostics(&self) -> JsTunnelDiagnostics {
                self.inner.diagnostics().into()
            }

            /// The outcomes kept in `diagnostics().events`, then every event
            /// as it happens, until the tunnel stops. Each call is a reader of
            /// its own.
            #[napi]
            pub fn subscribe_events(&self) -> TunnelEvents {
                TunnelEvents {
                    events: tokio::sync::Mutex::new(self.inner.subscribe_events()),
                    closed: tokio_util::sync::CancellationToken::new(),
                }
            }

            /// Its sessions, bytes per path, relay health and direct link,
            /// sampled now: the part of `diagnostics()` that keeps changing.
            #[napi]
            pub fn metrics(&self) -> JsTunnelMetrics {
                self.inner.metrics().into()
            }

            /// `metrics()` at once, then every `everyMs` (one second when
            /// absent; 100 ms to an hour) and whenever the tunnel changes,
            /// until it stops. One second suits a panel on screen; lists
            /// and logs need five or more.
            #[napi]
            pub fn subscribe_metrics(&self, every_ms: Option<u32>) -> TunnelMetricsReader {
                TunnelMetricsReader {
                    stream: tokio::sync::Mutex::new(self.inner.subscribe_metrics(period(every_ms))),
                    closed: tokio_util::sync::CancellationToken::new(),
                }
            }

            /// Resolves with the next status this tunnel reports, rendered as
            /// `status()` renders it: an event listener for status changes.
            ///
            /// Each handle keeps its own position, so a change that happens
            /// between two calls is returned by the next one rather than
            /// missed. Once the tunnel settles as `stopped` or
            /// `failed:<reason>`, every call resolves at once with that final
            /// status, which is what ends a listening loop. Rejects after
            /// `timeoutMs` without a change.
            #[napi]
            pub async fn next_status(&self, timeout_ms: Option<u32>) -> Result<String> {
                let mut status = self.status.lock().await;
                let fresh = status.has_changed().unwrap_or(false);
                let settled = matches!(
                    *status.borrow(),
                    TunnelStatus::Stopped | TunnelStatus::Failed(_)
                );
                // An unseen change answers at once; so does a settled tunnel,
                // which never reports again.
                if !fresh && !settled {
                    let changed = status.changed();
                    // `Err` means the worker is gone and its last status is final.
                    let _ = match timeout_ms {
                        Some(ms) => {
                            tokio::time::timeout(Duration::from_millis(u64::from(ms)), changed)
                                .await
                                .map_err(|_| {
                                    invalid_argument(format!("no status change within {ms} ms"))
                                })?
                        }
                        None => changed.await,
                    };
                }
                Ok(status_label(&status.borrow_and_update()))
            }

            /// Resolves once the tunnel is connected, and rejects if it fails or
            /// is stopped first.
            ///
            /// Without `timeoutMs` this waits indefinitely, which is only safe
            /// because a tunnel the relay refuses permanently settles as failed
            /// rather than retrying forever.
            #[napi]
            pub async fn wait_ready(&self, timeout_ms: Option<u32>) -> Result<()> {
                match timeout_ms {
                    Some(ms) => self
                        .inner
                        .wait_ready_timeout(Duration::from_millis(u64::from(ms)))
                        .await
                        .map_err(to_napi),
                    None => self.inner.wait_ready().await.map_err(to_napi),
                }
            }

            /// Cancels the tunnel's worker and waits for it to unwind.
            #[napi]
            pub async fn stop(&self) -> Result<()> {
                self.inner.stop().await.map_err(to_napi)
            }
        }
    };
}

tunnel_class!(Registration wrapping sdk::Registration);
tunnel_class!(Connection wrapping sdk::Connection);
