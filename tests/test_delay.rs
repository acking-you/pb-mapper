use pb_mapper::common::config::init_tracing;
use pb_mapper::common::listener::TcpListenerProvider;
use pb_mapper::common::stream::TcpStreamProvider;
use pb_mapper::local::client::run_client_side_cli;
use pb_mapper::local::server::run_server_side_cli;
use pb_mapper::pb_server::run_server;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};
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

use tokio::net::TcpListener;

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_echo_server() -> Result<(), Box<dyn std::error::Error>> {
    let addr = match std::env::var("ADDR") {
        Ok(addr) => addr,
        Err(_) => "0.0.0.0:22222".into(),
    };
    // Bind to address and port
    let listener = TcpListener::bind(addr.as_str()).await?;

    println!("Server listening on {}", listener.local_addr()?);

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
async fn run_pb_mapper_server() {
    init_tracing();
    let addr = match std::env::var("ADDR") {
        Ok(addr) => addr,
        Err(_) => "0.0.0.0:11111".into(),
    };
    run_server(addr.as_str()).await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_pb_mapper_server_cli() {
    init_tracing();
    run_server_side_cli::<TcpStreamProvider, _>("127.0.0.1:1080", "127.0.0.1:11111", "a".into())
        .await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn run_pb_mapper_client_cli() {
    init_tracing();
    run_client_side_cli::<TcpListenerProvider, _>("0.0.0.0:12345", "127.0.0.1:11111", "a".into())
        .await;
}

async fn echo_delay<A: ToSocketAddrs>(addr: A) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
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
async fn test_echo_delay() {
    // Execute [`run_echo_server`], [`run_pb_mapper_server`], [`run_pb_mapper_server_cli`],
    // [`run_pb_mapper_client_cli`} manually before running this test.
    echo_delay("127.0.0.1:12345").await;
}

#[ignore = "Must be executed manually"]
#[tokio::test]
async fn test_raw_echo_delay() {
    // Execute [`run_echo_server`] manually before running this test.
    echo_delay("127.0.0.1:22222").await;
}
