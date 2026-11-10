//! Faults affect only loopback test sockets, never the host network or live relay.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, Connection, PathPolicy, RegisterRequest, Registration,
    Transport,
};
use pb_mapper_core::config::control_conn_pool_size;
use pb_mapper_testkit::{Relay, admin_client, admin_credential, reserve_addr, tcp_echo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};
use tokio_util::task::AbortOnDropHandle;

fn test_env() {
    static ENV: LazyLock<()> = LazyLock::new(|| {
        // SAFETY: initialized once before any worker starts in this test binary.
        // Tests never mutate these values again.
        unsafe {
            std::env::set_var("PB_MAPPER_CLIENT_HEALTH_CHECK_INTERVAL", "200ms");
        }
    });
    *ENV;
}

/// What a [`FaultProxy`] does with each socket it accepts from now on.
///
/// A socket keeps the route it was accepted under: a blackholed one stays
/// wedged after the route recovers, as on a real network.
#[derive(Clone, Copy)]
#[repr(u8)]
enum Route {
    /// Forward to the relay.
    Healthy = 0,
    /// Swallow every new socket: read what it sends and forward nothing.
    Blackhole = 1,
    /// Swallow the next new socket only, then route healthily again.
    BlackholeNext = 2,
}

/// What a [`FaultProxy`] and the cases driving it share.
#[derive(Default)]
struct ProxyState {
    /// A [`Route`], as its discriminant.
    route: AtomicU8,
    /// Sockets accepted so far; each is numbered by this count as it arrives.
    accepted: AtomicUsize,
    /// Sockets swallowed so far.
    blackholed: AtomicUsize,
    /// Swallowed sockets still open.
    blackholed_active: AtomicUsize,
    /// Added before every forwarded write, in milliseconds.
    delay_ms: AtomicU64,
    /// Forwarded sockets numbered at or below this drop what the client sends.
    discard_requests_through: AtomicUsize,
    /// Forwarded sockets numbered at or below this drop what the relay answers.
    discard_responses_through: AtomicUsize,
}

/// A TCP hop in front of a relay whose faults a case switches at run time.
struct FaultProxy {
    addr: SocketAddr,
    state: Arc<ProxyState>,
    _task: AbortOnDropHandle<()>,
}

impl FaultProxy {
    async fn start(upstream: SocketAddr, route: Route) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(ProxyState::default());
        state.route.store(route as u8, Ordering::SeqCst);
        let proxy = state.clone();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut downstream, _) = accepted.unwrap();
                        let id = proxy.accepted.fetch_add(1, Ordering::SeqCst) + 1;
                        let route = proxy
                            .route
                            .try_update(Ordering::SeqCst, Ordering::SeqCst, |route| {
                                Some(if route == Route::BlackholeNext as u8 {
                                    Route::Healthy as u8
                                } else {
                                    route
                                })
                            })
                            .unwrap();
                        let healthy = route == Route::Healthy as u8;
                        if !healthy {
                            proxy.blackholed.fetch_add(1, Ordering::SeqCst);
                        }
                        let state = proxy.clone();
                        tasks.spawn(async move {
                            if !healthy {
                                // Existing sockets stay wedged after the route recovers.
                                state.blackholed_active.fetch_add(1, Ordering::SeqCst);
                                let _ = tokio::io::copy(&mut downstream, &mut tokio::io::sink()).await;
                                state.blackholed_active.fetch_sub(1, Ordering::SeqCst);
                            } else if let Ok(mut remote) = TcpStream::connect(upstream).await {
                                downstream.set_nodelay(true).unwrap();
                                remote.set_nodelay(true).unwrap();
                                let (dr, dw) = downstream.split();
                                let (rr, rw) = remote.split();
                                let delay = &state.delay_ms;
                                tokio::select! {
                                    _ = delayed_copy(dr, rw, delay, Some((id, &state.discard_requests_through))) => {}
                                    _ = delayed_copy(rr, dw, delay, Some((id, &state.discard_responses_through))) => {}
                                }
                            }
                        });
                    }
                    Some(_) = tasks.join_next() => {}
                }
            }
        });
        Self {
            addr,
            state,
            _task: AbortOnDropHandle::new(task),
        }
    }

    fn set_route(&self, route: Route) {
        self.state.route.store(route as u8, Ordering::SeqCst);
    }

    async fn wait_blackholed(&self, count: usize) {
        timeout(Duration::from_secs(3), async {
            while self.state.blackholed.load(Ordering::SeqCst) < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

async fn delayed_copy(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    mut writer: impl tokio::io::AsyncWrite + Unpin,
    delay: &AtomicU64,
    discard: Option<(usize, &AtomicUsize)>,
) -> std::io::Result<()> {
    let mut buf = [0; 4096];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        if discard.is_some_and(|(id, through)| id <= through.load(Ordering::SeqCst)) {
            continue;
        }
        let ms = delay.load(Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        writer.write_all(&buf[..n]).await?;
    }
}

// Network notifications are process-wide. Fault-injection cases must not wake
// unrelated cases that measure retry rates or setup concurrency.
static NETWORK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn registered_but_one_way_control_socket_is_replaced_without_dropping_live_data() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-one-way").await;
    let proxy = FaultProxy::start(relay.addr(), Route::Healthy).await;
    let host = admin_client(proxy.addr);
    let direct = admin_client(relay.addr());
    let echo = tcp_echo(None).await;
    let registration = register(&host, echo.addr()).await;
    registration
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let admin = direct.admin().unwrap();
    timeout(Duration::from_secs(3), async {
        while admin.list_connections_all(None).await.unwrap().len() < control_conn_pool_size() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let old_ids: Vec<_> = admin
        .list_connections_all(None)
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.conn_id, c.generation))
        .collect();
    let controls_through = proxy.state.accepted.load(Ordering::SeqCst);
    let (connection, listen) = connect_ready(&direct, "echo", ConnectOptions::default()).await;
    let mut live = TcpStream::connect(listen).await.unwrap();
    timeout(Duration::from_secs(2), round_trip(&mut live))
        .await
        .unwrap();
    proxy
        .state
        .discard_responses_through
        .store(controls_through, Ordering::SeqCst);
    let started = Instant::now();
    let restored = timeout(Duration::from_secs(11), async {
        loop {
            timeout(Duration::from_millis(500), round_trip(&mut live))
                .await
                .unwrap();
            let ids = admin.list_connections_all(None).await.unwrap();
            if ids.len() == control_conn_pool_size()
                && ids
                    .iter()
                    .all(|c| !old_ids.contains(&(c.conn_id, c.generation)))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    eprintln!(
        "one-way control replacement: {:?}, success={}",
        started.elapsed(),
        restored.is_ok()
    );
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
    restored.expect("service inventory is not proof that the old control socket works");
}

#[tokio::test]
async fn delayed_control_ack_does_not_unregister_a_live_service() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-delayed-ack").await;
    let proxy = FaultProxy::start(relay.addr(), Route::Healthy).await;
    proxy.state.delay_ms.store(220, Ordering::SeqCst);
    let host = admin_client(proxy.addr);
    let direct = admin_client(relay.addr());
    let echo = tcp_echo(None).await;
    let registration = register(&host, echo.addr()).await;
    registration
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let before = direct
        .admin()
        .unwrap()
        .list_connections_all(None)
        .await
        .unwrap();
    let (connection, listen) = connect_ready(&direct, "echo", ConnectOptions::default()).await;
    let started = Instant::now();
    for _ in 0..3 {
        timeout(
            Duration::from_secs(6),
            round_trip(&mut TcpStream::connect(listen).await.unwrap()),
        )
        .await
        .unwrap();
    }
    let after = direct
        .admin()
        .unwrap()
        .list_connections_all(None)
        .await
        .unwrap();
    assert_eq!(
        before
            .iter()
            .map(|c| (c.conn_id, c.generation))
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|c| (c.conn_id, c.generation))
            .collect::<Vec<_>>(),
        "jitter must not churn control registrations"
    );
    eprintln!(
        "three encrypted streams with 440ms injected RTT: {:?}",
        started.elapsed()
    );
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

async fn register(client: &Client, addr: SocketAddr) -> Registration {
    client
        .register(RegisterRequest {
            key: "echo".into(),
            local_addr: addr.to_string(),
            transport: Transport::Tcp,
            codec: true,
            force_namespace: false,
        })
        .await
        .unwrap()
}

/// Subscribe to `key` on a reserved port and give it three seconds to be ready.
async fn connect_ready(
    client: &Client,
    key: &str,
    options: ConnectOptions,
) -> (Connection, SocketAddr) {
    let listen = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect_with_options(
            ConnectRequest {
                key: key.into(),
                local_addr: listen.to_string(),
                transport: Transport::Tcp,
            },
            options,
        )
        .await
        .unwrap();
    connection
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    (connection, listen)
}

async fn round_trip(stream: &mut TcpStream) {
    stream.write_all(b"weak-network-echo").await.unwrap();
    let mut reply = [0; 17];
    stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"weak-network-echo");
}

#[tokio::test]
async fn registration_replaces_blackholed_handshake_after_network_recovers() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-blackhole").await;
    let proxy = FaultProxy::start(relay.addr(), Route::Blackhole).await;
    let client = admin_client(proxy.addr);
    let echo = tcp_echo(None).await;
    let registration = register(&client, echo.addr()).await;
    proxy.wait_blackholed(control_conn_pool_size()).await;
    let restored = Instant::now();
    proxy.set_route(Route::Healthy);
    registration
        .wait_ready_timeout(Duration::from_secs(35))
        .await
        .unwrap();
    let recovered = restored.elapsed();
    eprintln!(
        "registration after route restoration: {recovered:?}, attempts={}",
        proxy.state.accepted.load(Ordering::SeqCst)
    );
    let (connection, listen) = connect_ready(&client, "echo", ConnectOptions::default()).await;
    timeout(
        Duration::from_secs(2),
        round_trip(&mut TcpStream::connect(listen).await.unwrap()),
    )
    .await
    .unwrap();
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
    assert!(
        recovered < Duration::from_secs(6),
        "stale handshake delayed restored route by {recovered:?}"
    );
}

#[tokio::test]
async fn stalled_health_probe_does_not_block_new_or_existing_traffic() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-health-probe").await;
    let register_client = admin_client(relay.addr());
    let echo = tcp_echo(None).await;
    let registration = register(&register_client, echo.addr()).await;
    registration
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let proxy = FaultProxy::start(relay.addr(), Route::Healthy).await;
    let client = admin_client(proxy.addr);
    let (connection, listen) = connect_ready(&client, "echo", ConnectOptions::default()).await;
    let mut existing = TcpStream::connect(listen).await.unwrap();
    timeout(Duration::from_secs(2), round_trip(&mut existing))
        .await
        .unwrap();
    proxy.set_route(Route::BlackholeNext);
    proxy.wait_blackholed(1).await;
    timeout(Duration::from_millis(500), round_trip(&mut existing))
        .await
        .unwrap();
    let started = Instant::now();
    let result = timeout(Duration::from_millis(750), async {
        let mut stream = TcpStream::connect(listen).await.unwrap();
        round_trip(&mut stream).await;
    })
    .await;
    eprintln!(
        "new traffic during stalled health probe: {:?}, success={}",
        started.elapsed(),
        result.is_ok()
    );
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
    result.expect("a background probe must not block accept/forwarding");
}

#[tokio::test]
async fn offline_registration_limits_attempts_and_stops_promptly() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-offline").await;
    let proxy = FaultProxy::start(relay.addr(), Route::Blackhole).await;
    let client = admin_client(proxy.addr);
    let registration = register(&client, relay.addr()).await;
    proxy.wait_blackholed(2).await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let attempts = proxy.state.accepted.load(Ordering::SeqCst);
    assert!(
        (4..=8).contains(&attempts),
        "offline attempt rate: {attempts} in 10s"
    );
    let started = Instant::now();
    timeout(Duration::from_millis(300), registration.stop())
        .await
        .unwrap()
        .unwrap();
    eprintln!(
        "10s offline: {attempts} attempts across two workers; stop={:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn connection_burst_has_bounded_setup_and_probe_concurrency() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-setup-capacity").await;
    let host = admin_client(relay.addr());
    let echo = tcp_echo(None).await;
    let registration = register(&host, echo.addr()).await;
    registration
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let proxy = FaultProxy::start(relay.addr(), Route::Healthy).await;
    let client = admin_client(proxy.addr);
    // Relay setup is what is being counted: no direct capability query per caller.
    let (connection, listen) = connect_ready(
        &client,
        "echo",
        ConnectOptions {
            path: PathPolicy::RelayOnly,
            ..Default::default()
        },
    )
    .await;
    proxy.set_route(Route::Blackhole);
    let before = proxy.state.accepted.load(Ordering::SeqCst);
    let mut callers = Vec::new();
    timeout(Duration::from_secs(1), async {
        for _ in 0..96 {
            callers.push(TcpStream::connect(listen).await.unwrap());
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let setups = proxy.state.accepted.load(Ordering::SeqCst) - before;
    assert!(
        (64..=65).contains(&setups),
        "expected 64 setup attempts plus at most one probe, got {setups}"
    );
    timeout(Duration::from_millis(300), connection.stop())
        .await
        .unwrap()
        .unwrap();
    registration.stop().await.unwrap();
    eprintln!("96 callers while offline: {setups} relay sockets (64 setup slots + one probe)");
}

#[tokio::test]
async fn services_missing_from_relay_recover_together_without_blocking_another_host() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-multiple-services").await;
    let proxy = FaultProxy::start(relay.addr(), Route::Healthy).await;
    let host = admin_client(proxy.addr);
    let direct = admin_client(relay.addr());
    let admin = direct.admin().unwrap();
    let keys = ["staticflow", "pocket-app", "pocket-meta", "other-host"];
    let echo = tcp_echo(None).await;
    let mut registrations = Vec::new();
    let mut connections = Vec::new();
    let mut listeners = Vec::new();
    for key in keys {
        let publisher = if key == "other-host" { &direct } else { &host };
        let registration = publisher
            .register(RegisterRequest {
                key: key.into(),
                local_addr: echo.addr().to_string(),
                transport: Transport::Tcp,
                codec: true,
                force_namespace: false,
            })
            .await
            .unwrap();
        registration
            .wait_ready_timeout(Duration::from_secs(3))
            .await
            .unwrap();
        registrations.push(registration);
        let (connection, listen) = connect_ready(&direct, key, ConnectOptions::default()).await;
        timeout(
            Duration::from_secs(2),
            round_trip(&mut TcpStream::connect(listen).await.unwrap()),
        )
        .await
        .unwrap();
        connections.push(connection);
        listeners.push(listen);
    }
    let mut unaffected = TcpStream::connect(listeners[3]).await.unwrap();
    timeout(Duration::from_secs(2), round_trip(&mut unaffected))
        .await
        .unwrap();

    for cycle in 0..2 {
        timeout(Duration::from_secs(3), async {
            loop {
                let conns = admin.list_connections_all(None).await.unwrap();
                if keys.iter().all(|key| {
                    conns
                        .iter()
                        .filter(|c| c.service_name == *key && c.healthy)
                        .count()
                        == control_conn_pool_size()
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let unaffected_ids: Vec<_> = admin
            .list_connections_all(None)
            .await
            .unwrap()
            .into_iter()
            .filter(|c| c.service_name == "other-host")
            .map(|c| (c.conn_id, c.generation))
            .collect();

        // Lose both directions of every old host socket; new attempts also
        // blackhole. Recovery must replace these sockets, not reuse them.
        proxy.set_route(Route::Blackhole);
        let old_sockets = proxy.state.accepted.load(Ordering::SeqCst);
        proxy
            .state
            .discard_requests_through
            .store(old_sockets, Ordering::SeqCst);
        proxy
            .state
            .discard_responses_through
            .store(old_sockets, Ordering::SeqCst);
        timeout(Duration::from_secs(20), async {
            loop {
                timeout(Duration::from_secs(1), round_trip(&mut unaffected))
                    .await
                    .unwrap();
                let services = admin.list_services_all(None).await.unwrap();
                if services.len() == 1 && services[0].service_name == "other-host" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("all three disconnected services must actually leave relay inventory");

        timeout(Duration::from_secs(6), async {
            while proxy.state.blackholed_active.load(Ordering::SeqCst)
                < 3 * control_conn_pool_size()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("all host workers must be stuck in outage attempts before restoring the route");

        let restored = Instant::now();
        proxy.set_route(Route::Healthy);
        let recovery = timeout(Duration::from_secs(8), async {
            loop {
                timeout(Duration::from_secs(1), round_trip(&mut unaffected))
                    .await
                    .unwrap();
                let services = admin.list_services_all(None).await.unwrap();
                if keys
                    .iter()
                    .all(|key| services.iter().any(|s| s.service_name == *key))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            for listen in &listeners {
                round_trip(&mut TcpStream::connect(listen).await.unwrap()).await;
            }
        })
        .await;
        eprintln!(
            "multi-service outage cycle {cycle}: all registrations and payloads recovered in {:?}, success={}",
            restored.elapsed(),
            recovery.is_ok()
        );
        recovery.expect("every missing service must recover without restarting the relay or host");
        let after: Vec<_> = admin
            .list_connections_all(None)
            .await
            .unwrap()
            .into_iter()
            .filter(|c| c.service_name == "other-host")
            .map(|c| (c.conn_id, c.generation))
            .collect();
        assert_eq!(unaffected_ids, after, "another host must not be evicted");
    }
    for connection in connections {
        connection.stop().await.unwrap();
    }
    for registration in registrations {
        registration.stop().await.unwrap();
    }
}

#[tokio::test]
async fn repeated_brief_jitter_preserves_control_and_existing_streams() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-repeated-jitter").await;
    let proxy = FaultProxy::start(relay.addr(), Route::Healthy).await;
    let host = admin_client(proxy.addr);
    let direct = admin_client(relay.addr());
    let echo = tcp_echo(None).await;
    let registration = register(&host, echo.addr()).await;
    registration
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let admin = direct.admin().unwrap();
    timeout(Duration::from_secs(3), async {
        while admin.list_connections_all(None).await.unwrap().len() < control_conn_pool_size() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let before: Vec<_> = admin
        .list_connections_all(None)
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.conn_id, c.generation))
        .collect();
    let (connection, listen) = connect_ready(&direct, "echo", ConnectOptions::default()).await;
    let mut live = TcpStream::connect(listen).await.unwrap();
    timeout(Duration::from_secs(2), round_trip(&mut live))
        .await
        .unwrap();
    for cycle in 0..3 {
        proxy.state.delay_ms.store(800, Ordering::SeqCst);
        let state = proxy.state.clone();
        let restore = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            state.delay_ms.store(0, Ordering::SeqCst);
        });
        let started = Instant::now();
        timeout(Duration::from_secs(6), async {
            tokio::join!(round_trip(&mut live), async {
                round_trip(&mut TcpStream::connect(listen).await.unwrap()).await;
            });
        })
        .await
        .unwrap();
        restore.await.unwrap();
        let after: Vec<_> = admin
            .list_connections_all(None)
            .await
            .unwrap()
            .into_iter()
            .map(|c| (c.conn_id, c.generation))
            .collect();
        assert_eq!(before, after, "cycle {cycle} churned registrations");
        eprintln!(
            "jitter cycle {cycle}: old and new streams completed in {:?}",
            started.elapsed()
        );
    }
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

#[tokio::test]
async fn network_hint_replaces_stalled_setup_and_real_payload_recovers_promptly() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-hint").await;
    let proxy = FaultProxy::start(relay.addr(), Route::Blackhole).await;
    let host = Client::from_credential(
        format!("localhost:{}", proxy.addr.port()),
        admin_credential(),
        false,
        None,
    );
    let direct = admin_client(relay.addr());
    let echo = tcp_echo(None).await;
    let registration = register(&host, echo.addr()).await;
    proxy.wait_blackholed(2).await;
    let local = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = direct
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: local.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    let restored = Instant::now();
    proxy.set_route(Route::Healthy);
    host.notify_network_change();
    timeout(Duration::from_secs(2), async {
        registration.wait_ready().await.unwrap();
        connection.wait_ready().await.unwrap();
        round_trip(&mut TcpStream::connect(local).await.unwrap()).await;
    })
    .await
    .unwrap();
    eprintln!(
        "network hint to encrypted payload: {:?}",
        restored.elapsed()
    );
    timeout(Duration::from_secs(2), async {
        while direct.service_status("echo").await.unwrap().len() < control_conn_pool_size() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let before: Vec<_> = direct
        .service_status("echo")
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.conn_id, c.generation))
        .collect();
    let mut existing = TcpStream::connect(local).await.unwrap();
    round_trip(&mut existing).await;
    // Hints may be noisy: they must not replace healthy controls or forwarding.
    for _ in 0..100 {
        host.notify_network_change();
        tokio::task::yield_now().await;
    }
    timeout(Duration::from_secs(2), round_trip(&mut existing))
        .await
        .unwrap();
    let after: Vec<_> = direct
        .service_status("echo")
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.conn_id, c.generation))
        .collect();
    assert_eq!(before, after);
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
    assert_eq!(registration.diagnostics().active_control_setups, 0);
    assert_eq!(registration.diagnostics().active_data_setups, 0);
}

#[tokio::test]
async fn many_mappings_share_setup_budget_and_cancel_without_stranded_slots() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    test_env();
    let relay = Relay::start("recovery-shared-budget").await;
    let proxy = FaultProxy::start(relay.addr(), Route::Blackhole).await;
    let host = admin_client(proxy.addr);
    let echo = tcp_echo(None).await;
    let mut registrations = Vec::new();
    for i in 0..20 {
        registrations.push(
            host.register(RegisterRequest {
                key: format!("mapping-{i}"),
                local_addr: echo.addr().to_string(),
                transport: Transport::Tcp,
                codec: true,
                force_namespace: false,
            })
            .await
            .unwrap(),
        );
    }
    proxy.wait_blackholed(8).await;
    assert_eq!(registrations[0].diagnostics().active_control_setups, 8);
    assert_eq!(
        proxy.state.blackholed.load(Ordering::SeqCst),
        8,
        "separate services must not each allocate eight sockets"
    );
    for registration in &registrations {
        registration.stop().await.unwrap();
    }
    assert_eq!(registrations[0].diagnostics().active_control_setups, 0);
    proxy.set_route(Route::Healthy);
    let recovered = register(&host, echo.addr()).await;
    recovered
        .wait_ready_timeout(Duration::from_secs(2))
        .await
        .unwrap();
    recovered.stop().await.unwrap();
}
