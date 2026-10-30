//! Loopback comparison, not a WAN prediction:
//! cargo run -p pb-mapper-cli --release --example relay_transport_bench
#![allow(clippy::unwrap_used, clippy::expect_used)]

use pb_mapper_client::sdk::{
    Client, ConnectOptions, ConnectRequest, PathPolicy, RegisterOptions, RegisterRequest,
    RelayTransport, Transport,
};
use pb_mapper_testkit::{READY_TIMEOUT, Relay, admin_credential};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::task::AbortOnDropHandle;

#[tokio::main]
async fn main() {
    for carrier in [RelayTransport::Tcp, RelayTransport::Quic] {
        for codec in [false, true] {
            measure(carrier, codec).await;
        }
    }
}

async fn measure(carrier: RelayTransport, codec: bool) {
    let relay = Relay::start_with_quic("carrier-bench").await;
    let client = || {
        Client::from_credential(relay.addr().to_string(), admin_credential(), false, None)
            .with_relay_transport(carrier)
            .unwrap()
    };
    let publisher = client();
    let consumer = client();
    let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = backend.local_addr().unwrap();
    let _echo = AbortOnDropHandle::new(tokio::spawn(async move {
        let (mut socket, _) = backend.accept().await.unwrap();
        socket.set_nodelay(true).unwrap();
        let (mut read, mut write) = socket.split();
        tokio::io::copy(&mut read, &mut write).await.unwrap();
        write.shutdown().await.unwrap();
    }));
    let registration = publisher
        .register_with_options(
            RegisterRequest {
                key: "bench".into(),
                local_addr: backend_addr.to_string(),
                transport: Transport::Tcp,
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
    let local = pb_mapper_testkit::reserve_addr(pb_mapper_testkit::Transport::Tcp).await;
    let connection = consumer
        .connect_with_options(
            ConnectRequest {
                key: "bench".into(),
                local_addr: local.to_string(),
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
    let mut socket = TcpStream::connect(local).await.unwrap();
    socket.set_nodelay(true).unwrap();
    let (mut read, mut write) = socket.split();
    // Warm the route and handshake before timing steady-state data transfer.
    write.write_all(b"warm").await.unwrap();
    let mut warm = [0; 4];
    read.read_exact(&mut warm).await.unwrap();
    assert_eq!(&warm, b"warm");
    let payload = vec![0x5a; 64 << 20];
    let mut returned = vec![0; payload.len()];
    let start = Instant::now();
    tokio::time::timeout(Duration::from_secs(60), async {
        tokio::try_join!(write.write_all(&payload), read.read_exact(&mut returned)).unwrap();
    })
    .await
    .unwrap();
    let elapsed = start.elapsed();
    assert_eq!(payload, returned);
    let mut timings = Vec::new();
    for i in 0u64..200 {
        let request = i.to_be_bytes();
        let start = Instant::now();
        write.write_all(&request[..1]).await.unwrap();
        write.write_all(&request[1..]).await.unwrap();
        let mut response = [0; 8];
        tokio::time::timeout(Duration::from_secs(5), read.read_exact(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request, response);
        timings.push(start.elapsed());
    }
    write.shutdown().await.unwrap();
    let mut tail = Vec::new();
    tokio::time::timeout(READY_TIMEOUT, read.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(tail.is_empty());
    timings.sort();
    println!(
        "{carrier:?} codec={codec} 64MiB echo={elapsed:?} payload={:.1}MiB/s p50={:?} p95={:?} p99={:?}",
        64.0 / elapsed.as_secs_f64(),
        timings[100],
        timings[190],
        timings[198]
    );
    println!("consumer: {:?}", consumer.relay_diagnostics().await);
    connection.stop().await.unwrap();
    registration.stop().await.unwrap();
}
