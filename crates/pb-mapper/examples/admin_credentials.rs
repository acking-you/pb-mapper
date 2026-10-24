//! The temporary-credential lifecycle, end to end, with the administrator key.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<administrator key> \
//!   cargo run -p pb-mapper --example admin_credentials -- 3600
//! ```
//!
//! The administrator key never leaves the operator. What is handed out is a
//! `pbmt1_` credential: derived from the root key, scoped to its own
//! namespace, renewable, and revocable on the spot. The relay keeps lifecycle
//! metadata only, so `reveal_key` re-derives a secret rather than reading a
//! stored copy.

use std::time::Duration;

use pb_mapper::{Client, ClientConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ttl = Duration::from_secs(
        std::env::args()
            .nth(1)
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(3600),
    );
    let client = Client::new(ClientConfig {
        server: std::env::var("PB_MAPPER_SERVER")?,
        credential: std::env::var("MSG_HEADER_KEY")?,
        keep_alive: false,
        namespace: None,
    })?;
    // Fails locally with `Error::NotAdministrator` for a temporary credential.
    let admin = client.admin()?;

    // Issue: one credential per device, tenant, or agent session.
    let issued = admin.issue_key(ttl, Some("sdk-example".into())).await?;
    println!(
        "issued key {} ({}), expires at {}",
        issued.key_id, issued.state, issued.expires_at
    );
    println!("hand this to the workload: {}", issued.credential);

    // Inspect without the secret, or with it.
    let shown = admin.show_key(issued.key_id).await?;
    println!("show: label={:?} state={}", shown.label, shown.state);
    let revealed = admin.reveal_key(issued.key_id).await?;
    assert_eq!(revealed.credential, issued.credential);

    // Renew: extend the lifetime to `ttl` from now. The secret is unchanged.
    let renewed = admin.renew_key(issued.key_id, ttl * 2).await?;
    println!("renewed until {}", renewed.expires_at);

    // List every credential, paging on our behalf.
    for key in admin.list_keys_all().await? {
        println!("  key {} {:?} {}", key.key_id, key.label, key.state);
    }

    // Revoke: live relay connections under this credential close at once and
    // new direct setups are refused.
    let revoked = admin.revoke_key(issued.key_id).await?;
    println!("revoked key {}: {}", revoked.key_id, revoked.state);

    // Housekeeping: drop expired and revoked entries.
    println!("garbage-collected {} credential(s)", admin.gc_keys().await?);

    let status = admin.auth_status().await?;
    println!(
        "auth: {} active / {} capacity, {} successes, {} failures, legacy {:?}",
        status.active_keys,
        status.capacity,
        status.auth_successes,
        status.auth_failures,
        status.legacy_protocol
    );
    Ok(())
}
