//! Reach a registered TCP service on a local port.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --example connect_tcp -- 127.0.0.1:9090 web
//! ```
//!
//! Every connection accepted on the local port first tries a direct QUIC path
//! to the publisher and falls back to the relay before any byte is sent if it
//! cannot be set up. This follows the tunnel's metrics and prints how many
//! took each path, and the bytes each carried, whenever that changes.

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

    // A sample a second, and one as each connection opens or closes.
    let mut metrics = connection.subscribe_metrics(Duration::from_secs(1));
    let mut last = None;
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => { result?; break; }
            sample = metrics.recv() => {
                let Some(sample) = sample else { break };
                let (sessions, traffic) = (sample.sessions, sample.traffic);
                if last != Some((sessions, traffic)) {
                    println!(
                        "connections so far: {} direct, {} relay; open now: {} direct, {} relay; \
                         bytes direct {}/{}, relay {}/{} (sent/received)",
                        sessions.direct,
                        sessions.relay,
                        sessions.active_direct,
                        sessions.active_relay,
                        traffic.direct.sent,
                        traffic.direct.received,
                        traffic.relay.sent,
                        traffic.relay.received,
                    );
                    last = Some((sessions, traffic));
                }
            }
        }
    }
    connection.stop().await?;
    Ok(())
}
