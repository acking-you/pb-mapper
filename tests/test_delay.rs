use pb_mapper::common::config::init_tracing;
use pb_mapper::common::listener::{ListenerProvider, TcpListenerProvider, UdpListenerProvider};
use pb_mapper::common::stream::{StreamProvider, TcpStreamProvider, UdpStreamProvider};
use pb_mapper::local::client::run_client_side_cli;
use pb_mapper::local::server::run_server_side_cli;
use pb_mapper::pb_server::run_server;
use pb_mapper::utils::addr::ToSocketAddrs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

struct TimerTickGurad {
    ins: Instant,
}

impl TimerTickGurad {
    fn new() -> Self {
        Self {
            ins: Instant::now(),
        }
    }
}

impl Drop for TimerTickGurad {
    fn drop(&mut self) {
        let end = Instant::now();
        let duration = end - self.ins;
        println!("duration:{duration:?}");
    }
}

use pb_mapper::common::listener::StreamAccept;

async fn echo_server<P: ListenerProvider>(
    server_addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = P::bind(server_addr).await?;
    println!("run local server:{server_addr}");
    loop {
        // Accept incoming connections
        let (mut stream, addr) = listener.accept().await?;
        println!("Connected from {}", addr);

        // Process each connection concurrently
        tokio::spawn(async move {
            // Read data from client
            let mut buf = vec![0; 1024];
            loop {
                let n = match stream.read(&mut buf).await {
                    Ok(n) => n,
                    Err(e) => {
                        println!("Error reading: {}", e);
                        return;
                    }
                };

                // If no data received, assume disconnect
                if n == 0 {
                    return;
                }

                // Echo data back to client
                if let Err(e) = stream.write_all(&buf[..n]).await {
                    println!("Error writing: {}", e);
                    return;
                }

                println!("Echoed {} bytes to {}", n, addr);
            }
        });
    }
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_tcp_echo_server() -> Result<(), Box<dyn std::error::Error>> {
    echo_server::<TcpListenerProvider>("0.0.0.0:22222").await
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_udp_echo_server() -> Result<(), Box<dyn std::error::Error>> {
    echo_server::<UdpListenerProvider>("0.0.0.0:33333").await
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_pb_mapper_server() {
    init_tracing();
    run_server("0.0.0.0:11111").await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_pb_mapper_tcp_server_cli() {
    init_tracing();
    run_server_side_cli::<TcpStreamProvider, _>("127.0.0.1:22222", "127.0.0.1:11111", "a".into())
        .await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_pb_mapper_udp_server_cli() {
    init_tracing();
    run_server_side_cli::<UdpStreamProvider, _>("127.0.0.1:33333", "127.0.0.1:11111", "b".into())
        .await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_pb_mapper_tcp_client_cli() {
    init_tracing();
    run_client_side_cli::<TcpListenerProvider, _>("0.0.0.0:12345", "127.0.0.1:11111", "a".into())
        .await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_pb_mapper_udp_client_cli() {
    init_tracing();
    run_client_side_cli::<UdpListenerProvider, _>("0.0.0.0:23456", "127.0.0.1:11111", "b".into())
        .await;
}

async fn echo_delay<P: StreamProvider, A: ToSocketAddrs + 'static + Send>(addr: A) {
    let mut stream = P::from_addr(addr).await.unwrap();
    let mut buf = [0; 1024];
    let expected = b"abc";
    for _ in 0..10 {
        let n = {
            let _guard = TimerTickGurad::new();
            stream.write_all(expected).await.unwrap();
            stream.read(&mut buf).await.unwrap()
        };

        assert_eq!(expected, &buf[..n]);
    }
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn test_tcp_echo_delay() {
    // Execute [`run_echo_tcp_server`], [`run_pb_mapper_server`],
    // [`run_pb_mapper_tcp_server_cli`], [`run_pb_mapper_tcp_client_cli`} manually before running
    // this test.
    echo_delay::<TcpStreamProvider, _>("127.0.0.1:12345").await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn test_raw_tcp_echo_delay() {
    // Execute [`run_echo_tcp_server`] manually before running this test.
    echo_delay::<TcpStreamProvider, _>("127.0.0.1:22222").await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn test_udp_echo_delay() {
    // Execute [`run_echo_tcp_server`], [`run_pb_mapper_server`],
    // [`run_pb_mapper_udp_server_cli`], [`run_pb_mapper_udp_client_cli`} manually before running
    // this test.
    echo_delay::<UdpStreamProvider, _>("127.0.0.1:23456").await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn test_raw_udp_echo_delay() {
    // Execute [`run_echo_tcp_server`] manually before running this test.
    echo_delay::<UdpStreamProvider, _>("127.0.0.1:33333").await;
}
