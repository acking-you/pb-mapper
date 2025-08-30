mod pb_mapper_actor;

use crate::signals::CreateActors;
use messages::prelude::Context;
use rinf::DartSignal;
use tokio::spawn;
use tokio_with_wasm::alias as tokio;

pub use pb_mapper_actor::*;

/// Spawns the actors.
pub async fn create_actors() {
    // Wait until the start signal arrives.
    let start_receiver = CreateActors::get_dart_signal_receiver();
    start_receiver.recv().await;

    // Create actor contexts.
    let pb_mapper_context = Context::new();
    let pb_mapper_addr = pb_mapper_context.address();

    // Spawn the actors.
    let pb_mapper_actor = PbMapperActor::new(pb_mapper_addr);
    spawn(pb_mapper_context.run(pb_mapper_actor));
}
