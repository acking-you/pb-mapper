//! A running tunnel's events, read by the window one at a time.
//!
//! One export, [`pb_mapper_tunnel_events_call_json`], takes `{"op": …, …}`
//! as `pb_mapper_relay_call_json` does:
//!
//! - `subscribe {kind: "share" | "connect", key}` → `{id, diagnostics}`;
//! - `next {id, timeout_ms}` waits up to `timeout_ms` (at most 30 s) and
//!   answers `{type: "event", event, diagnostics}`, `{type: "lagged",
//!   skipped, diagnostics}` when the reader fell behind, `{type: "idle"}`, or
//!   `{type: "closed"}` once the tunnel has stopped or the reader closed;
//! - `close {id}` ends a reader, waking a pending `next`.
//!
//! Each answer carries the tunnel's diagnostics as they are by then, so a
//! reader never keeps a state of its own. A reader holds its tunnel weakly:
//! it never keeps one running. Calls run on Dart's worker isolates, so the
//! handle must outlive every call in flight.

use std::{
    collections::HashMap,
    ffi::c_char,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use pb_mapper_client::sdk::{TunnelDiagnostics, TunnelEvent, TunnelEvents};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    error::CtlError,
    handle::PbMapperHandle,
    response::{err_ctl, err_null_handle, ok_data, parse_c_string},
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
    tunnel: Weak<TunnelHandle>,
    closed: CancellationToken,
}

impl Reader {
    fn diagnostics(&self) -> Option<TunnelDiagnostics> {
        self.tunnel.upgrade().map(|tunnel| tunnel.diagnostics())
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Subscribe { kind: TunnelKind, key: String },
    Next { id: u64, timeout_ms: u32 },
    Close { id: u64 },
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
    },
    Lagged {
        skipped: u64,
        diagnostics: Option<TunnelDiagnostics>,
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
    if handle.is_null() {
        return err_null_handle();
    }
    let request = match parse_c_string(request, "request").and_then(|raw| {
        serde_json::from_str::<Request>(&raw).map_err(|e| CtlError::invalid_argument(e.to_string()))
    }) {
        Ok(request) => request,
        Err(error) => return err_ctl(&error),
    };
    let handle = unsafe { &*handle };
    match handle.runtime.block_on(call(handle, request)) {
        Ok(value) => ok_data(value),
        Err(error) => err_ctl(&error),
    }
}

async fn call(handle: &PbMapperHandle, request: Request) -> Result<Value, CtlError> {
    let readers = &handle.tunnel_events;
    match request {
        Request::Subscribe { kind, key } => {
            let reader = {
                let state = handle.state.lock().await;
                let tunnel = state.running_tunnel(matches!(kind, TunnelKind::Share), &key)?;
                Reader {
                    events: tokio::sync::Mutex::new(tunnel.subscribe_events()),
                    tunnel: Arc::downgrade(tunnel),
                    closed: CancellationToken::new(),
                }
            };
            let diagnostics = reader.diagnostics();
            let mut open = readers.open.lock();
            if open.len() >= MAX_READERS {
                return Err(CtlError::invalid_argument("too many tunnel event readers"));
            }
            let id = readers.last_id.fetch_add(1, Ordering::Relaxed) + 1;
            open.insert(id, Arc::new(reader));
            Ok(json!({ "id": id, "diagnostics": diagnostics }))
        }
        Request::Next { id, timeout_ms } => {
            let Some(reader) = readers.open.lock().get(&id).cloned() else {
                return to_value(Next::Closed);
            };
            let wait = Duration::from_millis(timeout_ms.into()).min(MAX_WAIT);
            let next = tokio::select! {
                biased;
                () = reader.closed.cancelled() => Next::Closed,
                received = tokio::time::timeout(wait, async {
                    reader.events.lock().await.recv().await
                }) => match received {
                    Ok(Some(Ok(event))) => Next::Event { event, diagnostics: reader.diagnostics() },
                    Ok(Some(Err(lagged))) => Next::Lagged {
                        skipped: lagged.skipped,
                        diagnostics: reader.diagnostics(),
                    },
                    Ok(None) => Next::Closed,
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use pb_mapper_client::sdk::{
        Client, ClientConfig, RegisterOptions, RegisterRequest, Transport,
    };
    use std::{future::Future, task::Poll};

    #[test]
    fn closing_a_reader_wakes_its_pending_read_and_leaves_the_tunnel_running() {
        let handle = PbMapperHandle {
            tunnel_events: Default::default(),
            runtime: tokio::runtime::Runtime::new().unwrap(),
            state: Arc::new(tokio::sync::Mutex::new(crate::state::PbMapperState::new(
                None,
            ))),
            control_shutdown: CancellationToken::new(),
        };
        handle.runtime.block_on(async {
            let _credential_guard = pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK
                .lock()
                .await;
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
            handle.tunnel_events.open.lock().insert(
                1,
                Arc::new(Reader {
                    events: tokio::sync::Mutex::new(tunnel.subscribe_events()),
                    tunnel: Arc::downgrade(&tunnel),
                    closed: CancellationToken::new(),
                }),
            );
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
    }
}
