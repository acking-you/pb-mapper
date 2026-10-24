//! Reach a registered TCP service on a local port.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --example connect_tcp -- 127.0.0.1:9090 web
//! ```
//!
//! Every connection accepted on the local port first tries a direct QUIC path
//! to the publisher and falls back to the relay before any byte is sent if it
//! cannot be set up. This prints how many took each path whenever that changes.

use std::time::Duration;

use pb_mapper::{Client, ClientConfig, ConnectRequest, Transport};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("PB_MAPPER_SERVER")?;
    let credential = std::env::var("MSG_HEADER_KEY")?;
    let local_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:9090".to_string());
    let key = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "echo".to_string());

    let client = Client::new(ClientConfig {
        server,
        credential,
        keep_alive: true,
        namespace: None,
    })?;
    let connection = client
        .connect(ConnectRequest {
            key,
            local_addr: local_addr.clone(),
            transport: Transport::Tcp,
        })
        .await?;
    connection.wait_ready().await?;
    println!("`{}` is reachable at {local_addr}", connection.key());

    let mut last = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => { result?; break; }
            _ = tick.tick() => {
                let Some(path) = connection.diagnostics().direct else { continue };
                let split = (path.direct_sessions, path.relay_sessions, path.fallbacks);
                if last != Some(split) {
                    println!(
                        "connections so far: {} direct, {} relay ({} fell back); open now: {} direct, {} relay",
                        split.0, split.1, split.2, path.active_direct, path.active_relay
                    );
                    last = Some(split);
                }
            }
        }
    }
    connection.stop().await?;
    Ok(())
}
