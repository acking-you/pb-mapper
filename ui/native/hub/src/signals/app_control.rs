use rinf::DartSignal;
use serde::Deserialize;

#[derive(Deserialize, DartSignal)]
pub struct CreateActors;

// Internal signal for periodic status updates (not exposed to Dart)
#[derive(Clone)]
pub struct InternalStatusUpdate;
