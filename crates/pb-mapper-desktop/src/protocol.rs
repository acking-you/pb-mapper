//! Desktop protocol v2 (ALPN `pb-mapper/desktop/2`): one JSON record per
//! message; only `HostMessage::Video` and `HostMessage::CursorShape` are
//! followed by one payload record (`wire.rs`). Every message and struct
//! rejects unknown fields, and every enum variant is a struct variant or wraps
//! one strict struct (`State`, `Stats`, `CursorShape`), because serde ignores
//! extra fields on an internally tagged unit variant. A v2.x peer sends a new
//! message, field or value only after the other side listed the matching
//! capability.
use crate::{
    CursorKind, CursorShape, Display, InputEvent, KeyframeReason, QualityPreset, VideoCodec,
    VideoLimits, modes::ModeList,
};
use serde::{Deserialize, Serialize};

/// The host captures and injects on a Windows backend that can lock it. The
/// v2.0 host advertises nothing else: it cannot tell, before thread C builds
/// the backend, whether the privileged helper (`secure_desktop`) captures.
pub(crate) const CAPABILITY_LOCK: &str = "lock";
/// The viewer reads `Stats::encoder_dwell_ms`; a host writes it to no other.
pub(crate) const CAPABILITY_ENCODER_DWELL: &str = "encoder_dwell";
/// The viewer reads `Cursor::input_sequence`; a host writes it to no other.
pub(crate) const CAPABILITY_CURSOR_SEQUENCE: &str = "cursor_sequence";
/// From a viewer: it can decode `VideoCodec::H265`. A host that can encode it
/// then answers `Accepted { codec: h265 }`; no other host ever sees the
/// value, which is never in `codecs`.
pub(crate) const CAPABILITY_H265: &str = "h265";
/// From a viewer: `limits.max_fps` is a frame rate its user chose, which
/// replaces every quality preset's own ceiling for the session.
pub(crate) const CAPABILITY_FPS_OVERRIDE: &str = "fps_override";
/// Both ways: this end speaks display modes. A host lists it when its
/// backend can change its display's mode; a viewer that listed it reads
/// `StateRecord::modes`, `DisplayModeFailed` and `DisplayChanging`, and
/// sends `DisplayMode` and `RestoreDisplayMode` only to a host that did.
pub(crate) const CAPABILITY_DISPLAY_MODE: &str = "display_mode";
/// From a host: it injects `Input::PhysicalKey`. A viewer sends that input
/// to no other host.
pub(crate) const CAPABILITY_PHYSICAL_KEYS: &str = "physical_keys";
/// Both ways: this end speaks pointer shapes. A host lists it when its
/// backend reports them; only when both listed it does the host write
/// `CursorShape` and `Cursor::shape`.
pub(crate) const CAPABILITY_CURSOR_SHAPE: &str = "cursor_shape";
const MAX_CAPABILITIES: usize = 16;

/// The shape slots of a session. A `CursorShape` defines or overwrites one;
/// the host chooses which, and a viewer keeps exactly this many, so neither
/// table grows and nothing is ever evicted by message.
pub(crate) const CURSOR_SLOTS: usize = 64;

/// A shape slot: a number below `CURSOR_SLOTS`, any other a protocol error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub(crate) struct ShapeSlot(u8);
impl ShapeSlot {
    pub(crate) fn new(index: usize) -> Option<Self> {
        u8::try_from(index)
            .ok()
            .filter(|index| usize::from(*index) < CURSOR_SLOTS)
            .map(Self)
    }
    pub(crate) fn index(self) -> usize {
        self.0.into()
    }
}
impl TryFrom<u8> for ShapeSlot {
    type Error = &'static str;
    fn try_from(index: u8) -> Result<Self, Self::Error> {
        Self::new(index.into()).ok_or("cursor shape slot out of bounds")
    }
}
impl From<ShapeSlot> for u8 {
    fn from(slot: ShapeSlot) -> Self {
        slot.0
    }
}

/// Capability lists hold at most 16 values of `[a-z0-9_]{1,32}`. A receiver
/// ignores values it does not know, but a malformed list is a protocol error.
pub(crate) fn valid_capabilities(capabilities: &[String]) -> bool {
    capabilities.len() <= MAX_CAPABILITIES
        && capabilities.iter().all(|value| {
            (1..=32).contains(&value.len())
                && value
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ViewerMessage {
    /// First and only first message. The host answers `Accepted` or `Rejected`.
    Authenticate {
        /// The 43-character base64url form of the invitation's 32-byte token.
        token: String,
        /// One or two codecs, in the viewer's preference order; never
        /// `h265`, which is the capability of that name.
        codecs: Vec<VideoCodec>,
        /// Constant for the session.
        limits: VideoLimits,
        /// The initial preset.
        quality: QualityPreset,
        /// The v2.0 viewer sends none.
        capabilities: Vec<String>,
    },
    /// Cumulative transport receipt: every `Video` with `seq` up to this one was
    /// fully read. Send credit only; it grants, renews or proves nothing about
    /// input.
    Received {
        seq: u64,
    },
    /// Echo of `HostMessage::Probe`, written by the viewer's network task.
    ProbeEcho {
        nonce: u64,
    },
    /// The newest frame actually presented and validated by the viewer's
    /// `VideoSink`. Input authorisation only.
    Presented {
        generation: u64,
        frame_id: u64,
    },
    Keyframe {
        reason: KeyframeReason,
    },
    /// 1..=120: a runtime limit, separate from `limits`.
    FpsCap {
        fps: u32,
    },
    /// Live: never a reconnect, never an encoder rebuild.
    Quality {
        preset: QualityPreset,
    },
    /// The viewer is hidden: the host ends control and stops capture.
    Pause {},
    /// The host forces an IDR and restarts its QoS startup.
    Resume {},
    /// Sent on change: resets the host's RTT baseline.
    Path {
        route: Route,
    },
    /// Ordered input lane.
    Control {
        enabled: bool,
    },
    /// Ordered input lane.
    Release {},
    /// Ordered input lane; includes `Input::Lock`.
    Input {
        event: InputEvent,
    },
    /// Only after `PING_IDLE` without any other message.
    Ping {},
    /// Ordered input lane: change the host display's mode to one it listed
    /// in `StateRecord::modes`. Only to a host that listed `display_mode`.
    DisplayMode {
        width: u32,
        height: u32,
    },
    /// Ordered input lane: back to the display's original mode. Only to a
    /// host that listed `display_mode`.
    RestoreDisplayMode {},
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Direct,
    RelayTcp,
    RelayQuic,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HostMessage {
    Accepted {
        codec: VideoCodec,
        limits: VideoLimits,
        nominal_fps: u32,
        quality: QualityPreset,
        capabilities: Vec<String>,
    },
    /// Written only after the token check, within a 1 s timeout, then close.
    Rejected {
        reason: RejectReason,
    },
    State(StateRecord),
    /// Followed by exactly one payload record (an Annex B access unit or a JPEG).
    Video {
        /// 1, 2, 3 … per session, assigned at admission.
        seq: u64,
        generation: u64,
        /// Monotonic for the session, never reused.
        frame_id: u64,
        keyframe: bool,
        /// A static-desktop liveness re-encode.
        duplicate: bool,
        width: u32,
        height: u32,
        pts_us: u64,
        /// Encoder submit to socket write start: encode, hand-off and credit wait.
        host_us: u32,
    },
    Cursor {
        generation: u64,
        x: i32,
        y: i32,
        visible: bool,
        /// The sequence of the last input the host had taken off its lane,
        /// applied or not, when it sampled the pointer: below the viewer's
        /// newest move, the report predates that move. Only to a viewer that
        /// listed `cursor_sequence`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_sequence: Option<u64>,
        /// The pointer's shape: a slot a `CursorShape` defined earlier in
        /// the session. Only when both ends listed `cursor_shape`. A viewer
        /// takes it from every report, whether or not it takes the position.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shape: Option<ShapeSlot>,
    },
    /// Defines a pointer shape in its slot, followed by exactly one payload
    /// record: its straight RGBA8 bitmap. Only when both ends listed
    /// `cursor_shape`, and before the first `Cursor` naming that slot.
    CursorShape(ShapeRecord),
    /// 1, 2, 3 …; one outstanding at a time, in stream order.
    Probe {
        nonce: u64,
    },
    /// Every `STATS_INTERVAL`; also the host-to-viewer liveness.
    Stats(StatsRecord),
    Notice {
        code: NoticeCode,
        message: String,
    },
    /// Final and best effort (`STOP_WRITE_TIMEOUT`).
    Stopped {
        reason: StopReason,
        message: String,
    },
}

/// What a `Stats` message carries.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatsRecord {
    pub capture_fps: f32,
    pub pace_fps: u32,
    pub target_fps: u32,
    /// 0 = no viewer cap.
    pub fps_cap: u32,
    /// Expected: bits per frame × pace.
    pub bitrate: u32,
    pub encoder_bitrate: u32,
    pub sent_bitrate: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_rtt_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_delay_ms: Option<u32>,
    pub ack_queue_ms: u32,
    /// The median time a unit stayed in the encoder, from submit to output,
    /// over the last second. Only to a viewer that listed `encoder_dwell`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder_dwell_ms: Option<u32>,
    pub dropped_frames: u64,
    /// Ticks skipped for transport backpressure or a full encoder.
    pub skipped_ticks: u64,
    pub keyframes: u64,
    pub frames: u64,
    pub bytes: u64,
    pub adaptation: Adaptation,
    pub rate_model: RateModel,
    pub gop: u32,
}

/// What a `State` message carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StateRecord {
    pub state: SessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StateReason>,
    /// Human text; a UI keys off `reason`.
    pub message: String,
    pub control_allowed: bool,
    pub controlling: bool,
    pub generation: u64,
    /// Physical pixels: the input space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Display>,
    pub desktop: DesktopKind,
    /// The encoded size of this generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoSize>,
    /// The display's modes, as they are in this generation. Only to a
    /// viewer that listed `display_mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<ModeList>,
}

/// What a `CursorShape` message carries: the slot it defines and the shape
/// but its bitmap, which the payload record holds. Each side is
/// 1..=`CursorShape::MAX_SIDE` and the hotspot lies inside; decoding refuses
/// anything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedShapeRecord")]
pub(crate) struct ShapeRecord {
    pub id: ShapeSlot,
    pub kind: CursorKind,
    pub hot_x: u16,
    pub hot_y: u16,
    pub width: u16,
    pub height: u16,
}
impl ShapeRecord {
    /// The record that defines `shape` in `id`.
    pub(crate) fn new(id: ShapeSlot, shape: &CursorShape) -> Self {
        let side = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
        let (hot_x, hot_y) = shape.hotspot();
        Self {
            id,
            kind: shape.kind(),
            hot_x: side(hot_x),
            hot_y: side(hot_y),
            width: side(shape.width()),
            height: side(shape.height()),
        }
    }
    /// The length its payload must have.
    pub(crate) fn payload_len(&self) -> usize {
        CursorShape::bytes(self.width.into(), self.height.into())
    }
    /// The shape it defines, with its payload.
    pub(crate) fn shape(&self, rgba: &[u8]) -> std::io::Result<CursorShape> {
        CursorShape::new(
            self.kind,
            self.width.into(),
            self.height.into(),
            (self.hot_x.into(), self.hot_y.into()),
            rgba,
        )
    }
}
/// A `ShapeRecord` as it arrives, before its bounds are checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedShapeRecord {
    id: ShapeSlot,
    kind: CursorKind,
    hot_x: u16,
    hot_y: u16,
    width: u16,
    height: u16,
}
impl TryFrom<UncheckedShapeRecord> for ShapeRecord {
    type Error = &'static str;
    fn try_from(record: UncheckedShapeRecord) -> Result<Self, Self::Error> {
        let UncheckedShapeRecord {
            id,
            kind,
            hot_x,
            hot_y,
            width,
            height,
        } = record;
        let side = 1..=CursorShape::MAX_SIDE;
        if !side.contains(&width.into())
            || !side.contains(&height.into())
            || hot_x >= width
            || hot_y >= height
        {
            return Err("cursor shape out of bounds");
        }
        Ok(Self {
            id,
            kind,
            hot_x,
            hot_y,
            width,
            height,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    Denied,
    Busy,
    InvalidRequest,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Connecting,
    Streaming,
    Paused,
    CaptureUnavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateReason {
    SecureDesktop,
    HelperUnavailable,
    CaptureFailed,
    InputFailed,
    ControlDenied,
    InputCongestion,
    StalePresentation,
    DisplayLost,
    /// Capture failed within the settle window of a display mode change.
    /// Only to a viewer that listed `display_mode`.
    DisplayChanging,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopKind {
    Ordinary,
    Secure,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoSize {
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adaptation {
    #[default]
    Startup,
    Steady,
    Raising,
    LoweringFps,
    LoweringBitrate,
    Unresponsive,
    Paused,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateModel {
    #[default]
    Nominal,
    Timestamp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeCode {
    Locked,
    LockFailed,
    /// A display mode change or restore was refused, or failed. Only to a
    /// viewer that listed `display_mode`.
    DisplayModeFailed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    HostStopped,
    HostEnded,
    Restarting,
    Revoked,
    CaptureFailed,
    ProtocolError,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Button, Input, Key};
    use serde_json::{Value, json};

    /// Serialise, parse back and serialise again: the JSON must not change.
    fn round_trip<T: Serialize + serde::de::DeserializeOwned>(message: &T) -> Value {
        let first = serde_json::to_value(message).unwrap();
        let parsed: T = serde_json::from_value(first.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), first);
        first
    }

    /// The same JSON with one more field must be rejected.
    fn rejects_extra_field<T: serde::de::DeserializeOwned>(mut value: Value) {
        value["unexpected"] = json!(1);
        assert!(serde_json::from_value::<T>(value).is_err());
    }

    fn event(input: Input) -> InputEvent {
        InputEvent {
            generation: 2,
            frame_id: 7,
            sequence: 9,
            input,
        }
    }

    #[test]
    fn strict_parsing() {
        let limits = VideoLimits::for_viewer(60.0);
        let viewer = [
            ViewerMessage::Authenticate {
                token: "t".repeat(43),
                codecs: vec![VideoCodec::H264, VideoCodec::Jpeg],
                limits,
                quality: QualityPreset::Balanced,
                capabilities: vec![],
            },
            ViewerMessage::Received { seq: 3 },
            ViewerMessage::ProbeEcho { nonce: 1 },
            ViewerMessage::Presented {
                generation: 2,
                frame_id: 7,
            },
            ViewerMessage::Keyframe {
                reason: KeyframeReason::DecoderError,
            },
            ViewerMessage::FpsCap { fps: 30 },
            ViewerMessage::Quality {
                preset: QualityPreset::Smooth,
            },
            ViewerMessage::Pause {},
            ViewerMessage::Resume {},
            ViewerMessage::Path {
                route: Route::RelayQuic,
            },
            ViewerMessage::Control { enabled: true },
            ViewerMessage::Release {},
            ViewerMessage::Input {
                event: event(Input::Button {
                    button: Button::Left,
                    down: true,
                    drag_start: true,
                }),
            },
            ViewerMessage::Input {
                event: event(Input::Key {
                    key: Key::Digit7,
                    down: false,
                }),
            },
            ViewerMessage::Input {
                event: event(Input::Lock {}),
            },
            ViewerMessage::Input {
                event: event(Input::PhysicalKey {
                    usage: 0xE1,
                    down: true,
                }),
            },
            ViewerMessage::Ping {},
            ViewerMessage::DisplayMode {
                width: 1920,
                height: 1080,
            },
            ViewerMessage::RestoreDisplayMode {},
        ];
        for message in &viewer {
            rejects_extra_field::<ViewerMessage>(round_trip(message));
        }
        let host = [
            HostMessage::Accepted {
                codec: VideoCodec::H264,
                limits,
                nominal_fps: 60,
                quality: QualityPreset::Quality,
                capabilities: vec![CAPABILITY_LOCK.into(), "secure_desktop".into()],
            },
            HostMessage::Rejected {
                reason: RejectReason::Busy,
            },
            HostMessage::State(StateRecord {
                state: SessionState::CaptureUnavailable,
                reason: Some(StateReason::SecureDesktop),
                message: "secure desktop".into(),
                control_allowed: true,
                controlling: false,
                generation: 4,
                display: Some(Display {
                    id: 1,
                    name: "main".into(),
                    x: -1920,
                    y: 0,
                    width: 1920,
                    height: 1080,
                }),
                desktop: DesktopKind::Secure,
                video: Some(VideoSize {
                    width: 1280,
                    height: 720,
                }),
                modes: Some(ModeList {
                    current: crate::Mode {
                        width: 1920,
                        height: 1080,
                    },
                    original: crate::Mode {
                        width: 2560,
                        height: 1440,
                    },
                    supported: vec![crate::Mode {
                        width: 1920,
                        height: 1080,
                    }],
                }),
            }),
            HostMessage::Video {
                seq: 1,
                generation: 4,
                frame_id: 11,
                keyframe: true,
                duplicate: false,
                width: 1280,
                height: 720,
                pts_us: 16_666,
                host_us: 4_000,
            },
            HostMessage::Cursor {
                generation: 4,
                x: -3,
                y: 8,
                visible: true,
                input_sequence: Some(12),
                shape: ShapeSlot::new(CURSOR_SLOTS - 1),
            },
            HostMessage::CursorShape(ShapeRecord {
                id: ShapeSlot::new(3).unwrap(),
                kind: CursorKind::ResizeNwse,
                hot_x: 0,
                hot_y: 255,
                width: 1,
                height: 256,
            }),
            HostMessage::Probe { nonce: 2 },
            HostMessage::Stats(StatsRecord {
                capture_fps: 59.5,
                pace_fps: 60,
                target_fps: 60,
                fps_cap: 0,
                bitrate: 4_000_000,
                encoder_bitrate: 4_000_000,
                sent_bitrate: 3_900_000,
                rtt_ms: Some(40),
                base_rtt_ms: None,
                queue_delay_ms: Some(2),
                ack_queue_ms: 0,
                encoder_dwell_ms: Some(3),
                dropped_frames: 0,
                skipped_ticks: 1,
                keyframes: 1,
                frames: 60,
                bytes: 500_000,
                adaptation: Adaptation::Steady,
                rate_model: RateModel::Nominal,
                gop: u32::MAX,
            }),
            HostMessage::Notice {
                code: NoticeCode::LockFailed,
                message: "access denied".into(),
            },
            HostMessage::Notice {
                code: NoticeCode::DisplayModeFailed,
                message: "the remote resolution is still changing".into(),
            },
            HostMessage::Stopped {
                reason: StopReason::HostEnded,
                message: String::new(),
            },
        ];
        for message in &host {
            rejects_extra_field::<HostMessage>(round_trip(message));
        }

        // drag_start is omitted when false, and absent means false.
        let press = |drag_start| {
            serde_json::to_value(Input::Button {
                button: Button::Left,
                down: true,
                drag_start,
            })
            .unwrap()
        };
        assert_eq!(press(true)["drag_start"], true);
        assert_eq!(
            press(false),
            json!({"kind": "button", "button": "left", "down": true})
        );
        assert!(matches!(
            serde_json::from_value(press(false)).unwrap(),
            Input::Button {
                drag_start: false,
                ..
            }
        ));

        // Nested structs, unknown ops and unknown values fail in both directions.
        let mut nested = serde_json::to_value(&viewer[0]).unwrap();
        nested["limits"]["bitrate"] = json!(1);
        assert!(serde_json::from_value::<ViewerMessage>(nested).is_err());
        let mut nested = serde_json::to_value(&viewer[12]).unwrap();
        nested["event"]["input"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<ViewerMessage>(nested).is_err());
        assert!(
            serde_json::from_value::<ViewerMessage>(json!({"op": "probe", "nonce": 1})).is_err()
        );
        assert!(serde_json::from_value::<HostMessage>(json!({"t": "status"})).is_err());
        assert!(
            serde_json::from_value::<HostMessage>(
                json!({"t": "rejected", "reason": "maybe_later"})
            )
            .is_err()
        );
        // A shape slot, a side or a hotspot out of bounds is no message.
        let shape = serde_json::to_value(&host[5]).unwrap();
        assert_eq!(shape["t"], "cursor_shape");
        for (field, value) in [
            ("id", json!(CURSOR_SLOTS)),
            ("height", json!(CursorShape::MAX_SIDE + 1)),
            ("hot_y", json!(256)),
            ("kind", json!("spinner")),
        ] {
            let mut broken = shape.clone();
            broken[field] = value;
            assert!(
                serde_json::from_value::<HostMessage>(broken).is_err(),
                "{field}"
            );
        }
        let mut cursor = serde_json::to_value(&host[4]).unwrap();
        cursor["shape"] = json!(CURSOR_SLOTS);
        assert!(serde_json::from_value::<HostMessage>(cursor).is_err());

        assert!(valid_capabilities(&[
            "lock".into(),
            "secure_desktop".into()
        ]));
        assert!(!valid_capabilities(&["Lock".into()]));
        assert!(!valid_capabilities(&vec!["a".into(); MAX_CAPABILITIES + 1]));
    }

    /// `Stats` as peers of every v2 version read it: the tag first, the
    /// fields in their order, and an absent optional figure left out.
    #[test]
    fn stats_keep_their_wire_bytes() {
        let full = HostMessage::Stats(StatsRecord {
            capture_fps: 59.5,
            pace_fps: 60,
            target_fps: 50,
            fps_cap: 45,
            bitrate: 4_000_000,
            encoder_bitrate: 3_800_000,
            sent_bitrate: 3_900_000,
            rtt_ms: Some(40),
            base_rtt_ms: Some(35),
            queue_delay_ms: Some(2),
            ack_queue_ms: 1,
            encoder_dwell_ms: Some(3),
            dropped_frames: 4,
            skipped_ticks: 5,
            keyframes: 6,
            frames: 60,
            bytes: 500_000,
            adaptation: Adaptation::LoweringFps,
            rate_model: RateModel::Timestamp,
            gop: u32::MAX,
        });
        let sparse = HostMessage::Stats(StatsRecord {
            capture_fps: 0.0,
            pace_fps: 1,
            target_fps: 2,
            fps_cap: 0,
            bitrate: 3,
            encoder_bitrate: 4,
            sent_bitrate: 5,
            rtt_ms: None,
            base_rtt_ms: None,
            queue_delay_ms: None,
            ack_queue_ms: 6,
            encoder_dwell_ms: None,
            dropped_frames: 7,
            skipped_ticks: 8,
            keyframes: 9,
            frames: 10,
            bytes: 11,
            adaptation: Adaptation::Startup,
            rate_model: RateModel::Nominal,
            gop: 0,
        });
        for (message, golden) in [
            (
                full,
                r#"{"t":"stats","capture_fps":59.5,"pace_fps":60,"target_fps":50,"fps_cap":45,"bitrate":4000000,"encoder_bitrate":3800000,"sent_bitrate":3900000,"rtt_ms":40,"base_rtt_ms":35,"queue_delay_ms":2,"ack_queue_ms":1,"encoder_dwell_ms":3,"dropped_frames":4,"skipped_ticks":5,"keyframes":6,"frames":60,"bytes":500000,"adaptation":"lowering_fps","rate_model":"timestamp","gop":4294967295}"#,
            ),
            (
                sparse,
                r#"{"t":"stats","capture_fps":0.0,"pace_fps":1,"target_fps":2,"fps_cap":0,"bitrate":3,"encoder_bitrate":4,"sent_bitrate":5,"ack_queue_ms":6,"dropped_frames":7,"skipped_ticks":8,"keyframes":9,"frames":10,"bytes":11,"adaptation":"startup","rate_model":"nominal","gop":0}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&message).unwrap(), golden);
            let read: HostMessage = serde_json::from_str(golden).unwrap();
            assert_eq!(serde_json::to_string(&read).unwrap(), golden);
        }
    }
}
