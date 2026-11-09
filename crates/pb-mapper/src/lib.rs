//! Client SDK for a deployed pb-mapper relay.
//!
//! A [`Client`] holds a relay address and a credential. `register` publishes a
//! local TCP or UDP service under a key, and `connect` makes that service
//! reachable on a local port. Every connection tries a direct QUIC path first
//! and falls back to the relay before any byte is sent; reconnection and
//! recovery are handled inside the returned handles.
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
//! let connection = client
//!     .connect(ConnectRequest {
//!         key: "echo".into(),
//!         local_addr: "127.0.0.1:9090".into(),
//!         transport: Transport::Tcp,
//!     })
//!     .await?;
//! connection.wait_ready().await?;
//!
//! // Status changes as they happen, and what recovery did.
//! let mut status = connection.subscribe();
//! status.changed().await.ok();
//! println!("{:?} {:?}", *status.borrow(), connection.diagnostics().events);
//!
//! // With the administrator key: a scoped, expiring credential for a workload.
//! let issued = client
//!     .admin()?
//!     .issue_key(Duration::from_secs(3600), Some("agent".into()))
//!     .await?;
//! client.admin()?.revoke_key(issued.key_id).await?;
//!
//! connection.stop().await?;
//! registration.stop().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Runnable examples for every capability are in `examples/`; start with
//! `cargo run -p pb-mapper --example quickstart`, which needs no relay of your
//! own. The CLI and relay binary are the `pb-mapper-cli` package
//! (`cargo install pb-mapper-cli`).
//!
//! # Remote desktop
//!
//! The `desktop` feature adds the `desktop` module: sharing this computer's
//! display over the same [`Client`] (`desktop::Host`, Windows today), and
//! viewing and controlling one from anywhere (`desktop::Viewer`), each by a
//! `pbd1.` invitation. A viewer hands out encoded H.264 or H.265 access
//! units, or JPEG pictures, for a decoder the integrator brings. Around them
//! are status streams (`Host::watch`, `Viewer::watch`), a reconnecting
//! `desktop::supervisor::Supervisor` with the app's policy, input helpers
//! (`DesktopView::display_point`, `Input::text_chunks`) and a sealed store
//! for a host's identity (`desktop::trust`). Both ends take the same path
//! defaults as every tunnel: LAN first, then direct, then the relay.
//! `cargo run -p pb-mapper --features desktop --example desktop_loopback`
//! runs a host and a viewer in one process on any OS.
#![cfg_attr(docsrs, feature(doc_cfg))]

pub use pb_mapper_client::sdk::*;

/// Optional endpoint-owned remote desktop. No capture or input work runs until
/// a host is explicitly started and an authorized viewer connects.
#[cfg(feature = "desktop")]
#[cfg_attr(docsrs, doc(cfg(feature = "desktop")))]
pub use pb_mapper_desktop as desktop;
