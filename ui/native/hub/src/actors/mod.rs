mod pb_mapper_actor;

use crate::signals::CreateActors;
#[cfg(any(target_os = "android", target_os = "ios"))]
use crate::signals::SetAppDirectoryPath;
use messages::prelude::Context;
use rinf::DartSignal;
use tokio::spawn;
use tokio_with_wasm::alias as tokio;

pub use pb_mapper_actor::*;

#[cfg(any(target_os = "android", target_os = "ios"))]
async fn get_app_dir() -> Option<String> {
    if let Some(v) = SetAppDirectoryPath::get_dart_signal_receiver().recv().await {
        Some(v.message.path)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn get_app_dir() -> Option<String> {
    None
}

/// Spawns the actors.
pub async fn create_actors() {
    // Wait for app directory path from Flutter first (for mobile platforms)
    let app_directory_path = get_app_dir().await;

    // Wait until the start signal arrives.
    let start_receiver = CreateActors::get_dart_signal_receiver();
    start_receiver.recv().await;

    // Create actor contexts.
    let pb_mapper_context = Context::new();
    let pb_mapper_addr = pb_mapper_context.address();

    // Spawn the actors with the app directory path.
    let pb_mapper_actor = PbMapperActor::new(pb_mapper_addr, app_directory_path);
    spawn(pb_mapper_context.run(pb_mapper_actor));
}
