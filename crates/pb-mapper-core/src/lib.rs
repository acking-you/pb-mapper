//! The bottom layer: credential primitives, framing checksums, configuration,
//! address resolution, and the file primitives the durable stores build on.
//!
//! Nothing here depends on another `pb-mapper` crate, which is what makes it
//! the bottom. `DataLenType` lives here rather than with the message framing
//! that names it, so that `checksum` and `error` can use it without depending
//! on the protocol layer.

pub mod addr;
pub mod checksum;
pub mod codec;
pub mod config;
pub mod conn_id;
pub mod durable_file;
pub mod error;
pub mod paging;
pub mod test_support;
pub mod timeout;

/// The width of the length prefix on a framed message.
pub type DataLenType = u32;
