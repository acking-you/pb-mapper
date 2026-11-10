//! A test binary of its own: it announces a network change, which is
//! process-wide and would retire every other test's QUIC carrier with it.
#![cfg(feature = "relay-quic")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use pb_mapper_client::sdk::{
    Client, ConnectOptions, PathPolicy, RegisterOptions, RelayTransport, Transport, TunnelStatus,
};
use pb_mapper_testkit::{READY_TIMEOUT, Relay, admin_client, publish, subscribe};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::task::AbortOnDropHandle;

fn client(relay: &Relay, transport: RelayTransport) -> Client {
    admin_client(relay.addr())
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
    let registration = publish(
        &publisher,
        "download",
        backend,
        Transport::Tcp,
        false,
        RegisterOptions {
            allow_direct: false,
            ..Default::default()
        },
    )
    .await;
    let (connection, addr) = subscribe(
        &consumer,
        "download",
        Transport::Tcp,
        ConnectOptions {
            path: PathPolicy::RelayOnly,
            ..Default::default()
        },
    )
    .await;

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
    // What is asserted is that the tail arrives, not how fast: a debug build
    // moves the 64 MiB in about seven seconds alone and took eleven on a
    // loaded machine, so the bound only stops a hang.
    tokio::time::timeout(Duration::from_secs(60), socket.read_to_end(&mut rest))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rest.len() + 1, SIZE, "the download's tail must arrive");
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}
