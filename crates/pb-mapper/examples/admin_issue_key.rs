use std::time::Duration;

use pb_mapper::{Client, ClientConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("PB_MAPPER_SERVER")?;
    let credential = std::env::var("MSG_HEADER_KEY")?;
    let ttl_secs = std::env::args()
        .nth(1)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(3600_u64);

    let client = Client::new(ClientConfig {
        server,
        credential,
        keep_alive: false,
        namespace: None,
    })?;
    let issued = client
        .admin()?
        .issue_key(Duration::from_secs(ttl_secs), Some("sdk".into()))
        .await?;
    println!("key id: {}", issued.key_id);
    println!("credential: {}", issued.credential);
    Ok(())
}
