#![cfg(feature = "relay-quic")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, PathPolicy, RegisterOptions, RegisterRequest,
    RelayTransport, Transport, TunnelStatus,
};
use pb_mapper_testkit::{READY_TIMEOUT, Relay, admin_credential};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};
use tokio_util::task::AbortOnDropHandle;

fn client(relay: &Relay, transport: RelayTransport) -> Client {
    Client::from_credential(relay.addr().to_string(), admin_credential(), false, None)
        .with_relay_transport(transport)
        .unwrap()
}

async fn echo() -> (std::net::SocketAddr, AbortOnDropHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.unwrap();
                    tasks.spawn(async move {
                        let (mut read, mut write) = stream.split();
                        let _ = tokio::io::copy(&mut read, &mut write).await;
                        let _ = write.shutdown().await;
                    });
                }
            }
        }
    }));
    (addr, task)
}

async fn tunnel(
    publisher: &Client,
    consumer: &Client,
    backend: std::net::SocketAddr,
    transport: Transport,
    codec: bool,
) -> (
    pb_mapper_client::sdk::Registration,
    pb_mapper_client::sdk::Connection,
    std::net::SocketAddr,
) {
    let registration = publisher
        .register_with_options(
            RegisterRequest {
                key: "echo".into(),
                local_addr: backend.to_string(),
                transport,
                codec,
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
        .wait_ready_timeout(READY_TIMEOUT)
        .await
        .unwrap();
    let local_addr = pb_mapper_testkit::reserve_addr(match transport {
        Transport::Tcp => pb_mapper_testkit::Transport::Tcp,
        Transport::Udp => pb_mapper_testkit::Transport::Udp,
    })
    .await;
    let connection = consumer
        .connect_with_options(
            ConnectRequest {
                key: "echo".into(),
                local_addr: local_addr.to_string(),
                transport,
            },
            ConnectOptions {
                path: PathPolicy::RelayOnly,
            },
        )
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
    (registration, connection, local_addr)
}

async fn exchange(addr: std::net::SocketAddr, byte: u8) {
    let mut socket = TcpStream::connect(addr).await.unwrap();
    let payload = vec![byte; 256 << 10];
    let mut returned = Vec::new();
    let (mut read, mut write) = socket.split();
    tokio::time::timeout(READY_TIMEOUT, async {
        tokio::try_join!(
            async {
                write.write_all(&payload).await?;
                write.shutdown().await
            },
            read.read_to_end(&mut returned)
        )
        .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        returned.len(),
        payload.len(),
        "FIN must preserve the complete response tail"
    );
    assert!(returned == payload);
}

#[tokio::test]
async fn multiplexed_quic_streams_preserve_fin_and_report_real_statistics() {
    let relay = Relay::start_with_quic("quic-multiplex").await;
    let client = client(&relay, RelayTransport::Quic);
    let (backend, _echo) = echo().await;
    let (registration, connection, addr) =
        tunnel(&client, &client, backend, Transport::Tcp, false).await;
    let mut tasks = tokio::task::JoinSet::new();
    for byte in 0..8 {
        tasks.spawn(exchange(addr, byte));
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    let before = client.relay_diagnostics().await;
    assert_eq!(before.transport, RelayTransport::Quic);
    assert_eq!(before.connections_created, 1);
    assert_eq!(before.connections.len(), 1);
    assert!(before.streams_opened >= 16);
    assert!(before.streams_reused >= 15);
    assert!(before.connections[0].sent_bytes > 0 && before.connections[0].received_bytes > 0);
    assert_eq!(
        connection.diagnostics().relay_transport,
        RelayTransport::Quic
    );
    // Stop an individual downstream without closing its sibling or pooled carrier.
    let abandoned = TcpStream::connect(addr).await.unwrap();
    drop(abandoned);
    exchange(addr, 255).await;
    assert_eq!(client.relay_diagnostics().await.connections_created, 1);
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

#[tokio::test]
async fn mixed_tcp_quic_peers_share_existing_codec_and_routing() {
    for (publisher_transport, consumer_transport) in [
        (RelayTransport::Tcp, RelayTransport::Quic),
        (RelayTransport::Quic, RelayTransport::Tcp),
    ] {
        let relay = Relay::start_with_quic("quic-mixed").await;
        let publisher = client(&relay, publisher_transport);
        let consumer = client(&relay, consumer_transport);
        let (backend, _echo) = echo().await;
        let (registration, connection, addr) =
            tunnel(&publisher, &consumer, backend, Transport::Tcp, true).await;
        exchange(addr, 42).await;
        assert_eq!(publisher.relay_transport(), publisher_transport);
        assert_eq!(consumer.relay_transport(), consumer_transport);
        connection.stop().await.unwrap();
        registration.stop().await.unwrap();
    }
}

#[tokio::test]
async fn udp_service_over_quic_preserves_datagrams() {
    let relay = Relay::start_with_quic("quic-udp").await;
    let client = client(&relay, RelayTransport::Quic);
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let backend = udp.local_addr().unwrap();
    let _echo = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut buf = [0; 65536];
        loop {
            let (n, peer) = udp.recv_from(&mut buf).await.unwrap();
            udp.send_to(&buf[..n], peer).await.unwrap();
        }
    }));
    let (registration, connection, addr) =
        tunnel(&client, &client, backend, Transport::Udp, true).await;
    pb_mapper_testkit::run_udp_datagram_echo(addr, 8, 4, None).await;
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

#[tokio::test]
async fn explicit_quic_never_silently_uses_tcp() {
    let relay = Relay::start("quic-unavailable").await;
    let quic = client(&relay, RelayTransport::Quic);
    assert!(quic.list_keys().await.is_err());
    assert!(quic.relay_diagnostics().await.last_error.is_some());
    assert_eq!(quic.relay_diagnostics().await.connections_created, 0);
    assert!(
        client(&relay, RelayTransport::Tcp)
            .list_keys()
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn revoked_credential_closes_quic_and_cannot_be_reused() {
    let relay = Relay::start_with_quic("quic-revocation").await;
    let (id, credential) = relay
        .issue_credential(Duration::from_secs(600), "quic")
        .await;
    let client = Client::from_credential(relay.addr().to_string(), credential, false, None)
        .with_relay_transport(RelayTransport::Quic)
        .unwrap();
    let (backend, _echo) = echo().await;
    let (registration, connection, addr) =
        tunnel(&client, &client, backend, Transport::Tcp, false).await;
    let mut active = TcpStream::connect(addr).await.unwrap();
    active.write_all(b"x").await.unwrap();
    assert_eq!(active.read_u8().await.unwrap(), b'x');
    let administrator = self::client(&relay, RelayTransport::Quic);
    // Administrator status defaults to its own namespace, even on QUIC.
    assert!(administrator.list_keys().await.unwrap().is_empty());
    assert_eq!(client.list_keys().await.unwrap(), vec!["echo"]);
    relay.revoke(id).await;
    // Authorization cancels active forwarding before the connection's close drain.
    assert!(
        tokio::time::timeout(Duration::from_secs(1), active.read_u8())
            .await
            .unwrap()
            .is_err()
    );
    assert!(matches!(
        client.list_keys().await,
        Err(pb_mapper_client::sdk::Error::Remote {
            retryable: false,
            ..
        }) | Err(pb_mapper_client::sdk::Error::Status { .. })
    ));
    // Existing workers must settle after revocation, including when recovery
    // reaches discovery instead of the original registration/status exchange.
    for mut status in [registration.subscribe(), connection.subscribe()] {
        tokio::time::timeout(
            READY_TIMEOUT,
            status.wait_for(|status| matches!(status, TunnelStatus::Failed(_))),
        )
        .await
        .unwrap()
        .unwrap();
    }
    // A different credential still authenticates on its own pooled connection.
    assert!(administrator.list_keys().await.is_ok());
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

#[tokio::test]
async fn rejected_discovery_terminates_new_tunnels_and_preserves_sdk_errors() {
    let relay = Relay::start_with_quic("quic-discovery-rejected").await;
    let (id, credential) = relay
        .issue_credential(Duration::from_secs(600), "rejected")
        .await;
    relay.revoke(id).await;
    let client = Client::from_credential(relay.addr().to_string(), credential, false, None)
        .with_relay_transport(RelayTransport::Quic)
        .unwrap();
    assert!(matches!(
        client.list_keys().await,
        Err(pb_mapper_client::sdk::Error::Remote {
            retryable: false,
            ..
        })
    ));
    let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let registration = client
        .register_with_options(
            RegisterRequest {
                key: "rejected".into(),
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
    let connection = client
        .connect_with_options(
            ConnectRequest {
                key: "rejected".into(),
                local_addr: "127.0.0.1:0".into(),
                transport: Transport::Tcp,
            },
            ConnectOptions {
                path: PathPolicy::RelayOnly,
            },
        )
        .await
        .unwrap();
    let (registered, connected) = tokio::join!(
        registration.wait_ready_timeout(READY_TIMEOUT),
        connection.wait_ready_timeout(READY_TIMEOUT)
    );
    assert!(matches!(
        registered,
        Err(pb_mapper_client::sdk::Error::TunnelFailed { .. })
    ));
    assert!(matches!(
        connected,
        Err(pb_mapper_client::sdk::Error::TunnelFailed { .. })
    ));
    assert_eq!(client.relay_diagnostics().await.connections_created, 0);
    registration.stop().await.unwrap();
    connection.stop().await.unwrap();
}

#[tokio::test]
async fn root_rotation_delivers_its_committed_response_and_reauthenticates() {
    let relay = Relay::start_with_quic("quic-root-rotation").await;
    let client = client(&relay, RelayTransport::Quic);
    let admin = client.admin().unwrap();
    admin
        .rotate_root_key(Some("new-root0123456789abcdefghijklmn".into()))
        .await
        .unwrap();
    assert!(client.list_keys().await.is_ok());
    assert_eq!(client.relay_diagnostics().await.connections_created, 2);
}

#[tokio::test]
async fn running_tunnels_recover_after_quic_relay_restart() {
    let mut relay = Relay::start_with_quic("quic-restart").await;
    let client = client(&relay, RelayTransport::Quic);
    let (backend, _echo) = echo().await;
    let (registration, connection, addr) =
        tunnel(&client, &client, backend, Transport::Tcp, false).await;
    exchange(addr, 1).await;
    relay.stop_server().await;
    relay.restart_server(None).await;
    relay
        .wait_for_registration("echo", admin_credential(), None)
        .await;
    exchange(addr, 2).await;
    assert!(client.relay_diagnostics().await.connections_created >= 2);
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

#[tokio::test]
async fn quic_requires_the_discovered_certificate_and_exporter_bound_authentication() {
    use pb_mapper_protocol::{
        MessageReader,
        command::{MessageSerializer, PbConnRequest, PbConnResponse},
        quic,
        secure::ClientHeaderSession,
    };
    let relay = Relay::start_with_quic("quic-security").await;
    let mut tcp = TcpStream::connect(relay.addr()).await.unwrap();
    let session = ClientHeaderSession::new_v2(&admin_credential()).unwrap();
    let reply = session
        .exchange(
            &mut tcp,
            &PbConnRequest::QuicEndpoint.encode().unwrap(),
            READY_TIMEOUT,
        )
        .await
        .unwrap();
    let PbConnResponse::QuicEndpoint(Some(capability)) = PbConnResponse::decode(&reply).unwrap()
    else {
        panic!("missing QUIC capability");
    };
    let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let peer = std::net::SocketAddr::new(relay.addr().ip(), capability.port);
    let (_, unrelated_certificate) = quic::server_config().unwrap();
    endpoint.set_default_client_config(quic::client_config(&unrelated_certificate).unwrap());
    assert!(
        tokio::time::timeout(
            READY_TIMEOUT,
            endpoint.connect(peer, quic::SERVER_NAME).unwrap()
        )
        .await
        .unwrap()
        .is_err()
    );
    endpoint.set_default_client_config(quic::client_config(&capability.certificate).unwrap());
    let conn = endpoint
        .connect(peer, quic::SERVER_NAME)
        .unwrap()
        .await
        .unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    let session = ClientHeaderSession::new_v2(&admin_credential()).unwrap();
    // Valid credential and certificate, wrong TLS channel: no routing is allowed.
    session.write_initial(&mut send, &[0; 32]).await.unwrap();
    assert!(
        tokio::time::timeout(
            READY_TIMEOUT,
            session.response_reader(&mut recv).unwrap().read_msg()
        )
        .await
        .unwrap()
        .is_err()
    );
    tokio::time::timeout(READY_TIMEOUT, conn.closed())
        .await
        .unwrap();
    endpoint.close(0u32.into(), b"done");
}

#[tokio::test]
async fn a_blackholed_carrier_is_replaced_after_an_sdk_query_timeout() {
    use pb_mapper_protocol::{
        MessageWriter,
        command::{MessageSerializer, PbConnRequest, PbConnResponse},
        secure::{ClientHeaderSession, ServerSecurity},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let relay = Relay::start_with_quic("quic-blackhole").await;
    let mut tcp = TcpStream::connect(relay.addr()).await.unwrap();
    let session = ClientHeaderSession::new_v2(&admin_credential()).unwrap();
    let reply = session
        .exchange(
            &mut tcp,
            &PbConnRequest::QuicEndpoint.encode().unwrap(),
            READY_TIMEOUT,
        )
        .await
        .unwrap();
    let PbConnResponse::QuicEndpoint(Some(mut capability)) =
        PbConnResponse::decode(&reply).unwrap()
    else {
        panic!("QUIC listener must be available");
    };
    let upstream = std::net::SocketAddr::new(relay.addr().ip(), capability.port);
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    capability.port = udp.local_addr().unwrap().port();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let discovery_addr = listener.local_addr().unwrap();
    let security = ServerSecurity::new(relay.auth().clone());
    let _discovery = AbortOnDropHandle::new(tokio::spawn(async move {
        loop {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let initial = security.read_initial(&mut tcp).await.unwrap();
            assert_eq!(
                PbConnRequest::decode(&initial.payload).unwrap(),
                PbConnRequest::QuicEndpoint
            );
            initial
                .session
                .response_writer(&mut tcp)
                .unwrap()
                .write_msg(
                    &PbConnResponse::QuicEndpoint(Some(capability.clone()))
                        .encode()
                        .unwrap(),
                )
                .await
                .unwrap();
        }
    }));
    let blackhole = Arc::new(AtomicBool::new(false));
    let _proxy = AbortOnDropHandle::new(tokio::spawn({
        let blackhole = blackhole.clone();
        async move {
            let mut first = None;
            let mut current = None;
            let mut buffer = vec![0; 65536];
            loop {
                let (n, peer) = udp.recv_from(&mut buffer).await.unwrap();
                if peer == upstream {
                    if let Some(peer) = current
                        && !(blackhole.load(Ordering::Acquire) && Some(peer) == first)
                    {
                        udp.send_to(&buffer[..n], peer).await.unwrap();
                    }
                } else {
                    first.get_or_insert(peer);
                    if blackhole.load(Ordering::Acquire) && Some(peer) == first {
                        continue;
                    }
                    current = Some(peer);
                    udp.send_to(&buffer[..n], upstream).await.unwrap();
                }
            }
        }
    }));
    let client =
        Client::from_credential(discovery_addr.to_string(), admin_credential(), false, None)
            .with_relay_transport(RelayTransport::Quic)
            .unwrap();
    client.list_keys().await.unwrap();
    blackhole.store(true, Ordering::Release);
    assert!(client.list_keys().await.is_err());
    // The proxy permanently drops the original UDP peer, but accepts a fresh
    // handshake. Do not wait for Quinn's 60-second idle timeout to recover.
    client
        .list_keys()
        .await
        .expect("next request must use a fresh carrier");
    assert_eq!(client.relay_diagnostics().await.connections_created, 2);
}

#[tokio::test]
async fn stalled_discovery_does_not_block_sdk_observations() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::from_credential(
        listener.local_addr().unwrap().to_string(),
        admin_credential(),
        false,
        None,
    )
    .with_relay_transport(RelayTransport::Quic)
    .unwrap();
    let request = AbortOnDropHandle::new(tokio::spawn({
        let client = client.clone();
        async move { client.list_keys().await }
    }));
    let (mut stalled, _) = listener.accept().await.unwrap();
    // Ensure discovery has actually started; don't rely on a scheduling sleep.
    stalled.read_u8().await.unwrap();
    let snapshot = tokio::time::timeout(Duration::from_millis(200), client.relay_diagnostics())
        .await
        .expect("observations must not wait on network I/O");
    assert_eq!(snapshot.connections_created, 0);
    drop(request);
}
