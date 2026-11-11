use std::time::Duration;

use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, DirectStage, DirectTransport, FallbackReason,
    LanTransport, PathPolicy, RegisterOptions, RelayTransport, Transport, TunnelEventKind,
};
use pb_mapper_protocol::direct::{SessionPath, SessionState};
use pb_mapper_server::RelayServerOptions;
use pb_mapper_testkit::{
    READY_TIMEOUT, Relay, admin_client, loopback_direct, loopback_quic, publish, subscribe,
    tcp_echo, udp_echo,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

async fn roundtrip(socket: &mut TcpStream) {
    tokio::time::timeout(READY_TIMEOUT, async {
        let payload = vec![0x37; 32_000];
        socket.write_all(&payload).await.unwrap();
        let mut got = vec![0; payload.len()];
        socket.read_exact(&mut got).await.unwrap();
        assert_eq!(got, payload);
    })
    .await
    .unwrap();
}

fn allow_direct() -> RegisterOptions {
    RegisterOptions {
        allow_direct: true,
        ..Default::default()
    }
}

fn auto_path() -> ConnectOptions {
    ConnectOptions {
        path: PathPolicy::Auto,
        ..Default::default()
    }
}

#[tokio::test]
async fn direct_tcp_survives_server_restart_and_local_stop_closes_it() {
    direct_survives_coordinator_loss(RelayTransport::Tcp).await;
}

#[cfg(feature = "relay-quic")]
#[tokio::test]
async fn direct_sessions_survive_quic_coordinator_loss() {
    direct_survives_coordinator_loss(RelayTransport::Quic).await;
}

async fn direct_survives_coordinator_loss(carrier: RelayTransport) {
    let mut relay = Relay::start_with_options(
        "direct-tcp",
        RelayServerOptions {
            direct: Some(loopback_direct()),
            quic: (carrier == RelayTransport::Quic).then(loopback_quic),
        },
    )
    .await;
    let client = admin_client(relay.addr())
        .with_relay_transport(carrier)
        .unwrap();
    let backend = tcp_echo(None).await;
    let registration = publish(
        &client,
        "echo",
        backend.addr(),
        Transport::Tcp,
        true,
        allow_direct(),
    )
    .await;
    let (connection, addr) = subscribe(&client, "echo", Transport::Tcp, auto_path()).await;
    let mut socket = TcpStream::connect(addr).await.unwrap();
    roundtrip(&mut socket).await;
    assert_eq!(connection.diagnostics().direct.unwrap().active_direct, 1);
    // The attempt has settled, and the history says how the session travels.
    let seen = connection.diagnostics();
    assert_eq!(seen.direct.as_ref().unwrap().attempt, None);
    assert!(
        seen.events
            .iter()
            .any(|event| matches!(event.kind, TunnelEventKind::DirectEstablished { .. }))
    );
    assert_eq!(registration.diagnostics().direct.unwrap().active_direct, 1);
    assert_eq!(backend.reached(), 1, "exactly one backend connection");
    // A completed sibling must drain its final response and leave this session alive.
    let mut sibling = TcpStream::connect(addr).await.unwrap();
    let payload = vec![0x63; 256_000];
    sibling.write_all(&payload).await.unwrap();
    sibling.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(READY_TIMEOUT, sibling.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.len(), payload.len());
    assert!(response == payload);
    relay.stop_server().await;
    roundtrip(&mut socket).await;
    relay.restart_server(Some(loopback_direct())).await;
    // Existing data survives a fresh coordinator incarnation. Both endpoints
    // replay observations, which do not recreate a grant or a relay attach.
    let restored = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let sessions_result = client.sessions().await;
            if let Ok(sessions) = sessions_result
                && let Some(session) = sessions.into_iter().find(|s| {
                    s.restored
                        && !s.stale
                        && s.consumer
                            .as_ref()
                            .is_some_and(|r| r.report.state == SessionState::Active)
                        && s.provider
                            .as_ref()
                            .is_some_and(|r| r.report.state == SessionState::Active)
                })
            {
                break session;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(!restored.conflicting);
    roundtrip(&mut socket).await;
    registration.stop().await.unwrap();
    let result = tokio::time::timeout(READY_TIMEOUT, socket.read_u8())
        .await
        .unwrap();
    assert!(
        result.is_err(),
        "provider stop must close the direct session"
    );
    connection.stop().await.unwrap();
}

#[tokio::test]
async fn udp_direct_preserves_datagrams_and_revocation_leaves_active_session_owned_by_client() {
    udp_direct(LanTransport::Quic).await;
}

/// The same over TCP within the LAN, which loopback stands in for here: the
/// datagrams keep their boundaries on a TLS stream as on QUIC's.
#[tokio::test]
async fn udp_direct_over_lan_tcp_preserves_datagrams() {
    udp_direct(LanTransport::Tcp).await;
}

async fn udp_direct(lan_transport: LanTransport) {
    let relay = Relay::start_with_direct("direct-udp", Some(loopback_direct())).await;
    let (id, credential) = relay
        .issue_credential(Duration::from_secs(600), "direct-udp")
        .await;
    let client = Client::from_credential(relay.addr().to_string(), credential, false, None);
    let backend = udp_echo(None).await;
    let registration = publish(
        &client,
        "udp",
        backend.addr(),
        Transport::Udp,
        true,
        allow_direct(),
    )
    .await;
    let (connection, addr) = subscribe(
        &client,
        "udp",
        Transport::Udp,
        ConnectOptions {
            path: PathPolicy::Auto,
            lan_transport,
            ..Default::default()
        },
    )
    .await;
    let application = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    application.connect(addr).await.unwrap();
    // One byte, a datagram under any path's MTU, and one only IP
    // fragmentation carries, which both UDP hops here cross on loopback.
    let mut sizes = vec![1, 1200];
    if loopback_carries(8000).await {
        sizes.push(8000);
    } else {
        eprintln!(
            "loopback drops an 8000-byte UDP datagram on this host (WSL's mirrored \
             networking routes 127.0.0.1 over a 1500-byte link that drops fragments); \
             the case sends nothing it cannot carry"
        );
    }
    for size in sizes {
        let payload = vec![size as u8; size];
        application.send(&payload).await.unwrap();
        let mut response = vec![0; 65536];
        let len = tokio::time::timeout(READY_TIMEOUT, application.recv(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response[..len], &payload);
    }
    let direct = connection.diagnostics().direct.unwrap();
    assert_eq!(direct.active_direct, 1);
    // Over QUIC, which of this machine's addresses wins varies; TCP is offered
    // on loopback alone, as the relay is on it.
    let transport = match lan_transport {
        LanTransport::Quic => DirectTransport::Quic,
        LanTransport::Tcp => DirectTransport::Tcp,
    };
    assert_eq!(direct.link.unwrap().transport, transport);
    // Each poll is a status request to the relay, so it is paced like the
    // relay-restart poll above rather than sent back to back.
    let observation = tokio::time::timeout(READY_TIMEOUT, async {
        loop {
            let sessions = client.sessions().await.unwrap();
            if let Some(s) = sessions.into_iter().find(|s| !s.stale) {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(!observation.conflicting);
    assert_eq!(
        observation.consumer.unwrap().report.path,
        SessionPath::Direct
    );
    // Other namespaces must not see these authenticated endpoint observations.
    let outsider = admin_client(relay.addr());
    assert!(outsider.sessions().await.unwrap().is_empty());
    relay.revoke(id).await;
    // Auto mode preserves an established direct session, but a new tunnel with
    // the revoked credential must still report a terminal authentication error.
    let rejected = client
        .connect_with_options(
            ConnectRequest {
                key: "udp".into(),
                local_addr: "127.0.0.1:0".into(),
                transport: Transport::Udp,
            },
            auto_path(),
        )
        .await
        .unwrap();
    let error = tokio::time::timeout(READY_TIMEOUT, rejected.wait_ready())
        .await
        .expect("permanent authentication failure must terminate readiness")
        .unwrap_err();
    assert!(matches!(
        error,
        pb_mapper_client::sdk::Error::TunnelFailed { .. }
    ));
    rejected.stop().await.unwrap();
    application.send(b"still direct").await.unwrap();
    let mut response = [0; 100];
    let len = tokio::time::timeout(READY_TIMEOUT, application.recv(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&response[..len], b"still direct");
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}

/// Whether this host's loopback delivers a UDP datagram of `size` bytes
/// between two sockets on 127.0.0.1, as a datagram tunnel's application and
/// backend need it to before pb-mapper sees anything.
async fn loopback_carries(size: usize) -> bool {
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let payload = vec![0x5a; size];
    sender
        .send_to(&payload, receiver.local_addr().unwrap())
        .await
        .unwrap();
    let mut got = vec![0; size + 1];
    matches!(
        tokio::time::timeout(Duration::from_secs(1), receiver.recv(&mut got)).await,
        Ok(Ok(len)) if got[..len] == payload[..]
    )
}

#[tokio::test]
async fn either_side_can_keep_connections_on_the_existing_relay_path() {
    // One server without the capability, then a capable server with an opt-out provider.
    for server_direct in [false, true] {
        let relay =
            Relay::start_with_direct("direct-disabled", server_direct.then(loopback_direct)).await;
        let client = admin_client(relay.addr());
        let backend = tcp_echo(None).await;
        let registration = publish(
            &client,
            "relay",
            backend.addr(),
            Transport::Tcp,
            true,
            RegisterOptions {
                allow_direct: !server_direct,
                ..Default::default()
            },
        )
        .await;
        let (connection, addr) = subscribe(&client, "relay", Transport::Tcp, auto_path()).await;
        let mut socket = TcpStream::connect(addr).await.unwrap();
        roundtrip(&mut socket).await;
        assert_eq!(connection.diagnostics().direct.unwrap().direct_sessions, 0);
        // The attempt ends where the offer is missing: at the relay while
        // negotiating, at the publisher when asking for a grant.
        let stage = if server_direct {
            DirectStage::Discovering
        } else {
            DirectStage::Negotiating
        };
        let seen = connection.diagnostics();
        assert_eq!(seen.direct.as_ref().unwrap().attempt, None);
        assert!(seen.events.iter().any(|event| event.kind
            == TunnelEventKind::DirectFallback {
                stage,
                reason: FallbackReason::NotOffered
            }));
        // The share counts the session too, on the relay it came over.
        let shared = registration.diagnostics().direct.unwrap();
        assert_eq!((shared.relay_sessions, shared.direct_sessions), (1, 0));
        assert_eq!(backend.reached(), 1);
        connection.stop().await.unwrap();
        registration.stop().await.unwrap();
    }
}
