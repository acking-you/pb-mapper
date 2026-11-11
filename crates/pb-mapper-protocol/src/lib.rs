//! Define message protocols and tools for reading and writing
//! messages
//!
//! The reader and writer traits are `async fn` in a public trait, which cannot
//! state its auto-trait bounds. That is deliberate: these are only ever awaited
//! on the connection task that owns the stream, never sent across one.

pub mod buffer;
pub mod command;
pub mod data;
pub mod direct;
pub mod forward;
mod frame_read;
mod framing;
#[cfg(feature = "relay-quic")]
pub mod quic;
pub mod secure;
pub mod transport;

pub use framing::{
    CodecMessageReader, CodecMessageWriter, MessageReader, MessageWriter, NormalMessageReader,
    NormalMessageWriter, get_decodec, get_default_decodec, get_default_encodec, get_encodec,
    get_header_msg_reader, get_header_msg_writer,
};

// Defined in `pb-mapper-core` so that the checksum and error types can name it
// without depending on this module. Re-exported rather than redeclared: a second
// `pub type` would be a distinct name for the same width, and the two would read
// as unrelated at the crate boundary.
pub use pb_mapper_core::DataLenType;

/// Maximum plaintext payload size read from local services.
const MAX_PLAINTEXT_LEN: DataLenType = 8 * 1024 * 1024;
/// AES-GCM tag length (bytes).
const CODEC_TAG_LEN: DataLenType = 16;
const _: () = assert!(
    CODEC_TAG_LEN as usize == ring::aead::MAX_TAG_LEN,
    "every encrypted frame carries one ring AES-GCM tag"
);
/// Maximum value of `datalen` to prevent Out of Memory.
/// For encrypted frames, the tag is appended to the payload.
const MAX_MSG_LEN: DataLenType = MAX_PLAINTEXT_LEN + CODEC_TAG_LEN;
