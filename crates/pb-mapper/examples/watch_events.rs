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
//! * `diagnostics()` is a snapshot whose `events` list says what recovery did:
//!   relay up or down and why, network changes, direct sessions established,
//!   fallbacks and the stage they left at. Each event carries a `seq`, so a
//!   poller prints only what it has not seen. The snapshot serializes to JSON
//!   as-is, ready to forward to a UI or a log pipeline.
//!
//! When the host reports a network change or a resume from sleep, call
//! `client.notify_network_change()`: waiting retries start at once and the
//! next connection tries direct again.

use std::time::Duration;

use pb_mapper::{Client, ClientConfig, ConnectRequest, Transport, TunnelEventKind, TunnelStatus};

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
    })?;
    let connection = client
        .connect(ConnectRequest {
            key,
            local_addr,
            transport: Transport::Tcp,
        })
        .await?;

    let mut status = connection.subscribe();
    let mut seen = 0_u64;
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => { result?; break; }
            changed = status.changed() => {
                if changed.is_err() {
                    break; // the worker is gone; its last status is final
                }
                let current = status.borrow_and_update().clone();
                println!("status: {current:?}");
                if matches!(current, TunnelStatus::Stopped | TunnelStatus::Failed(_)) {
                    break;
                }
            }
            _ = poll.tick() => {
                let snapshot = connection.diagnostics();
                let unseen = seen;
                for event in snapshot.events.iter().filter(|event| event.seq > unseen) {
                    seen = event.seq;
                    match event.kind {
                        TunnelEventKind::DirectEstablished { setup_ms } => {
                            println!("direct path up in {setup_ms} ms");
                        }
                        TunnelEventKind::DirectFallback { stage } => {
                            println!("direct gave way to the relay at `{}`", stage.as_str());
                        }
                        TunnelEventKind::RelayReady { latency_ms } => {
                            println!("relay answered in {latency_ms} ms");
                        }
                        TunnelEventKind::RelayFailed { failure, retry_in_ms } => {
                            println!("relay {}; retrying in {retry_in_ms} ms", failure.as_str());
                        }
                        TunnelEventKind::NetworkChanged => println!("network changed"),
                    }
                }
                if let Some(attempt) = snapshot.direct.as_ref().and_then(|path| path.attempt) {
                    println!(
                        "direct attempt: {} for {} ms",
                        attempt.stage.as_str(),
                        attempt.elapsed_ms
                    );
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
