#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end coverage of the client SDK: tunnels and administrator RPCs.

use std::time::Duration;

use pb_mapper_auth::MIN_TEMP_KEY_TTL;
use pb_mapper_client::sdk::{
    Admin, Client, ClientConfig, ConnectRequest, ConnectionInfo, RegisterRequest, Transport,
    TunnelStatus,
};
use pb_mapper_testkit::{
    READY_TIMEOUT, Relay, admin_credential, reserve_addr, run_raw_tcp_echo, run_udp_datagram_echo,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

const LONG_TTL: Duration = Duration::from_secs(600);

/// Wait until every worker in a registration's control-connection pool is
/// visible in the relay inventory. `Registration::wait_ready` intentionally
/// resolves after the first healthy worker, which is sufficient for traffic
/// but too early for tests that operate on the whole pool.
async fn wait_for_control_pool(admin: &Admin, service_name: &str) -> Vec<ConnectionInfo> {
    let expected = pb_mapper_core::config::control_conn_pool_size();
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        let connections: Vec<_> = admin
            .list_connections_all(None)
            .await
            .unwrap()
            .into_iter()
            .filter(|connection| connection.service_name == service_name)
            .collect();
        if connections.len() >= expected {
            return connections;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {expected} control connections for {service_name}, got {connections:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn spawn_tcp_echo() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0_u8; 4096];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            if stream.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    addr
}

async fn spawn_udp_echo() -> std::net::SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0_u8; 2048];
        loop {
            let Ok((n, peer)) = socket.recv_from(&mut buf).await else {
                return;
            };
            if socket.send_to(&buf[..n], peer).await.is_err() {
                return;
            }
        }
    });
    addr
}

async fn register_echo(
    client: &Client,
    key: &str,
    transport: Transport,
    codec: bool,
) -> (pb_mapper_client::sdk::Registration, std::net::SocketAddr) {
    let echo_addr = match transport {
        Transport::Tcp => spawn_tcp_echo().await,
        Transport::Udp => spawn_udp_echo().await,
    };
    let registration = client
        .register(RegisterRequest {
            key: key.into(),
            local_addr: echo_addr.to_string(),
            transport,
            codec,
            force_namespace: false,
        })
        .await
        .unwrap();
    registration
        .wait_ready_timeout(READY_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(registration.status(), TunnelStatus::Connected);
    (registration, echo_addr)
}

#[tokio::test]
async fn sdk_tcp_tunnel_round_trips() {
    let relay = Relay::start("sdk-tcp-tunnel").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);

    let echo_addr = spawn_tcp_echo().await;
    let registration = client
        .register(RegisterRequest {
            key: "echo".into(),
            local_addr: echo_addr.to_string(),
            transport: Transport::Tcp,
            codec: false,
            force_namespace: false,
        })
        .await
        .unwrap();
    registration
        .wait_ready_timeout(READY_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(registration.status(), TunnelStatus::Connected);

    let keys = client.list_keys().await.unwrap();
    assert!(keys.iter().any(|key| key == "echo"));

    let listen_addr = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: listen_addr.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();

    run_raw_tcp_echo(listen_addr, 8, None).await;

    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

#[tokio::test]
async fn sdk_admin_issue_list_revoke() {
    let relay = Relay::start("sdk-admin").await;
    let admin_client =
        Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let admin = admin_client.admin().unwrap();

    let issued = admin
        .issue_key(LONG_TTL.max(MIN_TEMP_KEY_TTL), Some("sdk-admin".into()))
        .await
        .unwrap();
    assert!(!issued.credential.is_empty());

    let page = admin.list_keys(0, 100).await.unwrap();
    assert!(page.items.iter().any(|item| item.key_id == issued.key_id));

    let temporary = Client::new(ClientConfig {
        server: relay.addr().to_string(),
        credential: issued.credential.clone(),
        keep_alive: false,
        namespace: None,
    })
    .unwrap();
    assert!(temporary.admin().is_err());

    let echo_addr = spawn_tcp_echo().await;
    let registration = temporary
        .register(RegisterRequest {
            key: "echo".into(),
            local_addr: echo_addr.to_string(),
            transport: Transport::Tcp,
            codec: false,
            force_namespace: false,
        })
        .await
        .unwrap();
    registration
        .wait_ready_timeout(READY_TIMEOUT)
        .await
        .unwrap();

    admin.revoke_key(issued.key_id).await.unwrap();

    let listen_addr = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = temporary
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: listen_addr.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    let ready = connection.wait_ready_timeout(Duration::from_secs(3)).await;
    assert!(ready.is_err(), "revoked credential should not stay ready");

    let _ = connection.stop().await;
    let _ = registration.stop().await;
}

#[tokio::test]
async fn sdk_tcp_codec_tunnel_round_trips() {
    let relay = Relay::start("sdk-tcp-codec").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (registration, _) = register_echo(&client, "echo-codec", Transport::Tcp, true).await;

    let listen_addr = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo-codec".into(),
            local_addr: listen_addr.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
    run_raw_tcp_echo(listen_addr, 4, None).await;
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

#[tokio::test]
async fn sdk_udp_tunnel_round_trips() {
    let relay = Relay::start("sdk-udp-tunnel").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (registration, _) = register_echo(&client, "echo-udp", Transport::Udp, false).await;

    let listen_addr = reserve_addr(pb_mapper_testkit::Transport::Udp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo-udp".into(),
            local_addr: listen_addr.to_string(),
            transport: Transport::Udp,
        })
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
    run_udp_datagram_echo(listen_addr, 4, 4, None).await;
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

#[tokio::test]
async fn sdk_status_and_admin_inventory() {
    let relay = Relay::start("sdk-inventory").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (registration, _) = register_echo(&client, "echo-inv", Transport::Tcp, false).await;

    let keys = client.list_keys().await.unwrap();
    assert!(keys.iter().any(|key| key == "echo-inv"));

    let conns = client.service_status("echo-inv").await.unwrap();
    assert!(
        conns.iter().any(|conn| conn.healthy),
        "registered service should have a healthy control connection"
    );
    client.remote_id().await.unwrap();

    let admin = client.admin().unwrap();
    let status = admin.auth_status().await.unwrap();
    assert!(status.capacity > 0);
    assert!(!status.server_instance_id.is_empty());

    let services = admin.list_services_all(None).await.unwrap();
    assert!(services.iter().any(|svc| svc.service_name == "echo-inv"));

    let connections = admin.list_connections_all(None).await.unwrap();
    assert!(
        connections
            .iter()
            .any(|conn| conn.service_name == "echo-inv")
    );

    registration.stop().await.unwrap();
}

#[tokio::test]
async fn sdk_connect_stop_releases_local_port() {
    let relay = Relay::start("sdk-stop").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (registration, _) = register_echo(&client, "echo-stop", Transport::Tcp, false).await;

    let listen_addr = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo-stop".into(),
            local_addr: listen_addr.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
    connection.stop().await.unwrap();

    let mut last_error = None;
    for _ in 0..20 {
        match TcpListener::bind(listen_addr).await {
            Ok(listener) => {
                drop(listener);
                last_error = None;
                break;
            }
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    assert!(
        last_error.is_none(),
        "connect stop should release {listen_addr}: {:?}",
        last_error
    );

    registration.stop().await.unwrap();
}

#[tokio::test]
async fn sdk_admin_show_reveal_renew() {
    let relay = Relay::start("sdk-admin-lifecycle").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let admin = client.admin().unwrap();

    let issued = admin
        .issue_key(LONG_TTL.max(MIN_TEMP_KEY_TTL), Some("lifecycle".into()))
        .await
        .unwrap();
    let shown = admin.show_key(issued.key_id).await.unwrap();
    assert_eq!(shown.key_id, issued.key_id);
    assert!(shown.credential.is_empty());

    let revealed = admin.reveal_key(issued.key_id).await.unwrap();
    assert_eq!(revealed.credential, issued.credential);

    let renewed = admin
        .renew_key(issued.key_id, LONG_TTL.max(MIN_TEMP_KEY_TTL))
        .await
        .unwrap();
    assert_eq!(renewed.key_id, issued.key_id);
    assert!(renewed.expires_at >= issued.expires_at);

    admin.revoke_key(issued.key_id).await.unwrap();
}

/// A `connect` whose local address is already taken must not report itself ready:
/// the status drives `wait_ready`, and a caller told the tunnel is up has to be
/// able to reach the local endpoint.
#[tokio::test]
async fn sdk_connect_is_not_ready_while_local_port_is_occupied() {
    let relay = Relay::start("sdk-occupied-port").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (registration, _) = register_echo(&client, "echo-busy", Transport::Tcp, false).await;

    // Held for the whole case, so the SDK's bind can never succeed.
    let squatter = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let busy_addr = squatter.local_addr().unwrap();

    let connection = client
        .connect(ConnectRequest {
            key: "echo-busy".into(),
            local_addr: busy_addr.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();

    let ready = connection.wait_ready_timeout(Duration::from_secs(2)).await;
    assert!(
        ready.is_err(),
        "connect should not report ready while `{busy_addr}` is occupied"
    );
    assert_ne!(
        connection.status(),
        TunnelStatus::Connected,
        "status must not claim Connected without a bound listener"
    );

    drop(squatter);
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
    run_raw_tcp_echo(busy_addr, 4, None).await;

    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

/// `stop()` must take the in-flight forwarded session with it, not leave it
/// forwarding on a tunnel the caller believes is gone.
#[tokio::test]
async fn sdk_stop_closes_established_forwarded_stream() {
    let relay = Relay::start("sdk-stop-stream").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (registration, _) = register_echo(&client, "echo-live", Transport::Tcp, false).await;

    let listen_addr = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo-live".into(),
            local_addr: listen_addr.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();

    // An established session: the round trip proves the whole path is forwarding.
    let mut stream = tokio::net::TcpStream::connect(listen_addr).await.unwrap();
    stream.write_all(b"ping").await.unwrap();
    let mut echoed = [0_u8; 4];
    stream.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping");

    connection.stop().await.unwrap();

    // The forwarding task is cancelled, so the local half is torn down: the next
    // read sees EOF or a reset rather than another echo.
    stream.write_all(b"post").await.ok();
    let mut buf = [0_u8; 4];
    let after_stop = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await;
    match after_stop {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(n)) => panic!("stopped tunnel still forwarded {n} bytes: {:?}", &buf[..n]),
        Err(_) => panic!("stopped tunnel left the forwarded stream open"),
    }

    registration.stop().await.unwrap();
}

/// A registration the relay rejects permanently must surface as `Failed`, not
/// loop as `Retrying`: a caller awaiting `wait_ready()` with no timeout would
/// otherwise wait forever on a tunnel that can never come up.
#[tokio::test]
async fn sdk_register_reports_permanent_rejection_as_failed() {
    let relay = Relay::start("sdk-reject").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);

    // Holds the name as TCP, so the UDP registration below is a transport
    // mismatch — one of the relay's non-retryable rejections.
    let (registration, _) = register_echo(&client, "echo-clash", Transport::Tcp, false).await;

    let udp_echo = spawn_udp_echo().await;
    let clashing = client
        .register(RegisterRequest {
            key: "echo-clash".into(),
            local_addr: udp_echo.to_string(),
            transport: Transport::Udp,
            codec: false,
            force_namespace: false,
        })
        .await
        .unwrap();

    let ready = clashing.wait_ready_timeout(READY_TIMEOUT).await;
    let error = ready.expect_err("a transport mismatch can never become ready");
    let rendered = error.to_string();
    assert!(
        rendered.contains("service_transport_mismatch"),
        "the rejection reason should reach the caller, got: {rendered}"
    );
    assert!(
        matches!(clashing.status(), TunnelStatus::Failed(_)),
        "a permanent rejection must be Failed, not Retrying: {:?}",
        clashing.status()
    );

    let _ = clashing.stop().await;
    registration.stop().await.unwrap();
}

/// The `connect` half of the same guarantee: a subscription the relay refuses
/// permanently must surface as `Failed`, not loop as `Retrying`. A temporary
/// credential asking for someone else's namespace is one of those refusals.
#[tokio::test]
async fn sdk_connect_reports_permanent_rejection_as_failed() {
    let relay = Relay::start("sdk-connect-reject").await;
    let (key_id, credential) = relay
        .issue_credential(LONG_TTL.max(MIN_TEMP_KEY_TTL), "connect-reject")
        .await;

    // Any namespace but its own: a temporary credential is confined to the one
    // its key id names, and the relay says so with `retryable: false`.
    let foreign_namespace = key_id.as_u64() + 1;
    let client = Client::from_credential(
        relay.addr().to_string(),
        credential,
        false,
        Some(foreign_namespace),
    );

    let listen_addr = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo-denied".into(),
            local_addr: listen_addr.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();

    let ready = connection.wait_ready_timeout(READY_TIMEOUT).await;
    let error = ready.expect_err("a foreign namespace can never become ready");
    let rendered = error.to_string();
    assert!(
        rendered.contains("namespace_access_denied"),
        "the rejection reason should reach the caller, got: {rendered}"
    );
    assert!(
        matches!(connection.status(), TunnelStatus::Failed(_)),
        "a permanent rejection must be Failed, not Retrying: {:?}",
        connection.status()
    );

    let _ = connection.stop().await;
}

/// `admin connection retire` frees a service's registered connections, and the
/// registration's own client notices and comes back.
///
/// The operator's escape hatch from the outage that motivated it: a service whose
/// per-service connection quota was full of connections the relay could not tell
/// were dead. Retiring them empties the quota without restarting the relay.
#[tokio::test]
async fn sdk_admin_retires_registered_connections() {
    let relay = Relay::start("sdk-retire").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (registration, _) = register_echo(&client, "echo-retire", Transport::Tcp, false).await;
    let admin = client.admin().unwrap();

    let before = wait_for_control_pool(&admin, "echo-retire").await;
    let registered = before.len();

    let retired = admin
        .retire_connections(None, "echo-retire".into(), None)
        .await
        .unwrap();
    assert_eq!(retired as usize, registered, "every connection was named");

    // Naming a service the relay no longer holds is not an error, it is zero.
    assert_eq!(
        admin
            .retire_connections(None, "echo-no-such-service".into(), None)
            .await
            .unwrap(),
        0
    );

    // The client treats retirement as a reconnect, so the service comes back on
    // its own. That is the whole point: an operator clears the quota without
    // taking the service down.
    //
    // Asserted on the relay's own view, not on `wait_ready_timeout`: the tunnel's
    // status may still read Connected from the pool that was just retired, so
    // waiting on it would return immediately and pass even if the service never
    // came back. Replacement conn_ids are proof it did, since the counter only
    // moves forward.
    let retired_ids: std::collections::HashSet<u32> =
        before.iter().map(|conn| conn.conn_id).collect();
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        let now = admin.list_connections_all(None).await.unwrap();
        let replaced = now
            .iter()
            .filter(|conn| conn.service_name == "echo-retire")
            .any(|conn| !retired_ids.contains(&conn.conn_id));
        if replaced {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the service never re-registered after retirement: {now:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !matches!(registration.status(), TunnelStatus::Failed(_)),
        "retirement is a reconnect, not a permanent failure: {:?}",
        registration.status()
    );

    // And the tunnel still carries traffic, which no connection count can show.
    let listen_addr = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect(ConnectRequest {
            key: "echo-retire".into(),
            local_addr: listen_addr.to_string(),
            transport: Transport::Tcp,
        })
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
    run_raw_tcp_echo(listen_addr, 2, None).await;

    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

/// Retiring one connection by id leaves the service's other connections alone.
#[tokio::test]
async fn sdk_admin_retires_a_single_connection_by_id() {
    let relay = Relay::start("sdk-retire-one").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
    let (registration, _) = register_echo(&client, "echo-retire-one", Transport::Tcp, false).await;
    let admin = client.admin().unwrap();

    let mut ours: Vec<u32> = wait_for_control_pool(&admin, "echo-retire-one")
        .await
        .iter()
        .map(|conn| conn.conn_id)
        .collect();
    ours.sort_unstable();
    // A register opens a pool of control connections, so this is the interesting
    // case rather than a degenerate one.
    assert!(
        ours.len() > 1,
        "expected a control-connection pool, got {ours:?}"
    );

    let retired = admin
        .retire_connections(None, "echo-retire-one".into(), Some(ours[0]))
        .await
        .unwrap();
    assert_eq!(retired, 1);

    let after = admin.list_connections_all(None).await.unwrap();
    assert!(
        !after
            .iter()
            .any(|conn| conn.service_name == "echo-retire-one" && conn.conn_id == ours[0]),
        "the named connection should be gone"
    );

    registration.stop().await.unwrap();
}
