//! This `hub` crate is the
//! entry point of the Rust logic for pb-mapper UI.

mod actors;
mod signals;

use actors::create_actors;
use signals::log_collector::FlutterLogCollector;
use tracing_subscriber::Registry;
use tracing_subscriber::fmt::layer;
use tracing_subscriber::layer::SubscriberExt;

use rinf::{dart_shutdown, write_interface};
use tokio::spawn;
use tokio_with_wasm::alias as tokio;

write_interface!();

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Initialize tracing with our custom log collector
    let collector = FlutterLogCollector;
    let layer = layer().event_format(collector);
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    // Spawn pb-mapper actors
    spawn(create_actors());

    tracing::info!("pb-mapper UI backend started successfully");

    // Keep the main function running until Dart shutdown
    dart_shutdown().await;
}
