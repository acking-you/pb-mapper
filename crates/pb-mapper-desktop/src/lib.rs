//! Endpoint-owned desktop sharing, carried by an existing pb-mapper deployment.
//! Hosts explicitly grant view access through a secret invitation. Control is
//! separately enabled at the host and requested by the viewer. TLS 1.3 pins the
//! host certificate from that invitation before sending its bearer capability.
//! The native Windows backend captures the ordinary desktop. An explicitly
//! installed, authorized helper handles UAC/Winlogon on its bound desktop.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod auth;
mod backend;
// Neutral: only the Windows encoder and injector use them until another
// platform lands.
#[cfg_attr(not(windows), allow(dead_code))]
mod codec;
mod credit;
mod host;
mod idr;
mod input;
mod invite;
mod lane;
mod launch;
mod mapping;
mod modes;
mod native;
// Only the Windows helper speaks it until another platform has an agent.
#[cfg_attr(not(windows), allow(dead_code))]
mod oop;
mod outgoing;
mod pacer;
mod platform;
pub mod privileged;
mod protocol;
mod qos;
mod scheduler;
mod security;
mod session;
mod shared;
#[cfg(test)]
mod slow_link;
mod status;
pub mod supervisor;
#[cfg(test)]
mod tests;
mod timer;
pub mod trust;
mod video;
mod video_sink;
mod viewer;
mod watch;
mod wire;
mod worker;

use serde::{Deserialize, Serialize};
use std::io;

pub use backend::{
    Backend, BackendCaps, BackendError, CursorMode, DRAIN_POLL, Description, FailureKind, HostOs,
    Waker, failure_kind,
};
pub use host::{Access, Custom, DisplaySelection, Host, HostBuilder, HostOptions, Native};
pub use input::{Button, DoubleClickGuard, Input, InputEvent, Key, cursor::CursorPosition};
pub(crate) use invite::valid_service;
pub use invite::{Invitation, InvitationError, InvitationFormat};
pub use modes::{DisplayModeError, Mode, ModeList, ModeTarget};
pub use native::{NativeBackend, native_backend};
pub use platform::{HostSupport, UnsupportedReason, displays, host_support, recover_display_modes};
pub use protocol::{
    Adaptation, DesktopKind, NoticeCode, RateModel, RejectReason, Route, SessionState, StateReason,
    StopReason, VideoSize,
};
pub use security::HostIdentity;
pub use status::{HostPath, HostState, HostStatus, HostViewer};
pub use video::{
    EncodedFrame, KeyframeReason, QualityPreset, Resolution, VideoCodec, VideoFormat, VideoLimits,
    VideoRate,
};
pub use video_sink::{AttachError, Pull, VideoConsumer, VideoSink, VideoUnit};
pub use viewer::{
    ConnectError, ConnectStage, DecoderInfo, DecoderState, Decoders, DesktopView, InputError,
    IntervalStats, NoticeInfo, PresentedInfo, VideoInfo, Viewer, ViewerConfig, ViewerLink,
    ViewerReason, ViewerState, ViewerStats, ViewerStatus,
};

/// What only a Windows build has.
#[cfg(windows)]
#[cfg_attr(docsrs, doc(cfg(windows)))]
pub mod windows {
    /// The fused encoder itself, for benches.
    #[doc(hidden)]
    pub use crate::platform::windows::video;
    pub use crate::platform::windows::{dpapi, mf::media_foundation};
}

/// Embedding applications coordinate their other input writers here. Merely
/// viewing a desktop never acquires this lease. Drop releases write admission
/// on end-control, revocation, disconnect, or local stop.
pub trait ControlAdmission: Send + Sync + 'static {
    fn acquire(&self) -> Result<Box<dyn ControlLease>>;
}
pub trait ControlLease: 'static {}
impl<T: 'static> ControlLease for T {}

pub type Result<T> = io::Result<T>;
pub(crate) fn error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Display {
    pub id: u32,
    pub name: String,
    /// Physical desktop pixels; coordinates may be negative on secondary displays.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The capture backend's desktop identity, independent of display resolution.
/// Every switch into/out of a secure desktop must advance `generation`, even
/// when the same monitor and desktop name are reused.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopContext {
    pub display: Display,
    pub generation: u64,
    pub desktop: DesktopKind,
}
