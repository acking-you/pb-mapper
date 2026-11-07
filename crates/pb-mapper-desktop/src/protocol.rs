//! Desktop protocol v2 (ALPN `pb-mapper/desktop/2`): one JSON record per
//! message; only `HostMessage::Video` is followed by one payload record
//! (`wire.rs`). Every message and struct rejects unknown fields, and every enum
//! variant is a struct variant, because serde ignores extra fields on an
//! internally tagged unit variant. A v2.x peer sends a new message, field or
//! value only after the other side listed the matching capability.
use crate::{Display, InputEvent, KeyframeReason, QualityPreset, VideoCodec, VideoLimits};
use serde::{Deserialize, Serialize};

/// The host captures and injects on a Windows backend that can lock it.
pub(crate) const CAPABILITY_LOCK: &str = "lock";
/// The privileged helper is active, so the secure desktop is shared too.
pub(crate) const CAPABILITY_SECURE_DESKTOP: &str = "secure_desktop";
const MAX_CAPABILITIES: usize = 16;

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
        /// One or two codecs, in the viewer's preference order.
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
    State {
        state: SessionState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<StateReason>,
        /// Human text; a UI keys off `reason`.
        message: String,
        control_allowed: bool,
        controlling: bool,
        generation: u64,
        /// Physical pixels: the input space.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<Display>,
        desktop: DesktopKind,
        /// The encoded size of this generation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video: Option<VideoSize>,
    },
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
    },
    /// 1, 2, 3 …; one outstanding at a time, in stream order.
    Probe {
        nonce: u64,
    },
    /// Every `STATS_INTERVAL`; also the host-to-viewer liveness.
    Stats {
        capture_fps: f32,
        pace_fps: u32,
        target_fps: u32,
        /// 0 = no viewer cap.
        fps_cap: u32,
        /// Expected: bits per frame × pace.
        bitrate: u32,
        encoder_bitrate: u32,
        sent_bitrate: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rtt_ms: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_rtt_ms: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_delay_ms: Option<u32>,
        ack_queue_ms: u32,
        dropped_frames: u64,
        skipped_ticks: u64,
        keyframes: u64,
        frames: u64,
        bytes: u64,
        adaptation: Adaptation,
        rate_model: RateModel,
        gop: u32,
    },
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adaptation {
    Startup,
    Steady,
    Raising,
    LoweringFps,
    LoweringBitrate,
    Unresponsive,
    Paused,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateModel {
    Nominal,
    Timestamp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeCode {
    Locked,
    LockFailed,
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{Button, Input, Key, security};
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
    fn strict_parsing_and_version_gate() {
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
            ViewerMessage::Ping {},
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
                capabilities: vec![CAPABILITY_LOCK.into(), CAPABILITY_SECURE_DESKTOP.into()],
            },
            HostMessage::Rejected {
                reason: RejectReason::Busy,
            },
            HostMessage::State {
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
            },
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
            },
            HostMessage::Probe { nonce: 2 },
            HostMessage::Stats {
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
                dropped_frames: 0,
                skipped_ticks: 1,
                keyframes: 1,
                frames: 60,
                bytes: 500_000,
                adaptation: Adaptation::Steady,
                rate_model: RateModel::Nominal,
                gop: u32::MAX,
            },
            HostMessage::Notice {
                code: NoticeCode::LockFailed,
                message: "access denied".into(),
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
        assert!(valid_capabilities(&[
            "lock".into(),
            "secure_desktop".into()
        ]));
        assert!(!valid_capabilities(&["Lock".into()]));
        assert!(!valid_capabilities(&vec!["a".into(); MAX_CAPABILITIES + 1]));

        // A v1 peer fails the TLS handshake with a typed version mismatch,
        // whichever side is the old one.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for old_host in [true, false] {
                let failure = security::tests::alpn_handshake(old_host).await;
                assert_eq!(
                    security::handshake_failure(&failure),
                    Some(security::HandshakeFailure::VersionMismatch),
                    "{failure}"
                );
            }
        });
    }
}
