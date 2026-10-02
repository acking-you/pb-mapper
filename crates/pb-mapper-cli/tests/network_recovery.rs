//! Faults affect only loopback test sockets, never the host network or live relay.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use pb_mapper_client::sdk::{Client, ConnectRequest, RegisterRequest, Registration, Transport};
use pb_mapper_testkit::{Relay, admin_credential, reserve_addr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};

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

struct FaultProxy {
    addr: SocketAddr,
    // 0: healthy; 1: blackhole new sockets; 2: blackhole the next socket only.
    mode: Arc<AtomicU8>,
    blackholed: Arc<AtomicUsize>,
    blackholed_active: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
    delay_ms: Arc<AtomicU64>,
    discard_requests_through: Arc<AtomicUsize>,
    discard_responses_through: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl FaultProxy {
    async fn start(upstream: SocketAddr, mode: u8) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mode = Arc::new(AtomicU8::new(mode));
        let blackholed = Arc::new(AtomicUsize::new(0));
        let blackholed_active = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let delay_ms = Arc::new(AtomicU64::new(0));
        let proxy_delay = delay_ms.clone();
        let discard_requests_through = Arc::new(AtomicUsize::new(0));
        let proxy_discard_requests = discard_requests_through.clone();
        let discard_responses_through = Arc::new(AtomicUsize::new(0));
        let proxy_discard = discard_responses_through.clone();
        let proxy_mode = mode.clone();
        let proxy_blackholed = blackholed.clone();
        let proxy_blackholed_active = blackholed_active.clone();
        let proxy_accepted = accepted.clone();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut downstream, _) = accepted.unwrap();
                        let id = proxy_accepted.fetch_add(1, Ordering::SeqCst) + 1;
                        let mode = proxy_mode.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |m| Some(if m == 2 { 0 } else { m })).unwrap();
                        if mode != 0 { proxy_blackholed.fetch_add(1, Ordering::SeqCst); }
                        let delay = proxy_delay.clone();
                        let discard_requests = proxy_discard_requests.clone();
                        let discard = proxy_discard.clone();
                        let blackholed_active = proxy_blackholed_active.clone();
                        tasks.spawn(async move {
                            if mode != 0 {
                                // Existing sockets stay wedged after the route recovers.
                                blackholed_active.fetch_add(1, Ordering::SeqCst);
                                let _ = tokio::io::copy(&mut downstream, &mut tokio::io::sink()).await;
                                blackholed_active.fetch_sub(1, Ordering::SeqCst);
                            } else if let Ok(mut remote) = TcpStream::connect(upstream).await {
                                downstream.set_nodelay(true).unwrap();
                                remote.set_nodelay(true).unwrap();
                                let (dr, dw) = downstream.split();
                                let (rr, rw) = remote.split();
                                tokio::select! {
                                    _ = delayed_copy(dr, rw, &delay, Some((id, &discard_requests))) => {}
                                    _ = delayed_copy(rr, dw, &delay, Some((id, &discard))) => {}
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
            mode,
            blackholed,
            blackholed_active,
            accepted,
            delay_ms,
            discard_requests_through,
            discard_responses_through,
            task,
        }
    }

    async fn wait_blackholed(&self, count: usize) {
        timeout(Duration::from_secs(3), async {
            while self.blackholed.load(Ordering::SeqCst) < count {
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

#[tokio::test]
async fn registered_but_one_way_control_socket_is_replaced_without_dropping_live_data() {
    test_env();
    let relay = Relay::start("recovery-one-way").await;
    let proxy = FaultProxy::start(relay.addr(), 0).await;
    let host = Client::from_credential(proxy.addr.to_string(), admin_credential(), false, None);
    let direct = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (echo, echo_task) = echo_server().await;
    let registration = register(&host, echo).await;
    registration
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let admin = direct.admin().unwrap();
    timeout(Duration::from_secs(3), async {
        while admin.list_connections_all(None).await.unwrap().len() < 2 {
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
    let controls_through = proxy.accepted.load(Ordering::SeqCst);
    let listen = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = direct
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: listen.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let mut live = TcpStream::connect(listen).await.unwrap();
    timeout(Duration::from_secs(2), round_trip(&mut live))
        .await
        .unwrap();
    proxy
        .discard_responses_through
        .store(controls_through, Ordering::SeqCst);
    let started = Instant::now();
    let restored = timeout(Duration::from_secs(11), async {
        loop {
            timeout(Duration::from_millis(500), round_trip(&mut live))
                .await
                .unwrap();
            let ids = admin.list_connections_all(None).await.unwrap();
            if ids.len() == 2
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
    echo_task.abort();
    restored.expect("service inventory is not proof that the old control socket works");
}

#[tokio::test]
async fn delayed_control_ack_does_not_unregister_a_live_service() {
    test_env();
    let relay = Relay::start("recovery-delayed-ack").await;
    let proxy = FaultProxy::start(relay.addr(), 0).await;
    proxy.delay_ms.store(220, Ordering::SeqCst);
    let host = Client::from_credential(proxy.addr.to_string(), admin_credential(), false, None);
    let direct = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (echo, echo_task) = echo_server().await;
    let registration = register(&host, echo).await;
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
    let listen = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = direct
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: listen.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
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
    echo_task.abort();
}

impl Drop for FaultProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn echo_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut streams = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.unwrap();
                    streams.spawn(async move {
                        let (mut reader, mut writer) = stream.split();
                        let _ = tokio::io::copy(&mut reader, &mut writer).await;
                    });
                }
                Some(_) = streams.join_next() => {}
            }
        }
    });
    (addr, task)
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

async fn round_trip(stream: &mut TcpStream) {
    stream.write_all(b"weak-network-echo").await.unwrap();
    let mut reply = [0; 17];
    stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"weak-network-echo");
}

#[tokio::test]
async fn registration_replaces_blackholed_handshake_after_network_recovers() {
    test_env();
    let relay = Relay::start("recovery-blackhole").await;
    let proxy = FaultProxy::start(relay.addr(), 1).await;
    let client = Client::from_credential(proxy.addr.to_string(), admin_credential(), false, None);
    let (echo, echo_task) = echo_server().await;
    let registration = register(&client, echo).await;
    proxy
        .wait_blackholed(pb_mapper_core::config::control_conn_pool_size())
        .await;
    let restored = Instant::now();
    proxy.mode.store(0, Ordering::SeqCst);
    registration
        .wait_ready_timeout(Duration::from_secs(35))
        .await
        .unwrap();
    let recovered = restored.elapsed();
    eprintln!(
        "registration after route restoration: {recovered:?}, attempts={}",
        proxy.accepted.load(Ordering::SeqCst)
    );
    let listen = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: listen.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    timeout(
        Duration::from_secs(2),
        round_trip(&mut TcpStream::connect(listen).await.unwrap()),
    )
    .await
    .unwrap();
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
    echo_task.abort();
    assert!(
        recovered < Duration::from_secs(6),
        "stale handshake delayed restored route by {recovered:?}"
    );
}

#[tokio::test]
async fn stalled_health_probe_does_not_block_new_or_existing_traffic() {
    test_env();
    let relay = Relay::start("recovery-health-probe").await;
    let register_client =
        Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (echo, echo_task) = echo_server().await;
    let registration = register(&register_client, echo).await;
    registration
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let proxy = FaultProxy::start(relay.addr(), 0).await;
    let client = Client::from_credential(proxy.addr.to_string(), admin_credential(), false, None);
    let listen = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: listen.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let mut existing = TcpStream::connect(listen).await.unwrap();
    timeout(Duration::from_secs(2), round_trip(&mut existing))
        .await
        .unwrap();
    proxy.mode.store(2, Ordering::SeqCst);
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
    echo_task.abort();
    result.expect("a background probe must not block accept/forwarding");
}

#[tokio::test]
async fn offline_registration_limits_attempts_and_stops_promptly() {
    test_env();
    let relay = Relay::start("recovery-offline").await;
    let proxy = FaultProxy::start(relay.addr(), 1).await;
    let client = Client::from_credential(proxy.addr.to_string(), admin_credential(), false, None);
    let registration = register(&client, relay.addr()).await;
    proxy.wait_blackholed(2).await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let attempts = proxy.accepted.load(Ordering::SeqCst);
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
    test_env();
    let relay = Relay::start("recovery-setup-capacity").await;
    let host = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (echo, echo_task) = echo_server().await;
    let registration = register(&host, echo).await;
    registration
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let proxy = FaultProxy::start(relay.addr(), 0).await;
    let client = Client::from_credential(proxy.addr.to_string(), admin_credential(), false, None);
    let listen = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: listen.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    proxy.mode.store(1, Ordering::SeqCst);
    let before = proxy.accepted.load(Ordering::SeqCst);
    let mut callers = Vec::new();
    timeout(Duration::from_secs(1), async {
        for _ in 0..96 {
            callers.push(TcpStream::connect(listen).await.unwrap());
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let setups = proxy.accepted.load(Ordering::SeqCst) - before;
    assert!(
        (64..=65).contains(&setups),
        "expected 64 setup attempts plus at most one probe, got {setups}"
    );
    timeout(Duration::from_millis(300), connection.stop())
        .await
        .unwrap()
        .unwrap();
    registration.stop().await.unwrap();
    echo_task.abort();
    eprintln!("96 callers while offline: {setups} relay sockets (64 setup slots + one probe)");
}

#[tokio::test]
async fn services_missing_from_relay_recover_together_without_blocking_another_host() {
    test_env();
    let relay = Relay::start("recovery-multiple-services").await;
    let proxy = FaultProxy::start(relay.addr(), 0).await;
    let host = Client::from_credential(proxy.addr.to_string(), admin_credential(), false, None);
    let direct = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let admin = direct.admin().unwrap();
    let keys = ["staticflow", "pocket-app", "pocket-meta", "other-host"];
    let (echo, echo_task) = echo_server().await;
    let mut registrations = Vec::new();
    let mut connections = Vec::new();
    let mut listeners = Vec::new();
    for key in keys {
        let publisher = if key == "other-host" { &direct } else { &host };
        let registration = publisher
            .register(RegisterRequest {
                key: key.into(),
                local_addr: echo.to_string(),
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
        let listen = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
        let connection = direct
            .connect(ConnectRequest {
                key: key.into(),
                local_addr: listen.to_string(),
                transport: Transport::Tcp,
            })
            .await
            .unwrap();
        connection
            .wait_ready_timeout(Duration::from_secs(3))
            .await
            .unwrap();
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
                        == pb_mapper_core::config::control_conn_pool_size()
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
        proxy.mode.store(1, Ordering::SeqCst);
        let old_sockets = proxy.accepted.load(Ordering::SeqCst);
        proxy
            .discard_requests_through
            .store(old_sockets, Ordering::SeqCst);
        proxy
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
            while proxy.blackholed_active.load(Ordering::SeqCst)
                < 3 * pb_mapper_core::config::control_conn_pool_size()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("all host workers must be stuck in outage attempts before restoring the route");

        let restored = Instant::now();
        proxy.mode.store(0, Ordering::SeqCst);
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
    echo_task.abort();
}

#[tokio::test]
async fn repeated_brief_jitter_preserves_control_and_existing_streams() {
    test_env();
    let relay = Relay::start("recovery-repeated-jitter").await;
    let proxy = FaultProxy::start(relay.addr(), 0).await;
    let host = Client::from_credential(proxy.addr.to_string(), admin_credential(), false, None);
    let direct = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (echo, echo_task) = echo_server().await;
    let registration = register(&host, echo).await;
    registration
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let admin = direct.admin().unwrap();
    timeout(Duration::from_secs(3), async {
        while admin.list_connections_all(None).await.unwrap().len() < 2 {
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
    let listen = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = direct
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: listen.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection
        .wait_ready_timeout(Duration::from_secs(3))
        .await
        .unwrap();
    let mut live = TcpStream::connect(listen).await.unwrap();
    timeout(Duration::from_secs(2), round_trip(&mut live))
        .await
        .unwrap();
    for cycle in 0..3 {
        proxy.delay_ms.store(800, Ordering::SeqCst);
        let delay = proxy.delay_ms.clone();
        let restore = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            delay.store(0, Ordering::SeqCst);
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
    echo_task.abort();
}
