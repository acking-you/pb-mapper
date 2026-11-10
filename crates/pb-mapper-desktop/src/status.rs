//! What the embedding app sees of its host: its state and its viewer.
use crate::{DesktopKind, Display, ModeList, QualityPreset, StateReason};
use serde::{Deserialize, Serialize};

/// The host's own view of its session, polled by the embedding app.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostStatus {
    pub state: HostState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StateReason>,
    pub message: String,
    pub control_allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<HostViewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Display>,
    pub desktop: DesktopKind,
    pub capabilities: Vec<String>,
    /// The display's modes while a viewer has it changed: what the host's
    /// owner may put back with `Host::restore_display_mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_mode: Option<ModeList>,
}
/// The connected viewer, as the host sees it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostViewer {
    /// Time since the session was accepted, on the host's monotonic clock.
    pub since_ms: u64,
    /// When the session was accepted, for display only.
    pub started_unix_ms: u64,
    pub path: HostPath,
    pub controlling: bool,
    pub paused: bool,
    pub quality: QualityPreset,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostState {
    Waiting,
    Connecting,
    Streaming,
    Paused,
    CaptureUnavailable,
    Stopped,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPath {
    Direct,
    Relay,
    Unknown,
}
