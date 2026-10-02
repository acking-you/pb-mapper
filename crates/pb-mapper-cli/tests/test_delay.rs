// See the note in `regression.rs`: the whole file is test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tunnel tests over the full `server` + `register` + `connect` path.
//!
//! Every case builds its own relay, echo server, and tunnel on reserved loopback
//! ports, so the four transport/codec combinations run concurrently and none of
//! them collides with a relay already running on the machine. The scaffolding
//! lives in `pb-mapper-testkit`, so any other test file can stand up the same
//! flow — see `temporary_credential_e2e.rs`.

use pb_mapper_testkit::{Transport, TunnelHarness, run_echo_delay, run_udp_datagram_echo};
use uni_stream::stream::TcpStreamProvider;

/// Push traffic through the tunnel and assert every payload comes back byte for byte.
///
/// These cases verify the logic. For latency numbers, run a separate binary.
async fn assert_tunnel_echoes(transport: Transport, need_codec: bool) {
    let harness = TunnelHarness::start(transport, need_codec).await;
    match transport {
        Transport::Tcp => run_echo_delay::<TcpStreamProvider, _>(harness.tunnel_addr(), 10).await,
        Transport::Udp => run_udp_datagram_echo(harness.tunnel_addr(), 10, 8, None).await,
    }
}

#[tokio::test]
async fn tcp_tunnel_echoes_without_codec() {
    assert_tunnel_echoes(Transport::Tcp, false).await;
}

#[tokio::test]
async fn tcp_tunnel_echoes_with_codec() {
    assert_tunnel_echoes(Transport::Tcp, true).await;
}

#[tokio::test]
async fn udp_tunnel_echoes_without_codec() {
    assert_tunnel_echoes(Transport::Udp, false).await;
}

#[tokio::test]
async fn udp_tunnel_echoes_with_codec() {
    assert_tunnel_echoes(Transport::Udp, true).await;
}

/// Exercise a long-lived stream with a history-sized burst followed by small
/// interactive RPCs. Each exchange verifies every byte, including the FIN tail.
#[tokio::test]
async fn large_history_then_repeated_small_exchanges() {
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    for codec in [false, true] {
        let harness = TunnelHarness::start(Transport::Tcp, codec).await;
        let mut socket = TcpStream::connect(harness.tunnel_addr()).await.unwrap();
        socket.set_nodelay(true).unwrap();
        let payload = vec![0x5a; 20 << 20];
        let mut returned = vec![0; payload.len()];
        let (mut reader, mut writer) = socket.split();
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::try_join!(writer.write_all(&payload), reader.read_exact(&mut returned)).unwrap();
        })
        .await
        .expect("large payload must drain without deadlock");
        assert_eq!(returned, payload);
        let mut timings = Vec::new();
        for i in 0_u64..200 {
            let request = i.to_be_bytes();
            let start = Instant::now();
            // Separate tiny writes expose Nagle/delayed-ACK interactions.
            writer.write_all(&request[..1]).await.unwrap();
            writer.write_all(&request[1..]).await.unwrap();
            let mut response = [0; 8];
            tokio::time::timeout(Duration::from_secs(2), reader.read_exact(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response, request);
            timings.push(start.elapsed());
        }
        writer.shutdown().await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), reader.read_u8())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        timings.sort();
        eprintln!(
            "codec={codec}, 200 exchanges, p50={:?}, p95={:?}, p99={:?}",
            timings[100], timings[190], timings[198]
        );
    }
}
