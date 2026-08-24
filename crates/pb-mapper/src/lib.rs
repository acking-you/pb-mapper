//! Client SDK for a deployed pb-mapper relay.
//!
//! ```ignore
//! use std::time::Duration;
//!
//! use pb_mapper::{Client, ClientConfig, ConnectRequest, RegisterRequest, Transport};
//!
//! # async fn example() -> pb_mapper::Result<()> {
//! let client = Client::new(ClientConfig {
//!     server: "relay.example.com:7666".into(),
//!     credential: std::env::var("MSG_HEADER_KEY").expect("MSG_HEADER_KEY"),
//!     keep_alive: true,
//!     namespace: None,
//! })?;
//!
//! let registration = client
//!     .register(RegisterRequest {
//!         key: "echo".into(),
//!         local_addr: "127.0.0.1:8080".into(),
//!         transport: Transport::Tcp,
//!         codec: false,
//!         force_namespace: false,
//!     })
//!     .await?;
//! registration.wait_ready().await?;
//!
//! let _keys = client.list_keys().await?;
//! let issued = client
//!     .admin()?
//!     .issue_key(Duration::from_secs(3600), Some("agent".into()))
//!     .await?;
//! client.admin()?.revoke_key(issued.key_id).await?;
//!
//! registration.stop().await?;
//! # Ok(())
//! # }
//! ```
//!
//! The CLI binary still lives in `pb-mapper-cli`. That crate is `publish =
//! false`, so build it from a checkout — `make build-pb-mapper`, or `cargo
//! build --release --bin pb-mapper` — rather than installing it from a registry.

pub use pb_mapper_client::sdk::*;
