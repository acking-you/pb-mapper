use rinf::DartSignal;
use serde::Deserialize;

#[derive(Deserialize, DartSignal)]
pub struct CreateActors;

#[allow(dead_code)]
#[derive(Deserialize, DartSignal)]
pub struct SetAppDirectoryPath {
    pub path: String,
}

// Internal signal for periodic status updates (not exposed to Dart)
#[allow(dead_code)]
#[derive(Clone)]
pub struct InternalStatusUpdate;
