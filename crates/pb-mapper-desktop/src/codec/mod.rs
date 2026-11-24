//! Neutral bitstream handling and encoder rules shared by every hardware
//! encoder.
pub(crate) mod annexb;
// The length-prefixed samples of VideoToolbox: macOS only, tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod avcc;
pub(crate) mod pipeline;
// Sending a dropped picture again: the macOS encoder's, tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod resubmit;
// The capture thread a push source's pictures come from: the Windows host's,
// tested everywhere.
pub(crate) mod present_waiter;
// When a still picture goes again: refinement bursts, then liveness. The
// Windows host's, tested everywhere.
pub(crate) mod refine;
pub(crate) mod sps;
// Which rate model a pace sweep fits: measurement support, which the macOS
// encoder's measurement uses and video_bench includes by path; tested
// everywhere.
#[cfg(test)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod rate_fit;
