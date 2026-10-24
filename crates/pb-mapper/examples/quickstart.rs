//! A zero-setup tour: an embedded relay, a published echo service, and a local
//! port that reaches it — direct over QUIC, with the relay as the fallback.
//!
//! ```text
//! cargo run -p pb-mapper --example quickstart
//! ```
//!
//! Everything runs in this process on loopback. Against a deployed relay, drop
//! step 1 and point `ClientConfig::server` at it; the rest is unchanged.

use std::net::SocketAddr;
use std::time::Instant;

use pb_mapper::{Client, ClientConfig, ConnectRequest, RegisterRequest, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. A relay: TCP for the relay itself, the same port over UDP for direct
    //    coordination. `pb-mapper server` does exactly this by default.
    let admin_key = pb_mapper_auth::generate_admin_key();
    let state_dir =
        std::env::temp_dir().join(format!("pb-mapper-quickstart-{}", std::process::id()));
    let auth = pb_mapper_auth::AuthRuntime::start(
        admin_key.as_bytes().try_into()?,
        pb_mapper_auth::AuthConfig {
            state_dir: state_dir.clone(),
            ..Default::default()
        },
    )
    .await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let relay = listener.local_addr()?;
    let shutdown = CancellationToken::new();
    tokio::spawn(pb_mapper_server::run_server_on_listener_with_direct(
        listener,
        shutdown.clone(),
        None,
        false,
        auth,
        Some(pb_mapper_server::DirectServerConfig {
            listen: relay,
            // Announce only the port: clients pair it with the relay address
            // they dialed.
            advertise: SocketAddr::from(([0, 0, 0, 0], 0)),
        }),
    ));

    // 2. The private service: a TCP echo server.
    let echo = TcpListener::bind("127.0.0.1:0").await?;
    let echo_addr = echo.local_addr()?;
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = socket.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });

    // 3. Publish it. Direct sessions are allowed by default.
    let client = Client::new(ClientConfig {
        server: relay.to_string(),
        credential: admin_key,
        keep_alive: true,
        namespace: None,
    })?;
    let registration = client
        .register(RegisterRequest {
            key: "echo".into(),
            local_addr: echo_addr.to_string(),
            transport: Transport::Tcp,
            codec: false,
            force_namespace: false,
        })
        .await?;
    registration.wait_ready().await?;
    println!("registered `echo` on relay {relay}");

    // 4. Reach it from a local port: direct first, relay fallback, reconnects
    //    handled by the SDK.
    let local = free_local_addr().await?;
    let connection = client
        .connect(ConnectRequest {
            key: "echo".into(),
            local_addr: local.to_string(),
            transport: Transport::Tcp,
        })
        .await?;
    connection.wait_ready().await?;
    println!("`echo` is reachable at {local}");

    // 5. Use it like the service were local.
    let mut socket = TcpStream::connect(local).await?;
    let mut reply = [0_u8; 5];
    for round in 1..=3 {
        let started = Instant::now();
        socket.write_all(b"hello").await?;
        socket.read_exact(&mut reply).await?;
        println!("round trip {round}: {:?}", started.elapsed());
    }

    // 6. Which path did it take?
    let diagnostics = connection.diagnostics();
    if let Some(path) = &diagnostics.direct {
        println!(
            "direct sessions: {}, relay sessions: {}, fallbacks: {}",
            path.direct_sessions, path.relay_sessions, path.fallbacks
        );
    }
    for event in &diagnostics.events {
        println!("event #{}: {:?}", event.seq, event.kind);
    }

    drop(socket);
    connection.stop().await?;
    registration.stop().await?;
    shutdown.cancel();
    let _ = std::fs::remove_dir_all(state_dir);
    Ok(())
}

/// A loopback address nothing is listening on, for the local end of the tunnel.
async fn free_local_addr() -> std::io::Result<SocketAddr> {
    TcpListener::bind("127.0.0.1:0").await?.local_addr()
}
