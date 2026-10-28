//! Bounded pull subscriptions. Calls run on Dart's worker isolate; closing a
//! subscription cancels a pending read. IDs belong to one runtime handle.
use crate::{
    error::CtlError,
    handle::PbMapperHandle,
    response::{err_ctl, err_null_handle, ok_data, parse_c_string},
};
use pb_mapper_client::sdk::DiagnosticsSubscription;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    ffi::c_char,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct Subscriptions {
    next: AtomicU64,
    feeds: parking_lot::Mutex<HashMap<u64, Arc<Feed>>>,
}
struct Feed {
    receiver: tokio::sync::Mutex<DiagnosticsSubscription>,
    closed: CancellationToken,
}
impl Drop for Subscriptions {
    fn drop(&mut self) {
        for feed in self.feeds.get_mut().values() {
            feed.closed.cancel();
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Request {
    Subscribe {
        key: String,
        share: bool,
    },
    Next {
        id: u64,
        #[serde(default = "wait_ms")]
        timeout_ms: u32,
    },
    Close {
        id: u64,
    },
}
fn wait_ms() -> u32 {
    250
}

/// JSON operations: subscribe {key, share}, next {id, timeout_ms}, close {id}.
/// A next response contains event, lagged, timeout, or closed. Refresh state
/// after lag and continue; no event is silently dropped. Free every response
/// with pb_mapper_free_string. The handle must outlive all concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_diagnostics_call_json(
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
    match request {
        Request::Subscribe { key, share } => {
            let (snapshot, receiver) = handle.state.lock().await.tunnel_diagnostics(&key, share)?;
            let mut feeds = handle.diagnostics.feeds.lock();
            if feeds.len() >= 256 {
                return Err(CtlError::invalid_argument(
                    "diagnostic subscription capacity reached",
                ));
            }
            let id = handle.diagnostics.next.fetch_add(1, Ordering::Relaxed) + 1;
            feeds.insert(
                id,
                Arc::new(Feed {
                    receiver: tokio::sync::Mutex::new(receiver),
                    closed: CancellationToken::new(),
                }),
            );
            Ok(json!({"id": id, "snapshot": snapshot}))
        }
        Request::Next { id, timeout_ms } => {
            let feed = handle.diagnostics.feeds.lock().get(&id).cloned();
            let Some(feed) = feed else {
                return Ok(json!({"closed": true}));
            };
            let receive = async { feed.receiver.lock().await.recv().await };
            tokio::select! {
                biased;
                () = feed.closed.cancelled() => Ok(json!({"closed": true})),
                result = tokio::time::timeout(Duration::from_millis(timeout_ms.min(30_000).into()), receive) => {
                    use tokio::sync::broadcast::error::RecvError;
                    Ok(match result {
                        Ok(Ok(event)) => json!({"event": event}),
                        Ok(Err(RecvError::Lagged(skipped))) => json!({"lagged": skipped}),
                        Ok(Err(RecvError::Closed)) => json!({"closed": true}),
                        Err(_) => json!({"timeout": true}),
                    })
                }
            }
        }
        Request::Close { id } => {
            if let Some(feed) = handle.diagnostics.feeds.lock().remove(&id) {
                feed.closed.cancel();
            }
            Ok(json!({"closed": true}))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use pb_mapper_client::sdk::{
        Client, ClientConfig, ConnectOptions, ConnectRequest, PathPolicy, RegisterOptions,
        RegisterRequest, Transport,
    };
    use std::{future::Future, task::Poll};

    #[test]
    fn close_wakes_an_idle_read_without_stopping_the_tunnel() {
        let root = std::env::temp_dir().join(format!(
            "pb-mapper-diagnostics-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let handle = PbMapperHandle {
            diagnostics: Default::default(),
            runtime: tokio::runtime::Runtime::new().unwrap(),
            state: Arc::new(tokio::sync::Mutex::new(crate::state::PbMapperState::new(
                Some(root.display().to_string()),
            ))),
            control_shutdown: CancellationToken::new(),
        };
        handle.runtime.block_on(async {
            let _credential_guard = pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK
                .lock()
                .await;
            let relay = pb_mapper_testkit::Relay::start("ffi-diagnostics").await;
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
            // Once ready, no local connection drives further setup events.
            let tunnel = client
                .connect_with_options(
                    ConnectRequest {
                        key: "idle".into(),
                        local_addr: "127.0.0.1:0".into(),
                        transport: Transport::Tcp,
                    },
                    ConnectOptions {
                        path: PathPolicy::RelayOnly,
                    },
                )
                .await
                .unwrap();
            tunnel
                .wait_ready_timeout(Duration::from_secs(2))
                .await
                .unwrap();
            let feed = Arc::new(Feed {
                receiver: tokio::sync::Mutex::new(tunnel.subscribe_diagnostics()),
                closed: CancellationToken::new(),
            });
            handle.diagnostics.feeds.lock().insert(1, feed);
            // Drain replay using the same bounded interface as Dart.
            while call(
                &handle,
                Request::Next {
                    id: 1,
                    timeout_ms: 1,
                },
            )
            .await
            .unwrap()
            .get("event")
            .is_some()
            {}
            let mut pending = std::pin::pin!(call(
                &handle,
                Request::Next {
                    id: 1,
                    timeout_ms: 30_000
                }
            ));
            std::future::poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            call(&handle, Request::Close { id: 1 }).await.unwrap();
            assert_eq!(pending.await.unwrap()["closed"], true);
            tunnel
                .wait_ready_timeout(Duration::from_secs(1))
                .await
                .unwrap();
            assert!(handle.diagnostics.feeds.lock().is_empty());
            tunnel.stop().await.unwrap();
            registration.stop().await.unwrap();
        });
        drop(handle);
        let _ = std::fs::remove_dir_all(root);
    }
}
