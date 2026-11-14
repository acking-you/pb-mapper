//! Neutral bitstream handling and encoder rules shared by every hardware
//! encoder.
pub(crate) mod annexb;
// The length-prefixed samples of VideoToolbox: macOS only, tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod avcc;
pub(crate) mod pipeline;
