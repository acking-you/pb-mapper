//! The viewer's video sink lives in `viewer/sink.rs`. This keeps the crate
//! root's `pub use video_sink::{..}` compiling until it re-exports these
//! from `viewer` itself.
pub use crate::viewer::{AttachError, Pull, VideoConsumer, VideoSink, VideoUnit};
