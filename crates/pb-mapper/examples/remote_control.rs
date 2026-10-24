//! Remote control for an agent harness, the pattern pb-mapper is built around.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<administrator key> \
//!   cargo run -p pb-mapper --example remote_control
//! ```
//!
//! The operator keeps the administrator key. For each remote session it:
//!
//! 1. issues a short-lived `pbmt1_` credential with its own namespace;
//! 2. gives it to the device, which publishes its control endpoint — here an
//!    echo server standing in for a harness's HTTP or WebSocket port — under a
//!    well-known key;
//! 3. gives the same credential to the controller, which gets that endpoint on
//!    a local port, peer to peer where the network allows;
//! 4. revokes the credential when the session ends: relay connections close at
//!    once and new direct setups are refused.
//!
//! Two sessions run side by side to show the isolation: both publish under
//! the same key, and neither can see or reach the other's. Device and
//! controller share this process only to keep the example self-contained;
//! each is a separate `Client`, exactly as on two machines.

use std::net::SocketAddr;
use std::time::Duration;

use pb_mapper::{Client, ClientConfig, ConnectRequest, RegisterRequest, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const SERVICE: &str = "harness";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("PB_MAPPER_SERVER")?;
    let operator = client(&server, std::env::var("MSG_HEADER_KEY")?)?;
    let admin = operator.admin()?;

    let mut sessions = Vec::new();
    for label in ["session-a", "session-b"] {
        // 1. One credential per session, expiring on its own if never revoked.
        let issued = admin
            .issue_key(Duration::from_secs(15 * 60), Some(label.into()))
            .await?;

        // 2. The device publishes its endpoint with that credential.
        let endpoint = spawn_endpoint(label).await?;
        let device = client(&server, issued.credential.clone())?;
        let registration = device
            .register(RegisterRequest {
                key: SERVICE.into(),
                local_addr: endpoint.to_string(),
                transport: Transport::Tcp,
                codec: true,
                force_namespace: false,
            })
            .await?;
        registration.wait_ready().await?;

        // 3. The controller reaches it with the same credential.
        let controller = client(&server, issued.credential)?;
        let local = TcpListener::bind("127.0.0.1:0").await?.local_addr()?;
        let connection = controller
            .connect(ConnectRequest {
                key: SERVICE.into(),
                local_addr: local.to_string(),
                transport: Transport::Tcp,
            })
            .await?;
        connection.wait_ready().await?;
        println!(
            "{label}: namespace {} sees services {:?}",
            issued.key_id,
            controller.list_keys().await?
        );
        sessions.push((label, issued.key_id, local, registration, connection));
    }

    // Each controller reaches its own device, never the other session's.
    for (label, _, local, _, connection) in &sessions {
        let mut socket = TcpStream::connect(local).await?;
        socket.write_all(b"whoami\n").await?;
        let mut reply = vec![0_u8; 64];
        let read = socket.read(&mut reply).await?;
        let path = connection.diagnostics().direct.map_or("relay", |path| {
            if path.direct_sessions > 0 {
                "direct"
            } else {
                "relay"
            }
        });
        println!(
            "{label} -> {} via {path}",
            String::from_utf8_lossy(&reply[..read]).trim()
        );
    }

    // The operator's view across every namespace.
    for service in admin.list_services_all(None).await? {
        println!(
            "operator: `{}` in namespace {}",
            service.service_name, service.namespace
        );
    }

    // 4. End the sessions.
    for (label, key_id, _, registration, connection) in sessions {
        admin.revoke_key(key_id).await?;
        connection.stop().await?;
        registration.stop().await?;
        println!("{label}: credential {key_id} revoked");
    }
    Ok(())
}

fn client(server: &str, credential: String) -> pb_mapper::Result<Client> {
    Client::new(ClientConfig {
        server: server.into(),
        credential,
        keep_alive: true,
        namespace: None,
    })
}

/// A stand-in for the harness's control port: answers every request with the
/// name of the session it belongs to.
async fn spawn_endpoint(name: &'static str) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0_u8; 64];
                if socket.read(&mut request).await.is_ok() {
                    let _ = socket.write_all(format!("{name}\n").as_bytes()).await;
                }
            });
        }
    });
    Ok(addr)
}
