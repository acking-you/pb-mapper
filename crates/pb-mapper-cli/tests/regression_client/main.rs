//! `register` and `connect` against fake relays.
//!
//! A binary of its own, because its cases shorten the timings a tunnel reads
//! from the environment: one control worker per registration, so retries are
//! countable; heartbeats and registration probes in tens of milliseconds;
//! the reject ladder in hundreds; health checks every 20 ms. They are set
//! once, before any case starts a tunnel (`client_env`), and never written
//! again, so no case reads another's values or a value half restored.
//! `register` reads only the first group and `connect` only the last.

// An integration test: a failed `unwrap` is a failed test, which is the report
// this target exists to produce. `allow-unwrap-in-tests` covers `#[cfg(test)]`
// modules but not a `tests/` target, whose whole body is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../e2e/bound_listener.rs"]
mod bound_listener;
#[path = "../regression/credential.rs"]
mod credential;
mod fake;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use pb_mapper_client::client::run_client_side_cli_with_callback;
use pb_mapper_client::server::run_server_side_cli_with_callback;
use pb_mapper_core::config::{
    PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL, PB_MAPPER_CLIENT_HEALTH_FAILURE_THRESHOLD,
    PB_MAPPER_CONTROL_CONN_POOL_SIZE, PB_MAPPER_CONTROL_HEARTBEAT_INTERVAL,
    PB_MAPPER_CONTROL_HEARTBEAT_TOLERANCE, PB_MAPPER_CONTROL_SUSPECT_GRACE,
    PB_MAPPER_REGISTRATION_PROBE_TIMEOUT, PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX,
    PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN,
};
use pb_mapper_protocol::MessageWriter;
use pb_mapper_protocol::command::{
    MessageSerializer, PbConnRequest, PbConnResponse, PbConnStatusReq, PbConnStatusResp,
    PbServiceConnStatus,
};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;
use uni_stream::stream::{TcpListenerProvider, TcpStreamProvider};

use bound_listener::{BoundListener, with_bound_listener};
use fake::{
    FakeRelay, Reply, plain_tcp_options, read_secure_request, relay_security, status_recorder,
};

/// How long a case waits for what it expects to happen at once: a bound for
/// a loaded machine, not a timing the case asserts.
const READY: Duration = Duration::from_secs(5);

/// Set this binary's timings, once, before the calling case starts a tunnel.
fn client_env() {
    static ENV: LazyLock<()> = LazyLock::new(|| {
        for (name, value) in [
            (PB_MAPPER_CONTROL_CONN_POOL_SIZE, "1"),
            (PB_MAPPER_CONTROL_HEARTBEAT_INTERVAL, "20ms"),
            (PB_MAPPER_CONTROL_HEARTBEAT_TOLERANCE, "50ms"),
            // Far past READY: a control connection that hears nothing is
            // given up at tolerance + grace, which would re-register on its
            // own. Only the registration probe's `Missing` answer can then
            // make a case's tunnel register again in time.
            (PB_MAPPER_CONTROL_SUSPECT_GRACE, "30s"),
            (PB_MAPPER_REGISTRATION_PROBE_TIMEOUT, "50ms"),
            (PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MIN, "400ms"),
            (PB_MAPPER_REGISTRATION_REJECT_BACKOFF_MAX, "800ms"),
            (PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL, "20ms"),
            (PB_MAPPER_CLIENT_HEALTH_FAILURE_THRESHOLD, "3"),
        ] {
            // SAFETY: mutating the environment is unsafe in edition 2024
            // because it is process-global. Every case calls this before it
            // starts a tunnel, the first call writes while the others wait on
            // the lock, and nothing writes these names again.
            unsafe { std::env::set_var(name, value) };
        }
    });
    *ENV
}

/// Poll `ready` every 10 ms until it holds, for up to [`READY`].
async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    timeout(READY, async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

#[tokio::test]
async fn local_server_reconnects_when_registered_conn_is_missing_from_remote_status() {
    client_env();
    let remote_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = remote_listener.local_addr().unwrap();
    let register_count = AtomicUsize::new(0);
    let (second_register_tx, second_register_rx) = oneshot::channel();
    let second_register_tx = Mutex::new(Some(second_register_tx));
    // Registers are accepted and held; every status says the service has no
    // connection, which is what has to make the local server register again.
    let _relay = FakeRelay::start(remote_listener, move |request| match request {
        PbConnRequest::Register { key, .. } => {
            let count = register_count.fetch_add(1, Ordering::SeqCst) + 1;
            // Reported as the second register arrives, ahead of its answer:
            // its arrival is all this case waits for.
            if count == 2
                && let Some(tx) = second_register_tx.lock().unwrap().take()
            {
                tx.send(()).unwrap();
            }
            tracing::debug!(key, count, "fake server accepted register");
            Reply::RespondAndHold(PbConnResponse::RegisterV2 {
                direct: None,
                conn_id: count as u32,
                generation: count as u64,
                lease_ttl_ms: 150,
            })
        }
        PbConnRequest::Status(PbConnStatusReq::Service {
            direct_query: _,
            key,
        }) => Reply::Respond(PbConnResponse::Status(PbConnStatusResp::Service {
            direct: None,
            key,
            connections: Vec::new(),
        })),
        PbConnRequest::Status(PbConnStatusReq::Keys) => {
            Reply::Respond(PbConnResponse::Status(PbConnStatusResp::Keys(Vec::new())))
        }
        _ => Reply::Close,
    })
    .await;

    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        plain_tcp_options(),
        None,
    ));

    timeout(READY, second_register_rx)
        .await
        .expect("local server did not reconnect after remote status lost its registration")
        .unwrap();

    local_server.abort();
}

#[tokio::test]
async fn client_closes_initial_status_probe_after_key_check() {
    client_env();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let (security, _state) = relay_security().await;

    let fake_server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (request, session) = read_secure_request(&security, &mut stream).await;
        let PbConnRequest::Status(PbConnStatusReq::Service {
            direct_query: _,
            key,
        }) = request
        else {
            panic!("client did not use service status for initial key check");
        };

        let response = PbConnResponse::Status(PbConnStatusResp::Service {
            direct: None,
            key,
            connections: vec![PbServiceConnStatus {
                conn_id: 1,
                generation: 1,
                protocol_version: 2,
                healthy: true,
                last_rx_age_ms: 0,
            }],
        })
        .encode()
        .unwrap();
        {
            let mut writer = session.response_writer(&mut stream).unwrap();
            writer.write_msg(&response).await.unwrap();
        }

        // This binary probes every 20 ms, so a bound on time alone would pass
        // a probe stream kept until the next probe answers or times out. The
        // case checks order instead: the first stream has to close before the
        // next probe connects, which fails a stream kept for the listener's
        // life or until a later probe, and READY only bounds a hang. (One
        // dropped as the next probe starts, before it connects, still passes:
        // only a probe interval longer than READY would tell it from a prompt
        // close.)
        let mut buf = [0u8; 1];
        timeout(READY, async {
            tokio::select! {
                biased;
                read = stream.read(&mut buf) => read.unwrap(),
                _ = listener.accept() => {
                    panic!("client opened another probe before closing the first")
                }
            }
        })
        .await
        .expect("client kept the one-shot status probe connection open")
    });

    let local_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let client = tokio::spawn(run_client_side_cli_with_callback::<TcpListenerProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        false,
        None,
    ));

    assert_eq!(fake_server.await.unwrap(), 0);
    client.abort();
}

#[tokio::test]
async fn client_tolerates_one_failed_health_check_while_listener_is_active() {
    client_env();
    let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = local.local_addr().unwrap();

    let remote_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = remote_listener.local_addr().unwrap();
    let failed_status_responses = Arc::new(AtomicUsize::new(0));
    let status_count = Arc::new(AtomicUsize::new(0));

    // Every status succeeds unless the case has asked for failures; the
    // subscription itself is held open, unanswered.
    let relay_failures = failed_status_responses.clone();
    let relay_status_count = status_count.clone();
    let _relay = FakeRelay::start(remote_listener, move |request| {
        let should_fail = || {
            relay_status_count.fetch_add(1, Ordering::SeqCst);
            relay_failures
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
        };
        match request {
            PbConnRequest::Status(PbConnStatusReq::Service {
                direct_query: _,
                key,
            }) => {
                let connections = if should_fail() {
                    Vec::new()
                } else {
                    vec![PbServiceConnStatus {
                        conn_id: 1,
                        generation: 1,
                        protocol_version: 2,
                        healthy: true,
                        last_rx_age_ms: 0,
                    }]
                };
                Reply::Respond(PbConnResponse::Status(PbConnStatusResp::Service {
                    direct: None,
                    key,
                    connections,
                }))
            }
            PbConnRequest::Status(PbConnStatusReq::Keys) => {
                let keys = if should_fail() {
                    Vec::new()
                } else {
                    vec!["sf-backend".to_string()]
                };
                Reply::Respond(PbConnResponse::Status(PbConnStatusResp::Keys(keys)))
            }
            PbConnRequest::Subcribe { .. } => Reply::Hold,
            _ => Reply::Close,
        }
    })
    .await;

    let (status_changes, record_status) = status_recorder();
    let client = tokio::spawn(with_bound_listener(
        local,
        run_client_side_cli_with_callback::<BoundListener, _>(
            local_addr,
            remote_addr,
            Arc::from("sf-backend"),
            false,
            Some(record_status),
        ),
    ));

    wait_until("client did not report the initial key probe", || {
        !status_changes.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        status_changes.lock().unwrap().as_slice(),
        ["connected"],
        "the initial key probe did not connect"
    );

    let baseline = status_count.load(Ordering::SeqCst);
    failed_status_responses.store(2, Ordering::SeqCst);

    wait_until(
        "client did not recover after one failed health check",
        || status_count.load(Ordering::SeqCst) >= baseline + 3,
    )
    .await;

    assert!(
        TcpStream::connect(local_addr).await.is_ok(),
        "client listener stopped after one transient health-check failure"
    );
    assert_eq!(
        status_changes.lock().unwrap().as_slice(),
        ["connected"],
        "a single failed health check must not restart the listener"
    );

    client.abort();
}

/// A fake relay that refuses every registration with one error.
///
/// Returns the timestamps of the register attempts it saw, which is what the two
/// cases below actually assert on: how long the client waited before trying
/// again, and whether it tried again at all.
async fn rejecting_relay(
    listener: TcpListener,
    code: &'static str,
    retryable: bool,
) -> (FakeRelay, Arc<Mutex<Vec<tokio::time::Instant>>>) {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let relay_attempts = attempts.clone();
    let relay = FakeRelay::start(listener, move |request| match request {
        PbConnRequest::Register { .. } | PbConnRequest::RegisterScoped { .. } => {
            relay_attempts
                .lock()
                .unwrap()
                .push(tokio::time::Instant::now());
            Reply::RespondAndHold(PbConnResponse::error(
                code,
                "refused by the test relay",
                retryable,
            ))
        }
        // The registration probe, which the client runs against a
        // relay it has not registered with yet.
        PbConnRequest::Status(PbConnStatusReq::Service {
            direct_query: _,
            key,
        }) => Reply::RespondAndHold(PbConnResponse::Status(PbConnStatusResp::Service {
            direct: None,
            key,
            connections: Vec::new(),
        })),
        PbConnRequest::Status(PbConnStatusReq::Keys) => {
            Reply::RespondAndHold(PbConnResponse::Status(PbConnStatusResp::Keys(Vec::new())))
        }
        _ => Reply::Close,
    })
    .await;
    (relay, attempts)
}

#[tokio::test]
async fn retryable_registration_rejection_waits_on_the_slow_ladder() {
    client_env();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let (_relay, attempts) =
        rejecting_relay(listener, "service_connection_limit_exceeded", true).await;

    let (status_changes, record_status) = status_recorder();
    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        plain_tcp_options(),
        Some(record_status),
    ));

    wait_until("relay saw fewer than 2 register attempts", || {
        attempts.lock().unwrap().len() >= 2
    })
    .await;
    let seen = attempts.lock().unwrap().clone();
    let gap = seen[1].duration_since(seen[0]);
    // The transport ladder's first delay is 100ms, so anything under the reject
    // ladder's 400ms minimum means the rejection was retried as a transport
    // failure — which is what turned one refusal into thousands a minute.
    assert!(
        gap >= Duration::from_millis(300),
        "a retryable rejection retried after {gap:?}, far quicker than the reject ladder"
    );
    assert!(
        !status_changes
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "connected"),
        "a refused registration reported itself connected: {:?}",
        status_changes.lock().unwrap()
    );

    local_server.abort();
}

#[tokio::test]
async fn terminal_registration_rejection_stops_the_worker() {
    client_env();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let (_relay, attempts) = rejecting_relay(listener, "service_transport_mismatch", false).await;

    let (status_changes, record_status) = status_recorder();
    let local_addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let local_server = tokio::spawn(run_server_side_cli_with_callback::<TcpStreamProvider, _>(
        local_addr,
        remote_addr,
        Arc::from("sf-backend"),
        plain_tcp_options(),
        Some(record_status),
    ));

    // The pool returns once its only worker gives up, which is the whole point:
    // a permanent rejection has to end the tunnel rather than retry it forever.
    timeout(READY, local_server)
        .await
        .expect("a permanently rejected registration kept retrying")
        .unwrap();
    assert_eq!(attempts.lock().unwrap().len(), 1);
    let statuses = status_changes.lock().unwrap().clone();
    assert!(
        statuses
            .last()
            .is_some_and(|status| status.starts_with("failed: service_transport_mismatch")),
        "a permanent rejection was not reported to the caller: {statuses:?}"
    );
}
