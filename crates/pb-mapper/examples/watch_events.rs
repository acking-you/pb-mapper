//! Listen to a tunnel the way an app UI or an agent harness would.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --example watch_events -- web 127.0.0.1:9090
//! ```
//!
//! Two feeds, both free of credentials and payload:
//!
//! * `subscribe()` is a `tokio::sync::watch` receiver of [`TunnelStatus`]:
//!   `Starting`, `Connected`, `Retrying`, `Stopped`, `Failed(reason)`. It is
//!   what to drive a connection indicator from.
//! * `subscribe_diagnostics()` replays recent history, then delivers every
//!   transition through a bounded queue. Lag is explicit; `diagnostics()`
//!   reconciles current state. Events and snapshots serialize directly to JSON.
//!
//! Add `--port-mapping` (and build with the `port-mapping` feature) to observe
//! gateway discovery, lease requests, renewal, expiry and cleanup as well.
//!
//! When the host reports a network change or a resume from sleep, call
//! `client.notify_network_change()`: waiting retries start at once and the
//! next connection tries direct again.

use pb_mapper::{Client, ClientConfig, ConnectRequest, Transport};
use tokio::sync::broadcast::error::RecvError;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "echo".to_string());
    let local_addr = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "127.0.0.1:9090".to_string());
    let client = Client::new(ClientConfig {
        server: std::env::var("PB_MAPPER_SERVER")?,
        credential: std::env::var("MSG_HEADER_KEY")?,
        keep_alive: true,
        namespace: None,
    })?
    .with_port_mapping(std::env::args().any(|arg| arg == "--port-mapping"))?;
    let connection = client
        .connect(ConnectRequest {
            key,
            local_addr,
            transport: Transport::Tcp,
        })
        .await?;

    let mut status = connection.subscribe();
    let mut events = connection.subscribe_diagnostics();
    let mut status_open = true;
    let mut stopping = false;
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c(), if !stopping => {
                result?;
                stopping = true;
                connection.stop().await?;
            }
            changed = status.changed(), if status_open => {
                status_open = changed.is_ok();
                println!("status: {:?}", *status.borrow_and_update());
            }
            event = events.recv() => {
                match event {
                    Ok(event) => println!("{}", serde_json::to_string(&event)?),
                    Err(RecvError::Lagged(count)) => {
                        eprintln!("diagnostics skipped {count} events; reconciling snapshot");
                        println!("{}", serde_json::to_string(&connection.diagnostics())?);
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }

    // The whole snapshot, as an app would ship it.
    println!(
        "{}",
        serde_json::to_string_pretty(&connection.diagnostics())?
    );
    connection.stop().await?;
    Ok(())
}
