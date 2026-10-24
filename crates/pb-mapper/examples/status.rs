//! Inspect what the relay knows, with any credential.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --example status
//! ```
//!
//! Every query is scoped to the credential's namespace: a temporary credential
//! sees only its own services and sessions, never another tenant's.

use pb_mapper::{Client, ClientConfig, SessionPath};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(ClientConfig {
        server: std::env::var("PB_MAPPER_SERVER")?,
        credential: std::env::var("MSG_HEADER_KEY")?,
        keep_alive: false,
        namespace: None,
    })?;
    println!("SDK {}", pb_mapper::SDK_VERSION);

    // Registered service keys.
    let keys = client.list_keys().await?;
    println!("{} service(s): {keys:?}", keys.len());

    // The control connections behind each one, and whether they are healthy.
    for key in &keys {
        for conn in client.service_status(key.as_str()).await? {
            println!(
                "  {key}: conn {} generation {} healthy={} last heard {} ms ago",
                conn.conn_id, conn.generation, conn.healthy, conn.last_rx_age_ms
            );
        }
    }

    // Sessions as the endpoints report them. Reports are observations with an
    // age and a `stale` flag, not proof of liveness, and never authorize.
    for session in client.sessions().await? {
        let path = session
            .consumer
            .as_ref()
            .or(session.provider.as_ref())
            .map(|observed| observed.report.path);
        println!(
            "  session to `{}`: {} (stale={}, restored={})",
            session.service,
            match path {
                Some(SessionPath::Direct) => "direct",
                Some(SessionPath::Relay) => "relay",
                None => "unreported",
            },
            session.stale,
            session.restored
        );
    }

    // The relay's raw mapping table.
    let remote = client.remote_id().await?;
    println!("active: {}\nidle: {}", remote.active, remote.idle);
    Ok(())
}
