//! A test binary of its own: it announces a network change, which is
//! process-wide and would retire every other test's QUIC carrier with it.
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
    net::{TcpListener, TcpStream},
};
use tokio_util::task::AbortOnDropHandle;

fn client(relay: &Relay, transport: RelayTransport) -> Client {
    Client::from_credential(relay.addr().to_string(), admin_credential(), false, None)
        .with_relay_transport(transport)
        .unwrap()
}

/// A network change retires the connection a download is on, and the
/// registration, reconnecting, moves to a new one. When the service then
/// finishes, its stream is the retired connection's last: dropping it must
/// not be what closes the connection, which would discard the tail still in
/// flight.
#[tokio::test]
async fn a_retired_connection_delivers_its_last_streams_tail() {
    const SIZE: usize = 64 << 20;
    let relay = Relay::start_with_quic("quic-retired-tail").await;
    let publisher = client(&relay, RelayTransport::Quic);
    let consumer = client(&relay, RelayTransport::Tcp);
    let download = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = download.local_addr().unwrap();
    let _download = AbortOnDropHandle::new(tokio::spawn(async move {
        let (mut stream, _) = download.accept().await.unwrap();
        stream.write_all(&vec![5; SIZE]).await.unwrap();
        stream.shutdown().await.unwrap();
    }));
    let registration = publisher
        .register_with_options(
            RegisterRequest {
                key: "download".into(),
                local_addr: backend.to_string(),
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
        .wait_ready_timeout(READY_TIMEOUT)
        .await
        .unwrap();
    let addr = pb_mapper_testkit::reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = consumer
        .connect_with_options(
            ConnectRequest {
                key: "download".into(),
                local_addr: addr.to_string(),
                transport: Transport::Tcp,
            },
            ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    connection.wait_ready_timeout(READY_TIMEOUT).await.unwrap();

    // The requester is done talking: once the service finishes, nothing
    // else holds the publisher's stream open.
    let mut socket = TcpStream::connect(addr).await.unwrap();
    socket.shutdown().await.unwrap();
    socket.read_u8().await.unwrap();
    publisher.notify_network_change();
    // The relay drops the registration's control stream; the download goes on.
    let retired = consumer
        .admin()
        .unwrap()
        .retire_connections(None, "download".into(), None)
        .await
        .unwrap();
    assert!(retired > 0);
    tokio::time::timeout(READY_TIMEOUT, async {
        while publisher.relay_diagnostics().await.connections_created < 2
            || registration.status() != TunnelStatus::Connected
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the registration moves to a new connection");

    let mut rest = Vec::new();
    tokio::time::timeout(READY_TIMEOUT, socket.read_to_end(&mut rest))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rest.len() + 1, SIZE, "the download's tail must arrive");
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}
