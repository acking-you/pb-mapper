//! The out-of-process backend's protocol: one request, one response and
//! one byte record (a unit or a still), in order, per call of the
//! `Backend` it carries. A change to any of it bumps the helper protocol.
use crate::{
    BackendCaps, CursorKind, CursorMode, CursorShape, DesktopContext, FailureKind, HostOs, Input,
    modes::{Mode, ModeList},
};
use serde::{Deserialize, Serialize};

/// The longest one call may take, and a worker may wait to start.
pub(crate) const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// The longest a call that may switch a display mode takes: a switch blocks
/// while the display retrains its link, a reset may be two switches, and
/// the journal is synced first; `Start` replays a journal a killed worker
/// left. A call that outlives it ends the session, and the worker's
/// backend, dropped with it, puts the mode back or leaves it journalled.
pub(crate) const MODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// The longest a drain may wait for encoder output.
pub(crate) const DRAIN_WAIT_MAX_MS: u32 = 4;

/// The service attests this login instance before starting a privileged worker.
/// Session IDs alone are reusable after logoff; lock/unlock keeps this identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionIdentity {
    pub session: u32,
    pub logon_time: i64,
    pub user: String,
    pub domain: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum Request {
    /// `display_device`, the display's name, finds it again when its id
    /// went stale.
    Start {
        display_id: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_device: Option<String>,
    },
    Attach {
        display_id: u32,
        session: SessionIdentity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_device: Option<String>,
    },
    // Struct variants throughout: serde ignores extra fields on an internally
    // tagged unit variant, which would undo `deny_unknown_fields`.
    Context {},
    Capture {},
    ConfigureVideo {
        limits: crate::VideoLimits,
        nominal_fps: u32,
    },
    /// Drain finished output, then submit one capture when `submit` is set.
    /// A drain alone may wait up to `wait_ms` (at most 4) for encoder output
    /// while units are pending. The helper never paces.
    Video {
        expected: DesktopContext,
        codec: crate::VideoCodec,
        submit: bool,
        keyframe: bool,
        rate: crate::VideoRate,
        wait_ms: u32,
    },
    Presented {
        generation: u64,
    },
    /// Includes `Input::Lock`. A `drag_start` press is held by the host's
    /// thread C before it gets here; the helper ignores the flag. `expected`
    /// is the desktop the host admitted it on: the worker injects only while
    /// the bound desktop is still that one, display included. The answer
    /// carries the pointer as the input left it, when the worker could read
    /// it.
    Input {
        expected: DesktopContext,
        sequence: u64,
        input: Input,
    },
    /// Whether the worker encodes H.265 on its display, which it probes in
    /// the interactive session: asked only for a viewer that decodes it.
    H265 {},
    /// The bound display's modes, answered in `modes`.
    DisplayModes {
        expected: DesktopContext,
    },
    /// Journalled in the helper's protected directory first.
    SetDisplayMode {
        expected: DesktopContext,
        mode: Mode,
    },
    /// Answered in `restored`.
    RestoreDisplayMode {},
    /// The pointer's shape, answered in `shape` with its bitmap as the byte
    /// record. From the first one on, the worker also reads the shape with
    /// every pointer it observes, and numbers each new one in `shape_serial`,
    /// so the host asks again only for a newer one.
    CursorShape {},
    Release {},
    Close {},
}
impl Request {
    /// How long this call may take, on both ends of the pipe.
    pub(crate) fn timeout(&self) -> std::time::Duration {
        match self {
            Self::Start { .. }
            | Self::DisplayModes { .. }
            | Self::SetDisplayMode { .. }
            | Self::RestoreDisplayMode {} => MODE_TIMEOUT,
            Self::Attach { .. }
            | Self::Context {}
            | Self::Capture {}
            | Self::ConfigureVideo { .. }
            | Self::Video { .. }
            | Self::Presented { .. }
            | Self::Input { .. }
            | Self::H265 {}
            | Self::CursorShape {}
            | Self::Release {}
            | Self::Close {} => TIMEOUT,
        }
    }
    /// Its answer reports the bound desktop, which the host keeps.
    pub(crate) fn reports_context(&self) -> bool {
        match self {
            Self::Release {} | Self::Close {} | Self::RestoreDisplayMode {} => false,
            Self::Start { .. }
            | Self::Attach { .. }
            | Self::Context {}
            | Self::Capture {}
            | Self::ConfigureVideo { .. }
            | Self::Video { .. }
            | Self::Presented { .. }
            | Self::Input { .. }
            | Self::H265 {}
            | Self::DisplayModes { .. }
            | Self::SetDisplayMode { .. }
            | Self::CursorShape {} => true,
        }
    }
    /// The worker reads the pointer for it. An applied input reads it too,
    /// but only when it can, so its answer may carry none.
    pub(crate) fn observes_cursor(&self) -> bool {
        match self {
            Self::Context {} | Self::Capture {} | Self::Video { .. } | Self::CursorShape {} => true,
            Self::Start { .. }
            | Self::Attach { .. }
            | Self::ConfigureVideo { .. }
            | Self::Presented { .. }
            | Self::Input { .. }
            | Self::H265 {}
            | Self::DisplayModes { .. }
            | Self::SetDisplayMode { .. }
            | Self::RestoreDisplayMode {}
            | Self::Release {}
            | Self::Close {} => false,
        }
    }
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub context: Option<DesktopContext>,
    pub error: Option<String>,
    /// What kind of failure `error` is, when the worker's backend said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<crate::VideoFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<crate::CursorPosition>,
    /// Units the worker's encoder holds, for the host's submit gate.
    pub pending: u32,
    /// In the `Start` response: the double-click time and box of the
    /// interactive session, read by the worker, which runs in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub double_click: Option<crate::DoubleClickGuard>,
    /// In the `Start` response: what the worker's backend can do, H.265
    /// aside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caps: Option<StartCaps>,
    /// In the `H265` response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h265: Option<bool>,
    /// In the `DisplayModes` response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<ModeList>,
    /// In the `RestoreDisplayMode` response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored: Option<bool>,
    /// To a request that observes the pointer, once the host asked for a
    /// shape: the number of the newest shape the worker read, 0 before the
    /// first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape_serial: Option<u64>,
    /// In the `CursorShape` response, when the worker has read one: the
    /// shape whose bitmap the byte record holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<ShapeHeader>,
}
/// A pointer shape but its bitmap, which follows as the byte record. The
/// host checks it, and the record's length, before it takes the shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShapeHeader {
    pub kind: CursorKind,
    pub width: u32,
    pub height: u32,
    pub hot_x: u32,
    pub hot_y: u32,
}
impl ShapeHeader {
    pub(crate) fn of(shape: &CursorShape) -> Self {
        let (hot_x, hot_y) = shape.hotspot();
        Self {
            kind: shape.kind(),
            width: shape.width(),
            height: shape.height(),
            hot_x,
            hot_y,
        }
    }
    /// The shape with `rgba`; `InvalidData` when either is out of bounds.
    pub(crate) fn shape(self, rgba: Vec<u8>) -> std::io::Result<CursorShape> {
        CursorShape::new(
            self.kind,
            self.width,
            self.height,
            (self.hot_x, self.hot_y),
            rgba,
        )
    }
}
/// What a worker's backend can do, as its `Start` answer says: never H.265,
/// which costs a probe and is asked only when a viewer decodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartCaps {
    pub lock: bool,
    pub display_modes: bool,
    pub os: Option<HostOs>,
    pub cursor: CursorMode,
    /// The worker injects `Input::PhysicalKey`.
    pub physical_keys: bool,
    /// The worker reads the pointer's shape (`Request::CursorShape`).
    pub cursor_shapes: bool,
}
impl From<&BackendCaps> for StartCaps {
    fn from(caps: &BackendCaps) -> Self {
        let BackendCaps {
            h265: _,
            lock,
            display_modes,
            os,
            cursor,
            physical_keys,
            cursor_shapes,
        } = *caps;
        Self {
            lock,
            display_modes,
            os,
            cursor,
            physical_keys,
            cursor_shapes,
        }
    }
}
impl From<StartCaps> for BackendCaps {
    fn from(caps: StartCaps) -> Self {
        let StartCaps {
            lock,
            display_modes,
            os,
            cursor,
            physical_keys,
            cursor_shapes,
        } = caps;
        Self {
            h265: false,
            lock,
            display_modes,
            os,
            cursor,
            physical_keys,
            cursor_shapes,
        }
    }
}
