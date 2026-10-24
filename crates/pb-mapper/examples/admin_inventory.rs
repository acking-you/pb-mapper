//! Operate a relay: see every tenant's services and connections, nudge stuck
//! ones, set the legacy-protocol policy, and rotate the root key.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<administrator key> \
//!   cargo run -p pb-mapper --example admin_inventory
//! # also rotate the administrator key, persisting the new one first:
//!   cargo run -p pb-mapper --example admin_inventory -- --rotate-to ./admin.key
//! ```

use std::path::PathBuf;

use pb_mapper::{Client, ClientConfig, LegacyProtocol};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rotate_to = match std::env::args().nth(1).as_deref() {
        Some("--rotate-to") => Some(PathBuf::from(
            std::env::args().nth(2).ok_or("--rotate-to needs a path")?,
        )),
        Some(other) => return Err(format!("unknown argument `{other}`").into()),
        None => None,
    };
    let client = Client::new(ClientConfig {
        server: std::env::var("PB_MAPPER_SERVER")?,
        credential: std::env::var("MSG_HEADER_KEY")?,
        keep_alive: false,
        namespace: None,
    })?;
    let admin = client.admin()?;

    // Every namespace at once; pass `Some(key_id)` to scope to one credential.
    let services = admin.list_services_all(None).await?;
    for service in &services {
        println!(
            "service `{}` in namespace {} ({}, codec={}, {} connection(s))",
            service.service_name,
            service.namespace,
            service.transport,
            service.codec_enabled,
            service.connection_count
        );
    }
    for conn in admin.list_connections_all(None).await? {
        println!(
            "  `{}` conn {} healthy={} protocol v{} last heard {} ms ago",
            conn.service_name,
            conn.conn_id,
            conn.healthy,
            conn.protocol_version,
            conn.last_rx_age_ms
        );
    }

    // Retire every unhealthy control connection the relay still holds. A
    // healthy publisher simply reconnects, so this is a nudge, not a shutdown.
    for conn in admin.list_connections_all(None).await? {
        if !conn.healthy {
            let retired = admin
                .retire_connections(Some(conn.key_id), conn.service_name, Some(conn.conn_id))
                .await?;
            println!("retired {retired} stuck connection(s)");
        }
    }

    // Refuse pre-v2 framing once every client has upgraded.
    if admin.auth_status().await?.active_legacy_connections == 0 {
        admin.set_legacy_protocol(LegacyProtocol::Deny).await?;
        println!("legacy protocol denied for new connections");
    }

    // Rotate the root key. The candidate is written next to `path` before the
    // request goes out, so a lost response can never lose the key in force.
    if let Some(path) = rotate_to {
        admin.rotate_root_key_to_file(&path, None).await?;
        println!(
            "administrator key rotated; the new key is in {}",
            path.display()
        );
    }
    Ok(())
}
