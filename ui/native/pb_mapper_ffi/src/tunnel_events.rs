//! A running tunnel's events, and its metrics, read by the window one at a
//! time.
//!
//! One export, [`pb_mapper_tunnel_events_call_json`], takes `{"op": …, …}`
//! as `pb_mapper_relay_call_json` does:
//!
//! - `subscribe {kind: "share" | "connect", key, metrics_ms?}` → `{id,
//!   diagnostics, metrics}`. With `metrics_ms`, the reader also samples the
//!   tunnel's metrics that often, and whenever a session opens or closes;
//! - `next {id, timeout_ms}` waits up to `timeout_ms` (at most 30 s) and
//!   answers `{type: "event", event, diagnostics, metrics}`, `{type:
//!   "metrics", metrics}` when a sample is due, `{type: "lagged", skipped,
//!   diagnostics, metrics}` when the reader fell behind, `{type: "idle"}`,
//!   or `{type: "closed"}` once the tunnel has stopped or the reader closed;
//! - `close {id}` ends a reader, waking a pending `next`.
//!
//! Each event answer carries the tunnel's diagnostics and metrics as they
//! are by then, so a reader never keeps a state of its own; a metrics answer
//! carries only the sample, which is what keeps a panel's figures live
//! between events. A reader holds its tunnel weakly: it never keeps one
//! running. Calls run on Dart's worker isolates, so the handle must outlive
//! every call in flight.

use std::{
    collections::HashMap,
    ffi::c_char,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use pb_mapper_client::sdk::{
    TunnelDiagnostics, TunnelEvent, TunnelEvents, TunnelMetrics, TunnelMetricsStream,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    error::CtlError,
    handle::PbMapperHandle,
    response::{Reply, parse_c_string, respond},
    state::TunnelHandle,
};

/// More readers than any window opens; past it, one was leaked.
const MAX_READERS: usize = 256;

/// The longest a `next` may wait, so a worker isolate is never parked for long.
const MAX_WAIT: Duration = Duration::from_secs(30);

/// The window's open readers, by id.
#[derive(Default)]
pub(crate) struct TunnelEventReaders {
    last_id: AtomicU64,
    open: parking_lot::Mutex<HashMap<u64, Arc<Reader>>>,
}

impl Drop for TunnelEventReaders {
    fn drop(&mut self) {
        for reader in self.open.get_mut().values() {
            reader.closed.cancel();
        }
    }
}

struct Reader {
    events: tokio::sync::Mutex<TunnelEvents>,
    /// Present when the reader asked for samples.
    metrics: Option<tokio::sync::Mutex<TunnelMetricsStream>>,
    tunnel: Weak<TunnelHandle>,
    closed: CancellationToken,
}

impl Reader {
    fn diagnostics(&self) -> Option<TunnelDiagnostics> {
        self.tunnel.upgrade().map(|tunnel| tunnel.diagnostics())
    }

    fn metrics(&self) -> Option<TunnelMetrics> {
        self.tunnel.upgrade().map(|tunnel| tunnel.metrics())
    }

    /// The next event, or a sample when one is due first. An event wins a
    /// tie; once the samples end, with the tunnel, only events are left.
    async fn receive(&self) -> Received {
        let mut events = self.events.lock().await;
        let mut samples = match &self.metrics {
            Some(metrics) => Some(metrics.lock().await),
            None => None,
        };
        loop {
            let sample = async {
                match samples.as_mut() {
                    Some(samples) => samples.recv().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                event = events.recv() => return Received::Event(event),
                sample = sample => match sample {
                    Some(sample) => return Received::Sample(Box::new(sample)),
                    None => samples = None,
                },
            }
        }
    }
}

enum Received {
    Event(Option<Result<TunnelEvent, pb_mapper_client::sdk::EventsLagged>>),
    Sample(Box<TunnelMetrics>),
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Subscribe {
        kind: TunnelKind,
        key: String,
        #[serde(default)]
        metrics_ms: Option<u32>,
    },
    Next {
        id: u64,
        timeout_ms: u32,
    },
    Close {
        id: u64,
    },
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TunnelKind {
    Share,
    Connect,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Next {
    Event {
        event: TunnelEvent,
        diagnostics: Option<TunnelDiagnostics>,
        metrics: Option<TunnelMetrics>,
    },
    Metrics {
        metrics: TunnelMetrics,
    },
    Lagged {
        skipped: u64,
        diagnostics: Option<TunnelDiagnostics>,
        metrics: Option<TunnelMetrics>,
    },
    Idle,
    Closed,
}

/// Runs one reader operation; see the module docs. Every answer is freed with
/// `pb_mapper_free_string`.
///
/// # Safety
///
/// `handle` must come from `pb_mapper_create` and outlive this call, and
/// `request` must be a NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_tunnel_events_call_json(
    handle: *mut PbMapperHandle,
    request: *const c_char,
) -> *mut c_char {
    // SAFETY: null or live, as the caller guarantees.
    respond(unsafe { handle.as_ref() }, |handle| {
        // SAFETY: null or NUL-terminated, as the caller guarantees.
        let request = unsafe { parse_c_string(request, "request") }.and_then(|raw| {
            serde_json::from_str::<Request>(&raw)
                .map_err(|e| CtlError::invalid_argument(e.to_string()))
        })?;
        Ok(Reply::Data(handle.runtime.block_on(call(handle, request))?))
    })
}

async fn call(handle: &PbMapperHandle, request: Request) -> Result<Value, CtlError> {
    let readers = &handle.tunnel_events;
    match request {
        Request::Subscribe {
            kind,
            key,
            metrics_ms,
        } => {
            let reader = {
                let state = handle.state.lock().await;
                let tunnel = state.running_tunnel(matches!(kind, TunnelKind::Share), &key)?;
                Reader {
                    events: tokio::sync::Mutex::new(tunnel.subscribe_events()),
                    metrics: metrics_ms.map(|ms| {
                        let every = Duration::from_millis(ms.into());
                        tokio::sync::Mutex::new(tunnel.subscribe_metrics(every))
                    }),
                    tunnel: Arc::downgrade(tunnel),
                    closed: CancellationToken::new(),
                }
            };
            let diagnostics = reader.diagnostics();
            // A stream's first sample comes at once: this answer takes it.
            let metrics = match &reader.metrics {
                Some(samples) => samples.lock().await.recv().await,
                None => reader.metrics(),
            };
            let mut open = readers.open.lock();
            if open.len() >= MAX_READERS {
                return Err(CtlError::invalid_argument("too many tunnel event readers"));
            }
            let id = readers.last_id.fetch_add(1, Ordering::Relaxed) + 1;
            open.insert(id, Arc::new(reader));
            Ok(json!({ "id": id, "diagnostics": diagnostics, "metrics": metrics }))
        }
        Request::Next { id, timeout_ms } => {
            let Some(reader) = readers.open.lock().get(&id).cloned() else {
                return to_value(Next::Closed);
            };
            let wait = Duration::from_millis(timeout_ms.into()).min(MAX_WAIT);
            let next = tokio::select! {
                biased;
                () = reader.closed.cancelled() => Next::Closed,
                received = tokio::time::timeout(wait, reader.receive()) => match received {
                    Ok(Received::Event(Some(Ok(event)))) => Next::Event {
                        event,
                        diagnostics: reader.diagnostics(),
                        metrics: reader.metrics(),
                    },
                    Ok(Received::Event(Some(Err(lagged)))) => Next::Lagged {
                        skipped: lagged.skipped,
                        diagnostics: reader.diagnostics(),
                        metrics: reader.metrics(),
                    },
                    Ok(Received::Event(None)) => Next::Closed,
                    Ok(Received::Sample(metrics)) => Next::Metrics { metrics: *metrics },
                    Err(_) => Next::Idle,
                },
            };
            to_value(next)
        }
        Request::Close { id } => {
            if let Some(reader) = readers.open.lock().remove(&id) {
                reader.closed.cancel();
            }
            to_value(Next::Closed)
        }
    }
}

fn to_value(next: Next) -> Result<Value, CtlError> {
    serde_json::to_value(next).map_err(|e| CtlError::internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_mapper_client::sdk::{
        Client, ClientConfig, RegisterOptions, RegisterRequest, Transport,
    };
    use std::{future::Future, task::Poll};

    use crate::test_support::{TempRoot, test_handle};

    #[test]
    fn closing_a_reader_wakes_its_pending_read_and_leaves_the_tunnel_running() {
        // A new state writes the process credential, so it is made under the
        // lock, in a directory of its own rather than the app's.
        let _credential_guard =
            pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK.blocking_lock();
        let root = TempRoot::new("tunnel-events");
        let handle = test_handle(root.path());
        handle.runtime.block_on(async {
            let relay = pb_mapper_testkit::Relay::start("ffi-tunnel-events").await;
            let client = Client::new(ClientConfig {
                server: relay.addr().to_string(),
                credential: pb_mapper_testkit::TEST_ADMIN_KEY.into(),
                keep_alive: false,
                namespace: None,
            })
            .unwrap();
            let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let registration = client
                .register_with_options(
                    RegisterRequest {
                        key: "idle".into(),
                        local_addr: backend.local_addr().unwrap().to_string(),
                        transport: Transport::Tcp,
                        codec: false,
                        force_namespace: false,
                    },
                    RegisterOptions {
                        allow_direct: false,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            registration
                .wait_ready_timeout(Duration::from_secs(5))
                .await
                .unwrap();
            let tunnel = Arc::new(TunnelHandle::Share(registration));
            let reader = |metrics: Option<u64>| {
                Arc::new(Reader {
                    events: tokio::sync::Mutex::new(tunnel.subscribe_events()),
                    metrics: metrics.map(|ms| {
                        tokio::sync::Mutex::new(tunnel.subscribe_metrics(Duration::from_millis(ms)))
                    }),
                    tunnel: Arc::downgrade(&tunnel),
                    closed: CancellationToken::new(),
                })
            };
            handle.tunnel_events.open.lock().insert(1, reader(None));
            handle
                .tunnel_events
                .open
                .lock()
                .insert(2, reader(Some(100)));
            let next = |timeout_ms| call(&handle, Request::Next { id: 1, timeout_ms });
            // The history first, each answer with the state as it is by then.
            let first = next(1).await.unwrap();
            assert_eq!(first["type"], "event");
            assert_eq!(first["diagnostics"]["relay_transport"], "tcp");
            while next(1).await.unwrap()["type"] == "event" {}
            // Ready, with no session to report, the tunnel has nothing to say.
            let mut pending = std::pin::pin!(next(30_000));
            std::future::poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            call(&handle, Request::Close { id: 1 }).await.unwrap();
            assert_eq!(pending.await.unwrap()["type"], "closed");
            // A reader that asked for samples gets them while the tunnel
            // is quiet: past the history, figures every period.
            let sampled = loop {
                let answer = call(
                    &handle,
                    Request::Next {
                        id: 2,
                        timeout_ms: 5_000,
                    },
                )
                .await
                .unwrap();
                if answer["type"] != "event" {
                    break answer;
                }
            };
            assert_eq!(sampled["type"], "metrics");
            assert_eq!(sampled["metrics"]["relay"]["transport"], "tcp");
            call(&handle, Request::Close { id: 2 }).await.unwrap();
            assert!(handle.tunnel_events.open.lock().is_empty());
            let TunnelHandle::Share(registration) = &*tunnel else {
                unreachable!()
            };
            registration
                .wait_ready_timeout(Duration::from_secs(1))
                .await
                .unwrap();
            registration.stop().await.unwrap();
        });
        drop(handle);
    }
}
