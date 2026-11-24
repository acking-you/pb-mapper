//! The out-of-process backend's protocol. The host sends one request per
//! call of the `Backend` it carries, and waits for its answer: a
//! [`Record::Reply`] and its byte record (a unit, a still or a shape's
//! bitmap; empty when it has none). Between answers the worker pushes what
//! the host would otherwise have to ask for again and again: each unit its
//! encoder finished, with its byte record ([`Record::Unit`]), a failure its
//! encoder met meanwhile ([`Record::Failed`]), that its source wants the
//! tick the host left armed ([`Record::Fresh`]), and the pointer as it
//! moves ([`Record::Pointer`]). Every record arrives in the order the
//! worker wrote it. A change to any of it bumps the helper protocol.
use crate::{
    BackendCaps, CursorKind, CursorMode, CursorPosition, CursorShape, DesktopContext, Display,
    FailureKind, HostOs, Input, Presented, RateModel, VideoFormat,
    modes::{Mode, ModeList},
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// The longest one call may take, and a worker may wait to start.
pub(crate) const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// The longest a call that may switch a display mode takes: a switch blocks
/// while the display retrains its link, a reset may be two switches, and
/// the journal is synced first; `Start` replays a journal a killed worker
/// left. A call that outlives it ends the session, and the worker's
/// backend, dropped with it, puts the mode back or leaves it journalled.
pub(crate) const MODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// The most units a worker's encoder may say it holds, in an answer or a
/// push: more is a broken worker, whose link the host drops.
pub(crate) const PENDING_MAX: u32 = 64;
/// The oldest present a worker dates a picture by, and a host takes a date
/// of: an older one is left out, and ignored.
pub(crate) const PRESENT_AGE_MAX: Duration = Duration::from_secs(1);

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
    /// Drain one finished unit into the answer, then submit one capture of
    /// `expected`, which the worker checks around it; the answer's
    /// `submitted` says when that capture left the tick armed: it found
    /// nothing new, or an encoder that had not asked for input. The worker
    /// then pushes `Record::Fresh` once its source wants that tick, and
    /// every other unit as it finishes. The helper never paces.
    Video {
        expected: DesktopContext,
        codec: crate::VideoCodec,
        keyframe: bool,
        rate: crate::VideoRate,
    },
    /// Whether the host's thread C wants video (`Backend::video_paused`).
    /// While it does, the worker pushes the pointer as it moves and tells
    /// the host when its source wants the armed tick, and holds fine
    /// timers; otherwise its capture thread stops looking. Units are pushed
    /// either way.
    Stream {
        on: bool,
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
    /// The GPU and driver that drive the bound display, answered in
    /// `adapter`: asked only for a viewer that decodes H.265, before `H265`,
    /// which the host skips when a worker's probe on the same GPU and driver
    /// answered it earlier in the host's process.
    Adapter {},
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
            | Self::Stream { .. }
            | Self::Presented { .. }
            | Self::Input { .. }
            | Self::Adapter {}
            | Self::H265 {}
            | Self::CursorShape {}
            | Self::Release {}
            | Self::Close {} => TIMEOUT,
        }
    }
    /// Its answer reports the bound desktop, which the host keeps.
    pub(crate) fn reports_context(&self) -> bool {
        match self {
            Self::Release {}
            | Self::Close {}
            | Self::RestoreDisplayMode {}
            | Self::Stream { .. } => false,
            Self::Start { .. }
            | Self::Attach { .. }
            | Self::Context {}
            | Self::Capture {}
            | Self::ConfigureVideo { .. }
            | Self::Video { .. }
            | Self::Presented { .. }
            | Self::Input { .. }
            | Self::Adapter {}
            | Self::H265 {}
            | Self::DisplayModes { .. }
            | Self::SetDisplayMode { .. }
            | Self::CursorShape {} => true,
        }
    }
    /// The worker reads the pointer for it. An applied input reads it too,
    /// but only when it can, so its answer may carry none. A capture does
    /// not: the worker pushes the pointer as it moves (`Record::Pointer`).
    pub(crate) fn observes_cursor(&self) -> bool {
        match self {
            Self::Context {} | Self::Capture {} | Self::CursorShape {} => true,
            Self::Start { .. }
            | Self::Attach { .. }
            | Self::ConfigureVideo { .. }
            | Self::Video { .. }
            | Self::Stream { .. }
            | Self::Presented { .. }
            | Self::Input { .. }
            | Self::Adapter {}
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
    /// the worker's source had no new picture to submit, or its encoder had
    /// not asked for input yet, which leaves the host's tick armed. Absent,
    /// it was handed the tick, or spent it on a full encoder.
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
    /// In the `Adapter` response: the GPU and driver that drive the bound
    /// display, when the worker can name them. In the `H265` response: the
    /// GPU and driver whose probe `h265` is, under which alone the host
    /// keeps it; absent when the worker answered without a probe (its
    /// display undescribed, or no GPU found for it), and that answer holds
    /// for its session alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<AdapterKey>,
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
    /// In the answer to a `Video` request that submitted a new picture its
    /// source dated: how long before the worker wrote the answer the
    /// display presented it, in microseconds, at most `PRESENT_AGE_MAX`.
    /// The host dates the picture by its own receipt of the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presented_ago_us: Option<u32>,
    /// With `presented_ago_us`: the presents the picture covers, at least
    /// 1, and 255 for any more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presented_count: Option<u8>,
}

/// What a worker writes: the answer to the host's request, or a push
/// between answers, each in the order the worker wrote it. `Reply` and
/// `Unit` are followed by their byte record, the others by none. The host
/// checks each: a unit within `ipc::MAX_FRAME`, of the generation it
/// expects when it takes it; `pending` within `PENDING_MAX`; a pointer
/// inside the display the worker last reported; an age within
/// `PRESENT_AGE_MAX`; an unknown kind or field is refused.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Record {
    /// The answer to the host's request.
    Reply(Box<Response>),
    /// A unit the worker's encoder finished, of a capture checked against
    /// the desktop of `generation`, and the units the encoder still holds;
    /// its bytes follow.
    Unit {
        generation: u64,
        video: VideoFormat,
        pending: u32,
        /// When the worker collected it from its encoder, in microseconds
        /// since the Unix epoch on the system clock, which the host on the
        /// same computer reads too: for the host's debug event
        /// `desktop_helper_push` alone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collected_unix_us: Option<u64>,
    },
    /// The worker's encoder failed as the worker collected its output, as
    /// a drain fails: the host's thread C takes the failure in the units'
    /// order. The worker's source is gone; the next capture builds another.
    Failed {
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<FailureKind>,
        pending: u32,
    },
    /// The worker's source wants the tick the host's last capture left
    /// armed: it published a new picture, dated by `presented_ago_us` and
    /// `presented_count` when it could date it, or a repeat came due
    /// (liveness, a keyframe), or its encoder asked for input. Sent once
    /// per armed tick, and only while the host streams.
    Fresh {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        presented_ago_us: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        presented_count: Option<u8>,
    },
    /// The pointer on the desktop the worker last reported, as an answer
    /// that observes the pointer would carry it, and, once the host asked
    /// for shapes, the newest shape's number: while the host streams, when
    /// either changed.
    Pointer {
        cursor: Option<CursorPosition>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shape_serial: Option<u64>,
    },
}

/// `presented_ago_us` and `presented_count` of `presented` as of `now`:
/// none for a picture presented more than `PRESENT_AGE_MAX` ago. One
/// presented after `now` is no age.
pub(crate) fn present_age(presented: Option<Presented>, now: Instant) -> (Option<u32>, Option<u8>) {
    let Some(presented) = presented else {
        return (None, None);
    };
    let age = now.saturating_duration_since(presented.at);
    if age > PRESENT_AGE_MAX {
        return (None, None);
    }
    (
        u32::try_from(age.as_micros()).ok(),
        Some(u8::try_from(presented.presents.max(1)).unwrap_or(u8::MAX)),
    )
}

/// The picture a worker dated `ago_us` before the host's `received`,
/// covering `count` presents: none without an age, or with one over
/// `PRESENT_AGE_MAX`.
pub(crate) fn presented_at(
    ago_us: Option<u32>,
    count: Option<u8>,
    received: Instant,
) -> Option<Presented> {
    let age = Duration::from_micros(ago_us?.into());
    if age > PRESENT_AGE_MAX {
        return None;
    }
    Some(Presented::new(
        received.checked_sub(age)?,
        count.map_or(1, u32::from),
    ))
}

/// Whether a pushed pointer lies inside `display`, as the worker's own read
/// confines it: a pointer it does not show is at the origin.
pub(crate) fn inside(cursor: &CursorPosition, display: &Display) -> bool {
    u32::try_from(cursor.x).is_ok_and(|x| x < display.width)
        && u32::try_from(cursor.y).is_ok_and(|y| y < display.height)
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
            rate_model: _,
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
            // Never on the pipe: the worker's Media Foundation encoder budgets
            // per frame, and `Nominal` is the safe start anyway, as the latch
            // corrects it and nothing corrects `Timestamp`.
            rate_model: RateModel::Nominal,
        }
    }
}

/// A GPU and its driver, as a worker names the one that drives its display:
/// the adapter's PCI identity (vendor, device, subsystem and revision) and
/// its user-mode driver's version. What a worker's probe answered of that
/// GPU holds for the next worker on it until its driver changes, which
/// changes the key. An adapter's LUID would not do: it names the adapter
/// only until the system restarts, and says nothing of its driver. A key
/// names a vendor and a driver version, without which it could not tell a
/// driver update: the worker builds one only through [`AdapterKey::new`],
/// and the host decodes one only through the same check.
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

    /// Protocol 11: `Request::Adapter` takes no field, and a response names
    /// an adapter, or leaves it out as every other answer does; either reads
    /// back as it was sent.
    #[test]
    fn an_adapter_crosses_in_a_response_or_stays_out() {
        let asked = serde_json::to_string(&Request::Adapter {}).unwrap();
        assert_eq!(asked, r#"{"op":"adapter"}"#);
        assert!(serde_json::from_str::<Request>(r#"{"op":"adapter","luid":7}"#).is_err());

        let json = serde_json::to_string(&Response::default()).unwrap();
        assert!(!json.contains("adapter"));
        assert_eq!(
            serde_json::from_str::<Response>(&json).unwrap().adapter,
            None
        );

        // An NVIDIA GPU on driver 32.0.15.6094.
        let driver = (32 << 48) | (15 << 16) | 6094;
        let adapter = AdapterKey::new(0x10de, 0x2c05, 0x8a1e_1043, 0xa1, driver);
        assert!(adapter.is_some());
        let json = serde_json::to_string(&Response {
            h265: Some(true),
            adapter,
            ..Response::default()
        })
        .unwrap();
        assert!(json.contains(
            r#""adapter":{"vendor_id":4318,"device_id":11269,"subsys_id":2317226051,"revision":161,"driver_version":9007199255730126}"#
        ));
        assert_eq!(
            serde_json::from_str::<Response>(&json).unwrap().adapter,
            adapter
        );
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
        // An answer whose key fails the check is refused whole.
        let answer = serde_json::to_string(&Response::default()).unwrap().replace(
            '}',
            r#","adapter":{"vendor_id":0,"device_id":1,"subsys_id":0,"revision":0,"driver_version":1}}"#,
        );
        assert!(serde_json::from_str::<Response>(&answer).is_err());
    }

    /// Protocol 12: every record the worker writes names its kind; an answer
    /// is its response's fields beside `"kind":"reply"`, and the fields a
    /// push leaves out are absent. A kind, or a field, the host does not know
    /// is refused, and so is a `Video` request that still asks to wait.
    #[test]
    fn a_record_names_its_kind_and_refuses_what_it_does_not_know() {
        let answer = serde_json::to_string(&Record::Reply(Box::new(Response {
            pending: 2,
            submitted: Some(false),
            ..Response::default()
        })))
        .unwrap();
        assert_eq!(
            answer,
            r#"{"kind":"reply","context":null,"error":null,"submitted":false,"pending":2}"#
        );
        let Record::Reply(read) = serde_json::from_str(&answer).unwrap() else {
            panic!("an answer");
        };
        assert_eq!((read.pending, read.submitted), (2, Some(false)));
        let fresh = Record::Fresh {
            presented_ago_us: None,
            presented_count: None,
        };
        assert_eq!(
            serde_json::to_string(&fresh).unwrap(),
            r#"{"kind":"fresh"}"#
        );
        for refused in [
            r#"{"kind":"repaint"}"#,
            r#"{"kind":"fresh","presented_at":1}"#,
            r#"{"kind":"reply","context":null,"error":null,"pending":0,"luid":7}"#,
            r#"{"kind":"pointer","cursor":null,"generation":1}"#,
            r#"{"pending":0}"#,
        ] {
            assert!(
                serde_json::from_str::<Record>(refused).is_err(),
                "{refused}"
            );
        }
        let video = |extra: &str| {
            format!(
                r#"{{"op":"video","expected":{{"display":{{"id":1,"name":"d","x":0,"y":0,"width":64,"height":64}},"generation":1,"desktop":"ordinary"}},"codec":"h264","keyframe":false,"rate":{{"bitrate":256000,"pace_fps":60}}{extra}}}"#
            )
        };
        assert!(serde_json::from_str::<Request>(&video("")).is_ok());
        assert!(serde_json::from_str::<Request>(&video(r#","wait_ms":4"#)).is_err());
        assert_eq!(
            serde_json::to_string(&Request::Stream { on: true }).unwrap(),
            r#"{"op":"stream","on":true}"#
        );
    }

    /// An age crosses as microseconds within a second, and is taken back
    /// against the host's receipt; one over a second is none, either way.
    /// A pointer is inside a display only on one of its pixels.
    #[test]
    fn an_age_crosses_within_a_second_and_a_pointer_within_its_display() {
        let now = Instant::now();
        let at = |ago_ms| now.checked_sub(Duration::from_millis(ago_ms)).unwrap();
        let presented = |ago_ms, presents| Some(Presented::new(at(ago_ms), presents));
        assert_eq!(present_age(presented(3, 2), now), (Some(3_000), Some(2)));
        assert_eq!(present_age(presented(3, 300), now).1, Some(u8::MAX));
        assert_eq!(present_age(presented(1_001, 1), now), (None, None));
        assert_eq!(present_age(None, now), (None, None));
        let dated = presented_at(Some(3_000), Some(2), now).unwrap();
        assert_eq!((dated.at, dated.presents), (at(3), 2));
        assert_eq!(presented_at(Some(3_000), None, now).unwrap().presents, 1);
        assert_eq!(presented_at(Some(1_000_001), Some(1), now), None);
        assert_eq!(presented_at(None, Some(1), now), None);

        let display = Display {
            id: 1,
            name: "d".into(),
            x: -1920,
            y: 0,
            width: 64,
            height: 48,
        };
        let cursor = |x, y| CursorPosition {
            x,
            y,
            visible: true,
        };
        assert!(inside(&cursor(0, 0), &display));
        assert!(inside(&cursor(63, 47), &display));
        assert!(!inside(&cursor(64, 0), &display));
        assert!(!inside(&cursor(0, 48), &display));
        assert!(!inside(&cursor(-1, 0), &display));
    }
}
