//! This `hub` crate is the
//! entry point of the Rust logic.

mod actors;
mod signals;
// mod testing;

use actors::create_actors;
use signals::log_collector::FlutterLogCollector;
use tracing_subscriber::Registry;
use tracing_subscriber::fmt::layer;
use tracing_subscriber::layer::SubscriberExt;

use rinf::{dart_shutdown, write_interface};
// use testing::run_unit_tests;
use tokio::spawn;
use tokio_with_wasm::alias as tokio;

write_interface!();

// You can go with any async library, not just `tokio`.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Initialize tracing with our custom log collector
    let collector = FlutterLogCollector;
    let layer = layer().event_format(collector);
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    // Spawn concurrent tasks.
    // Always use non-blocking async functions like `tokio::fs::File::open`.
    // If you must use blocking code, use `tokio::task::spawn_blocking`
    // or the equivalent provided by your async library.
    spawn(create_actors());

    // Add some test logs to verify the logging system after actors are created
    for _ in 0..1000 {
        tracing::info!("Actors created successfully");
        tracing::debug!("Debug log after actor creation");
        tracing::warn!("Warning log after actor creation");
        tracing::error!("Error log after actor creation");
        tracing::trace!("Trace log after actor creation");
    }

    // Keep the main function running until Dart shutdown.
    dart_shutdown().await;
}
