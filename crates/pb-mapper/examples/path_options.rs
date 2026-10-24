//! Every knob between "direct first" and "relay only", on both ends.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --example path_options -- 127.0.0.1:8080
//! ```
//!
//! The defaults — `register` allows direct, `connect` uses `PathPolicy::Auto`
//! — suit almost everything. The options exist for the cases that do not:
//!
//! * a publisher that must never be reached except through the relay, for
//!   audit or egress policy: `RegisterOptions { allow_direct: false, .. }`;
//! * a consumer on a network where UDP is known to be blocked, or one that
//!   wants the relay's exact pre-direct protocol: `PathPolicy::RelayOnly`;
//! * a publisher whose clients disappear without closing: an idle deadline.
//!
//! A direct path is chosen per connection and only when both ends allow it, so
//! either side opting out is enough to keep that tunnel on the relay.

use std::time::Duration;

use pb_mapper::{
    Client, ClientConfig, ConnectOptions, ConnectRequest, PathPolicy, RegisterOptions,
    RegisterRequest, Transport,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let local_service = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let client = Client::new(ClientConfig {
        server: std::env::var("PB_MAPPER_SERVER")?,
        credential: std::env::var("MSG_HEADER_KEY")?,
        keep_alive: true,
        namespace: None,
    })?;
    let request = |key: &str| RegisterRequest {
        key: key.into(),
        local_addr: local_service.clone(),
        transport: Transport::Tcp,
        codec: true,
        force_namespace: false,
    };

    // Publisher: relay only, and reclaim data connections idle for 90 s.
    // Traffic in either direction resets the deadline; TCP only.
    let audited = client
        .register_with_options(
            request("audited"),
            RegisterOptions {
                allow_direct: false,
                idle_timeout: Some(Duration::from_secs(90)),
            },
        )
        .await?;

    // Publisher: the defaults plus an idle deadline.
    let default_with_idle = client
        .register_with_idle_timeout(request("default-idle"), Duration::from_secs(90))
        .await?;

    // Publisher: spelled-out defaults, identical to `client.register(..)`.
    let explicit_default = client
        .register_with_options(request("default"), RegisterOptions::default())
        .await?;
    for registration in [&audited, &default_with_idle, &explicit_default] {
        registration.wait_ready().await?;
        println!("registered `{}`", registration.key());
    }

    // Consumer: relay only for this tunnel, whatever the publisher allows.
    let relayed = client
        .connect_with_options(
            ConnectRequest {
                key: "default".into(),
                local_addr: "127.0.0.1:9091".into(),
                transport: Transport::Tcp,
            },
            ConnectOptions {
                path: PathPolicy::RelayOnly,
            },
        )
        .await?;
    // Consumer: direct first, falling back to the relay. This is the default.
    let auto = client
        .connect_with_options(
            ConnectRequest {
                key: "default".into(),
                local_addr: "127.0.0.1:9092".into(),
                transport: Transport::Tcp,
            },
            ConnectOptions {
                path: PathPolicy::Auto,
            },
        )
        .await?;
    relayed.wait_ready().await?;
    auto.wait_ready().await?;
    println!("relay only at 127.0.0.1:9091, direct first at 127.0.0.1:9092");

    tokio::signal::ctrl_c().await?;
    for connection in [relayed, auto] {
        connection.stop().await?;
    }
    for registration in [audited, default_with_idle, explicit_default] {
        registration.stop().await?;
    }
    Ok(())
}
