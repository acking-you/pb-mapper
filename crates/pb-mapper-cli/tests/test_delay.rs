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
