#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end coverage of the client SDK: tunnels and administrator RPCs.

use std::time::Duration;

use pb_mapper_auth::MIN_TEMP_KEY_TTL;
use pb_mapper_client::sdk::{
    Client, ClientConfig, ConnectRequest, RegisterRequest, Transport, TunnelStatus,
};
use pb_mapper_testkit::{
    READY_TIMEOUT, Relay, admin_credential, reserve_addr, run_raw_tcp_echo, run_udp_datagram_echo,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

const LONG_TTL: Duration = Duration::from_secs(600);

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
