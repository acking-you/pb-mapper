mod pb_mapper_actor;

use crate::signals::SetAppDirectoryPath;
use messages::prelude::Context;
use rinf::DartSignal;
use tokio::spawn;
use tokio::time::{timeout, Duration};
use tokio_with_wasm::alias as tokio;

pub use pb_mapper_actor::*;

async fn get_app_dir() -> Option<String> {
    // Add timeout to prevent hanging if Flutter doesn't send the signal
    match timeout(Duration::from_secs(10), SetAppDirectoryPath::get_dart_signal_receiver().recv()).await {
        Ok(Some(signal_pack)) => {
            let path = signal_pack.message.path;
            if path.is_empty() {
                tracing::info!("Received empty app directory path from Flutter, using default config directory");
                None
            } else {
                tracing::info!("Received app directory path from Flutter: {}", path);
                Some(path)
            }
        }
        Ok(None) => {
            tracing::warn!("Flutter signal stream closed, using default config directory");
            None
        }
        Err(_) => {
            tracing::warn!("Timeout waiting for app directory path from Flutter, using default config directory");
            None
        }
    }
}

/// Spawns the actors.
pub async fn create_actors() {
    // Wait for app directory path from Flutter and start actors directly
    let app_directory_path = get_app_dir().await;

    // Create actor contexts.
    let pb_mapper_context = Context::new();
    let pb_mapper_addr = pb_mapper_context.address();

    // Spawn the actors with the app directory path.
    let pb_mapper_actor = PbMapperActor::new(pb_mapper_addr, app_directory_path);
    spawn(pb_mapper_context.run(pb_mapper_actor));
}
