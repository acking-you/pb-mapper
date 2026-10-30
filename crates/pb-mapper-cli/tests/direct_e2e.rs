#![cfg(feature = "p2p")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, DirectStage, PathPolicy, RegisterOptions,
    RegisterRequest, Transport, TunnelEventKind,
};
use pb_mapper_server::DirectServerConfig;
use pb_mapper_testkit::{READY_TIMEOUT, Relay, admin_credential, reserve_addr};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::task::AbortOnDropHandle;

async fn echo() -> (
    std::net::SocketAddr,
    Arc<AtomicUsize>,
    AbortOnDropHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let accepted = count.clone();
    let task = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                result=listener.accept()=>{let (mut socket,_)=result.unwrap(); accepted.fetch_add(1,Ordering::Relaxed); tasks.spawn(async move {let (mut r,mut w)=socket.split();let _=tokio::io::copy(&mut r,&mut w).await;});}
                Some(_)=tasks.join_next()=>{}
            }
        }
    }));
    (addr, count, task)
}
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

#[tokio::test]
async fn direct_tcp_survives_server_restart_and_local_stop_closes_it() {
    direct_survives_coordinator_loss(pb_mapper_client::sdk::RelayTransport::Tcp).await;
}

#[cfg(feature = "relay-quic")]
#[tokio::test]
async fn direct_sessions_survive_quic_coordinator_loss() {
    direct_survives_coordinator_loss(pb_mapper_client::sdk::RelayTransport::Quic).await;
}

async fn direct_survives_coordinator_loss(carrier: pb_mapper_client::sdk::RelayTransport) {
    let mut relay = Relay::start_with_options(
        "direct-tcp",
        pb_mapper_server::RelayServerOptions {
            direct: Some(DirectServerConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                advertise: "127.0.0.1:0".parse().unwrap(),
            }),
            quic: (carrier == pb_mapper_client::sdk::RelayTransport::Quic).then(|| {
                pb_mapper_server::QuicServerConfig {
                    listen: "127.0.0.1:0".parse().unwrap(),
                    advertise_port: 0,
                }
            }),
        },
    )
    .await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), false, None)
        .with_relay_transport(carrier)
        .unwrap();
    let (backend, count, _echo) = echo().await;
    let registration = client
        .register_with_options(
            RegisterRequest {
                key: "echo".into(),
                local_addr: backend.to_string(),
                transport: Transport::Tcp,
                codec: true,
                force_namespace: false,
            },
            RegisterOptions {
                allow_direct: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    registration
        .wait_ready_timeout(READY_TIMEOUT)
        .await
        .unwrap();
    let addr = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = client
        .connect_with_options(
            ConnectRequest {
                key: "echo".into(),
                local_addr: addr.to_string(),
                transport: Transport::Tcp,
            },
            ConnectOptions {
                path: PathPolicy::Auto,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
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
    assert_eq!(
        count.load(Ordering::Relaxed),
        1,
        "exactly one backend connection"
    );
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
    relay
        .restart_server(Some(DirectServerConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            advertise: "127.0.0.1:0".parse().unwrap(),
        }))
        .await;
    // Existing data survives a fresh coordinator incarnation. Both endpoints
    // replay observations, which do not recreate a grant or a relay attach.
    let restored = tokio::time::timeout(std::time::Duration::from_secs(25), async {
        loop {
            let sessions_result = client.sessions().await;
            if let Ok(sessions) = sessions_result
                && let Some(session) = sessions.into_iter().find(|s| {
                    s.restored
                        && !s.stale
                        && s.consumer.as_ref().is_some_and(|r| {
                            r.report.state == pb_mapper_protocol::direct::SessionState::Active
                        })
                        && s.provider.as_ref().is_some_and(|r| {
                            r.report.state == pb_mapper_protocol::direct::SessionState::Active
                        })
                })
            {
                break session;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
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
    use std::time::Duration;
    use tokio::net::UdpSocket;
    let relay = Relay::start_with_direct(
        "direct-udp",
        Some(DirectServerConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            advertise: "127.0.0.1:0".parse().unwrap(),
        }),
    )
    .await;
    let (id, credential) = relay
        .issue_credential(Duration::from_secs(600), "direct-udp")
        .await;
    let client = Client::from_credential(relay.addr().to_string(), credential, false, None);
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let backend = echo.local_addr().unwrap();
    let _echo = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut bytes = vec![0; 65536];
        loop {
            let (len, from) = echo.recv_from(&mut bytes).await.unwrap();
            echo.send_to(&bytes[..len], from).await.unwrap();
        }
    }));
    let registration = client
        .register_with_options(
            RegisterRequest {
                key: "udp".into(),
                local_addr: backend.to_string(),
                transport: Transport::Udp,
                codec: true,
                force_namespace: false,
            },
            RegisterOptions {
                allow_direct: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    registration
        .wait_ready_timeout(READY_TIMEOUT)
        .await
        .unwrap();
    let addr = reserve_addr(pb_mapper_testkit::Transport::Udp).await;
    let connection = client
        .connect_with_options(
            ConnectRequest {
                key: "udp".into(),
                local_addr: addr.to_string(),
                transport: Transport::Udp,
            },
            ConnectOptions {
                path: PathPolicy::Auto,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
    let application = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    application.connect(addr).await.unwrap();
    for size in [1, 1200, 8000] {
        let payload = vec![size as u8; size];
        application.send(&payload).await.unwrap();
        let mut response = vec![0; 65536];
        let len = tokio::time::timeout(READY_TIMEOUT, application.recv(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response[..len], &payload);
    }
    assert_eq!(connection.diagnostics().direct.unwrap().active_direct, 1);
    let observation = tokio::time::timeout(READY_TIMEOUT, async {
        loop {
            let sessions = client.sessions().await.unwrap();
            if let Some(s) = sessions.into_iter().find(|s| !s.stale) {
                break s;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!observation.conflicting);
    assert_eq!(
        observation.consumer.unwrap().report.path,
        pb_mapper_protocol::direct::SessionPath::Direct
    );
    // Other namespaces must not see these authenticated endpoint observations.
    let outsider =
        Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
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
            ConnectOptions {
                path: PathPolicy::Auto,
                ..Default::default()
            },
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

#[tokio::test]
async fn either_side_can_keep_connections_on_the_existing_relay_path() {
    // One server without the capability, then a capable server with an opt-out provider.
    for server_direct in [false, true] {
        let relay = Relay::start_with_direct(
            "direct-disabled",
            server_direct.then(|| DirectServerConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                advertise: "127.0.0.1:0".parse().unwrap(),
            }),
        )
        .await;
        let client =
            Client::from_credential(relay.addr().to_string(), admin_credential(), false, None);
        let (backend, count, _echo) = echo().await;
        let registration = client
            .register_with_options(
                RegisterRequest {
                    key: "relay".into(),
                    local_addr: backend.to_string(),
                    transport: Transport::Tcp,
                    codec: true,
                    force_namespace: false,
                },
                RegisterOptions {
                    allow_direct: !server_direct,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        registration
            .wait_ready_timeout(READY_TIMEOUT)
            .await
            .unwrap();
        let addr = reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
        let connection = client
            .connect_with_options(
                ConnectRequest {
                    key: "relay".into(),
                    local_addr: addr.to_string(),
                    transport: Transport::Tcp,
                },
                ConnectOptions {
                    path: PathPolicy::Auto,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();
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
        assert!(
            seen.events
                .iter()
                .any(|event| event.kind == TunnelEventKind::DirectFallback { stage })
        );
        assert!(registration.diagnostics().direct.is_none());
        assert_eq!(count.load(Ordering::Relaxed), 1);
        connection.stop().await.unwrap();
        registration.stop().await.unwrap();
    }
}
