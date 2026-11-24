//! The out-of-process backend's protocol: one request, one response and
//! one byte record (a unit or a still), in order, per call of the
//! `Backend` it carries. A change to any of it bumps the helper protocol.
use crate::{
    BackendCaps, CursorKind, CursorMode, CursorShape, DesktopContext, FailureKind, HostOs, Input,
    RateModel,
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
    /// Drain finished output, then submit one capture when `submit` is set;
    /// the answer's `submitted` says when that capture found nothing new.
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
    /// In the answer to a `Video` request that submits: `Some(false)` when
    /// the worker's source had no new picture to submit, which leaves the
    /// host's tick armed. Absent, it was handed the tick, or spent it on a
    /// full encoder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted: Option<bool>,
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
    /// The GPU the worker captures on, when it could name it with its
    /// driver: the host keeps the worker's H.265 answer under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<AdapterKey>,
}
impl From<&BackendCaps> for StartCaps {
    /// What `caps` say, and no adapter, which the worker adds.
    fn from(caps: &BackendCaps) -> Self {
        let BackendCaps {
            h265: _,
            lock,
            display_modes,
            os,
            cursor,
            physical_keys,
            cursor_shapes,
            rate_model: _,
        } = *caps;
        Self {
            lock,
            display_modes,
            os,
            cursor,
            physical_keys,
            cursor_shapes,
            adapter: None,
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
            adapter: _,
        } = caps;
        Self {
            h265: false,
            lock,
            display_modes,
            os,
            cursor,
            physical_keys,
            cursor_shapes,
            // Never on the pipe: the worker's Media Foundation encoder budgets
            // per frame, and `Nominal` is the safe start anyway, as the latch
            // corrects it and nothing corrects `Timestamp`.
            rate_model: RateModel::Nominal,
        }
    }
}

/// A GPU and its driver, as a worker names the one it captures on: the
/// adapter's PCI identity (vendor, device, subsystem and revision) and its
/// user-mode driver's version. What a worker answered of that GPU holds for
/// the next worker on it until its driver changes, which changes the key. An
/// adapter's LUID would not do: it names the adapter only until the system
/// restarts, and says nothing of its driver. A key names a vendor and a
/// driver version, without which it could not tell a driver update: the
/// worker builds one only through [`AdapterKey::new`], and the host decodes
/// one only through the same check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawAdapterKey")]
pub(crate) struct AdapterKey {
    vendor_id: u32,
    device_id: u32,
    subsys_id: u32,
    revision: u32,
    /// Its four 16-bit parts, most significant first, as DXGI packs them.
    driver_version: u64,
}
impl AdapterKey {
    /// `None` without a vendor or a driver version.
    pub(crate) fn new(
        vendor_id: u32,
        device_id: u32,
        subsys_id: u32,
        revision: u32,
        driver_version: u64,
    ) -> Option<Self> {
        (vendor_id != 0 && driver_version != 0).then_some(Self {
            vendor_id,
            device_id,
            subsys_id,
            revision,
            driver_version,
        })
    }
}
/// An [`AdapterKey`] as it crosses the pipe, before it is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAdapterKey {
    vendor_id: u32,
    device_id: u32,
    subsys_id: u32,
    revision: u32,
    driver_version: u64,
}
impl TryFrom<RawAdapterKey> for AdapterKey {
    type Error = &'static str;
    fn try_from(raw: RawAdapterKey) -> Result<Self, Self::Error> {
        let RawAdapterKey {
            vendor_id,
            device_id,
            subsys_id,
            revision,
            driver_version,
        } = raw;
        Self::new(vendor_id, device_id, subsys_id, revision, driver_version)
            .ok_or("an adapter key without a vendor or a driver version")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(adapter: Option<AdapterKey>) -> StartCaps {
        StartCaps {
            lock: true,
            display_modes: true,
            os: Some(HostOs::Windows),
            cursor: CursorMode::Separate,
            physical_keys: true,
            cursor_shapes: true,
            adapter,
        }
    }

    /// Protocol 11: the `Start` caps name the worker's adapter, or leave it
    /// out as every earlier worker did; either reads back as it was sent.
    #[test]
    fn start_caps_cross_with_and_without_an_adapter() {
        let without = caps(None);
        let json = serde_json::to_string(&without).unwrap();
        assert!(!json.contains("adapter"));
        assert_eq!(serde_json::from_str::<StartCaps>(&json).unwrap(), without);

        // An NVIDIA GPU on driver 32.0.15.6094.
        let driver = (32 << 48) | (15 << 16) | 6094;
        let with = caps(AdapterKey::new(0x10de, 0x2c05, 0x8a1e_1043, 0xa1, driver));
        assert!(with.adapter.is_some());
        let json = serde_json::to_string(&with).unwrap();
        assert!(json.contains(
            r#""adapter":{"vendor_id":4318,"device_id":11269,"subsys_id":2317226051,"revision":161,"driver_version":9007199255730126}"#
        ));
        assert_eq!(serde_json::from_str::<StartCaps>(&json).unwrap(), with);
    }

    /// The host decodes only a key the worker could have built: a vendor and
    /// a driver version, each field within its width, and nothing else.
    #[test]
    fn an_adapter_key_is_checked_as_it_is_decoded() {
        assert_eq!(AdapterKey::new(0, 1, 0, 0, 1), None);
        assert_eq!(AdapterKey::new(0x8086, 1, 0, 0, 0), None);
        let key = |fields: &str| serde_json::from_str::<AdapterKey>(&format!("{{{fields}}}"));
        let valid =
            r#""vendor_id":32902,"device_id":42880,"subsys_id":0,"revision":4,"driver_version":1"#;
        assert_eq!(
            key(valid).unwrap(),
            AdapterKey::new(0x8086, 0xa780, 0, 4, 1).unwrap()
        );
        for invalid in [
            r#""vendor_id":0,"device_id":1,"subsys_id":0,"revision":0,"driver_version":1"#,
            r#""vendor_id":1,"device_id":1,"subsys_id":0,"revision":0,"driver_version":0"#,
            r#""vendor_id":4294967296,"device_id":1,"subsys_id":0,"revision":0,"driver_version":1"#,
            r#""vendor_id":1,"device_id":-1,"subsys_id":0,"revision":0,"driver_version":1"#,
            r#""vendor_id":1,"device_id":1,"subsys_id":0,"revision":0"#,
        ] {
            assert!(key(invalid).is_err(), "{invalid}");
        }
        assert!(key(&format!(r#"{valid},"luid":7"#)).is_err());
        // A `Start` answer whose key fails the check is refused whole.
        let answer = serde_json::to_string(&caps(None)).unwrap().replace(
            '}',
            r#","adapter":{"vendor_id":0,"device_id":1,"subsys_id":0,"revision":0,"driver_version":1}}"#,
        );
        assert!(serde_json::from_str::<StartCaps>(&answer).is_err());
    }
}
