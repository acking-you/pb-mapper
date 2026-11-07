//! G1 only, deleted at G2: the unchanged app on the new core. The old Dart
//! still sends the v1 op shapes and reads frames as owned buffers of a legacy
//! status plus the encoded bytes, which its isolate hands to the Windows
//! runner, the Android decoder or the JPEG painter. This module answers both
//! from the v2 viewer: an internal consumer pulls the session's video, and
//! the old ops map onto the viewer's calls.
use super::{
    DesktopState, Target, background_enabled, connect, disconnected, enable_host, err, input_error,
};
use crate::{
    desktop_video,
    error::{CtlError, ErrorCode},
    handle::PbMapperHandle,
};
use pb_mapper::desktop::{
    self, CursorPosition, DesktopKind, DesktopView, HostStatus, Invitation, KeyframeReason, Pull,
    QualityPreset, SessionStatus, VideoCodec, VideoFormat, VideoLimits, VideoUnit, Viewer,
    ViewerConfig, ViewerReason, ViewerState,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// The largest unit the wire carries.
const MAX_PAYLOAD: usize = 4 * 1024 * 1024;
/// How long `pb_mapper_desktop_wait_frame` waits: the old worker's bound.
const WAIT_MS: u32 = 16;

/// The old Dart's view of the current session's video.
pub(super) struct Frames {
    consumer: u64,
    /// The last unit handed out, which the legacy status describes.
    shown: Option<Shown>,
    /// The cursor the old Dart last saw.
    cursor_seq: u64,
}
#[derive(Clone)]
struct Shown {
    generation: u64,
    frame_id: u64,
    format: VideoFormat,
}

/// The v1 shapes the old Dart sends.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Status,
    Control {
        enabled: bool,
    },
    Release,
    Keyframe,
    Presented {
        generation: u64,
        frame_id: u64,
    },
    Input {
        event: desktop::InputEvent,
    },
    Connect {
        invitation: String,
        #[serde(default)]
        relay_only: bool,
        #[serde(default)]
        native_video: bool,
        #[serde(default)]
        video: Option<desktop::VideoPreferences>,
    },
    HostEnable {
        service: String,
        display_id: u32,
    },
}

/// Whether `request` is an old shape. Consulted before the lifecycle
/// `Request` is parsed, whose `deny_unknown_fields` would refuse it: the ops
/// that left the lifecycle export, and a `connect` without `refresh_hz` or a
/// `host_enable` without `allow_control`, which the new Dart always sends.
pub(super) fn routes(request: &Value) -> bool {
    match request["op"].as_str() {
        Some("status" | "control" | "release" | "keyframe" | "presented" | "input") => true,
        Some("connect") => request.get("refresh_hz").is_none(),
        Some("host_enable") => request.get("allow_control").is_none(),
        _ => false,
    }
}

pub(super) fn call(handle: &PbMapperHandle, request: Value) -> Result<Value, CtlError> {
    let request: Request = serde_json::from_value(request)
        .map_err(|_| CtlError::invalid_argument("invalid desktop request"))?;
    let state = &handle.desktop;
    let viewer = || state.viewer().ok_or_else(disconnected);
    match request {
        Request::Status => return Ok(status(handle)),
        Request::Control { enabled } => viewer()?.request_control(enabled).map_err(err)?,
        Request::Release => viewer()?.release_input().map_err(err)?,
        Request::Keyframe => {
            viewer()?.request_keyframe(KeyframeReason::User);
        }
        Request::Presented {
            generation,
            frame_id,
        } => {
            let consumer = state.shim.lock().as_ref().map(|frames| frames.consumer);
            if let Some(consumer) = consumer {
                desktop_video::presented(consumer, generation, frame_id);
            }
        }
        // Rust stamps the presented frame and the sequence now.
        Request::Input { event } => viewer()?
            .input(event.generation, &[event.input])
            .map_err(input_error)?,
        Request::Connect {
            invitation,
            relay_only,
            native_video,
            video,
        } => {
            // The old Dart ends its retries only on INVALID_ARGUMENT.
            let invitation = Invitation::decode(&invitation)
                .map_err(|failure| CtlError::invalid_argument(failure.to_string()))?;
            let video = video.unwrap_or_default();
            let codec = if native_video {
                VideoCodec::H264
            } else {
                VideoCodec::Jpeg
            };
            let config = |progress| ViewerConfig {
                codecs: vec![codec],
                limits: VideoLimits {
                    max_width: video.max_width,
                    max_height: video.max_height,
                    max_fps: video.max_fps,
                },
                quality: QualityPreset::Balanced,
                progress: Some(progress),
            };
            let (_, source) = handle
                .runtime
                .block_on(connect(
                    handle,
                    Target::Invitation(invitation),
                    relay_only,
                    config,
                ))
                .map_err(legacy_error)?;
            attach(state, source)?;
        }
        // Allowing control, as the one-click setup always did.
        Request::HostEnable {
            service,
            display_id,
        } => {
            return handle
                .runtime
                .block_on(enable_host(handle, service, display_id, true, true));
        }
    }
    Ok(json!({}))
}

/// The old store stops retrying a pin failure only on this phrase.
fn legacy_error(failure: CtlError) -> CtlError {
    if failure.code() == ErrorCode::IdentityChanged {
        return CtlError::new(
            ErrorCode::IdentityChanged,
            format!("desktop identity changed: {failure}"),
        );
    }
    failure
}

/// The internal consumer: it holds the source's one attachment.
fn attach(state: &DesktopState, source: u64) -> Result<(), CtlError> {
    let consumer = u64::try_from(desktop_video::attach(source))
        .ok()
        .filter(|&consumer| consumer > 0)
        .ok_or_else(|| err("desktop connection cancelled"))?;
    *state.shim.lock() = Some(Frames {
        consumer,
        shown: None,
        cursor_seq: 0,
    });
    Ok(())
}

/// `{host, viewer, background}` in the legacy status shape.
fn status(handle: &PbMapperHandle) -> Value {
    let state = &handle.desktop;
    let background = state.background_status(handle.runtime.handle());
    // A slow helper management call holds the host lifecycle lock, but must
    // never delay the independent remote viewer's status.
    let host = state
        .host
        .try_lock()
        .ok()
        .and_then(|slot| {
            slot.as_ref()
                .map(|host| json!(desktop::legacy_host_status(&host.status())))
        })
        .or_else(|| background.as_ref().and_then(background_host));
    let viewer = state.viewer().map(|viewer| {
        let shown = state
            .shim
            .lock()
            .as_ref()
            .and_then(|frames| frames.shown.clone());
        legacy_status(&viewer, shown)
    });
    json!({"host": host, "viewer": viewer, "background": background})
}

fn background_host(snapshot: &Value) -> Option<Value> {
    if !background_enabled(snapshot) {
        return None;
    }
    // The helper's IPC v2 reports a `HostStatus`, its v1 the legacy shape.
    if let Ok(status) = serde_json::from_value::<HostStatus>(snapshot["host"].clone()) {
        return Some(json!(desktop::legacy_host_status(&status)));
    }
    if !snapshot["host"].is_null() {
        return Some(snapshot["host"].clone());
    }
    // The service may be waiting for a console session or retrying the relay.
    // It still owns an enabled grant, so the global Stop action must stay live.
    Some(json!(SessionStatus {
        state: "waiting".into(),
        message: snapshot["message"].as_str().unwrap_or_default().into(),
        control_allowed: snapshot["settings"]["allow_control"]
            .as_bool()
            .unwrap_or(false),
        ..Default::default()
    }))
}

/// The per-frame status the old Dart reads: the viewer's status, the cursor
/// from its view, and the last unit handed out.
fn legacy_status(viewer: &Viewer, shown: Option<Shown>) -> SessionStatus {
    let status = viewer.status();
    let view = viewer.view();
    // The v1 host put this prefix on input failures; the old store drops its
    // wish for control on it.
    let message = if status.reason == Some(ViewerReason::InputFailed) {
        format!("input_failed: {}", status.message)
    } else {
        status.message
    };
    SessionStatus {
        state: match status.state {
            ViewerState::Connecting => "connecting",
            ViewerState::Streaming => "streaming",
            ViewerState::Paused => "paused",
            ViewerState::CaptureUnavailable => "capture_unavailable",
            ViewerState::Stopped => "stopped",
        }
        .into(),
        message,
        control_allowed: status.control_allowed,
        controlling: status.controlling,
        generation: status.generation,
        frame_id: shown.as_ref().map_or(0, |shown| shown.frame_id),
        display: status.display,
        path: status.stats.path,
        desktop: match status.desktop {
            DesktopKind::Ordinary => "ordinary",
            DesktopKind::Secure => "secure",
        }
        .into(),
        video: shown.map(|shown| shown.format).unwrap_or_default(),
        capture_fps: status.stats.capture_fps,
        receive_fps: status.stats.received_fps,
        dropped_frames: status.stats.dropped_frames,
        bitrate: status.stats.received_bitrate,
        cursor: Some(CursorPosition {
            x: view.cursor_x,
            y: view.cursor_y,
            visible: view.flags & DesktopView::FLAG_CURSOR_VISIBLE != 0,
        }),
        frame_ack_ms: status.stats.picture_latency_ms.map_or(0.0, |ms| ms as f32),
        ..SessionStatus::default()
    }
}

/// The next unit with its legacy status. Without one, a status alone when
/// the cursor moved, as the v1 host sent it, so the old viewer's pointer
/// follows a still picture.
fn next_frame(handle: &PbMapperHandle, wait_ms: u32) -> Option<(SessionStatus, Option<VideoUnit>)> {
    let state = &handle.desktop;
    let viewer = state.viewer()?;
    let consumer = state.shim.lock().as_ref()?.consumer;
    let unit = match desktop_video::next(consumer, wait_ms, MAX_PAYLOAD).ok()? {
        Pull::Unit(unit) => Some(unit),
        Pull::Timeout | Pull::Paused => None,
        Pull::NeedCapacity(_) | Pull::Closed => return None,
    };
    let view = viewer.view();
    let shown = {
        let mut frames = state.shim.lock();
        let frames = frames
            .as_mut()
            .filter(|frames| frames.consumer == consumer)?;
        match &unit {
            Some(unit) => frames.shown = Some(describe(unit)),
            None if view.cursor_seq == frames.cursor_seq
                || view.state != DesktopView::STATE_STREAMING
                || frames
                    .shown
                    .as_ref()
                    .is_none_or(|shown| shown.generation != view.generation) =>
            {
                return None;
            }
            None => {}
        }
        frames.cursor_seq = view.cursor_seq;
        frames.shown.clone()
    };
    Some((legacy_status(&viewer, shown), unit))
}

fn describe(unit: &VideoUnit) -> Shown {
    Shown {
        generation: unit.generation,
        frame_id: unit.frame_id,
        format: VideoFormat {
            codec: unit.codec,
            width: unit.width,
            height: unit.height,
            keyframe: unit.keyframe,
            pts_us: unit.pts_us,
            duplicate: unit.duplicate,
            encode_us: 0,
        },
    }
}

/// One allocation containing u32 little-endian metadata length, UTF-8 status,
/// and encoded video bytes. Media is never encoded as JSON/base64. A null
/// pointer means no newer frame; free every non-null returned pointer.
#[repr(C)]
pub struct DesktopBuffer {
    pub data: *const u8,
    pub len: usize,
}

/// # Safety
/// `handle` must be null or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_frame(
    handle: *mut PbMapperHandle,
    _after: u64,
    _generation: u64,
) -> *mut DesktopBuffer {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    let handle = unsafe { &*handle };
    desktop_video::guard(std::ptr::null_mut(), || export_frame(next_frame(handle, 0)))
}

/// Bounded wait variant for a dedicated media worker. The buffer ownership is
/// identical to `pb_mapper_desktop_frame`; never call this on the UI thread.
///
/// # Safety
/// `handle` must be null or a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_wait_frame(
    handle: *mut PbMapperHandle,
    _after: u64,
    _generation: u64,
) -> *mut DesktopBuffer {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    let handle = unsafe { &*handle };
    desktop_video::guard(std::ptr::null_mut(), || {
        export_frame(next_frame(handle, WAIT_MS))
    })
}

fn export_frame(frame: Option<(SessionStatus, Option<VideoUnit>)>) -> *mut DesktopBuffer {
    let Some((status, unit)) = frame else {
        return std::ptr::null_mut();
    };
    let Ok(metadata) = serde_json::to_vec(&status) else {
        return std::ptr::null_mut();
    };
    // A status alone, for a cursor that moved, carries no bytes.
    let bytes = unit.as_ref().map_or(&[][..], |unit| &unit.bytes[..]);
    let mut data = Vec::with_capacity(4 + metadata.len() + bytes.len());
    data.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    data.extend_from_slice(&metadata);
    data.extend_from_slice(bytes);
    let data = data.into_boxed_slice();
    let buffer = DesktopBuffer {
        len: data.len(),
        data: Box::into_raw(data).cast::<u8>(),
    };
    Box::into_raw(Box::new(buffer))
}

/// # Safety
/// `buffer` must be null or a pointer a frame export returned, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_free_frame(buffer: *mut DesktopBuffer) {
    if buffer.is_null() {
        return;
    }
    let buffer = unsafe { Box::from_raw(buffer) };
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            buffer.data.cast_mut(),
            buffer.len,
        )));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::tests::{call_json, invitation, scripted_viewer, send, test_handle};
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn shim_routes_old_and_new_connect_shapes() {
        let handle = test_handle("shim-routes");
        let handle = &handle;
        // Garbage tells the two handlers apart before any network: the shim
        // keeps the old terminal code, the new op names the problem.
        let old = call_json(
            handle,
            json!({"op": "connect", "invitation": "garbage", "relay_only": false,
                "native_video": true, "video": {"max_width": 1920, "max_height": 1080,
                "max_fps": 60, "bitrate": 12_000_000}}),
        );
        assert_eq!(
            (&old["code"], &old["message"]),
            (
                &json!("INVALID_ARGUMENT"),
                &json!("invalid desktop invitation")
            )
        );
        let new = call_json(
            handle,
            json!({"op": "connect", "invitation": "garbage", "relay_only": false,
                "quality": "balanced", "refresh_hz": 60.0}),
        );
        assert_eq!(new["code"], "INVALID_INVITATION");
        let unknown = call_json(
            handle,
            json!({"op": "connect", "invitation": "garbage", "native_video": true, "bogus": 1}),
        );
        assert_eq!(unknown["message"], "invalid desktop request");
        assert_eq!(
            call_json(handle, json!({"op": "status"}))["data"],
            json!({"host": null, "viewer": null, "background": null})
        );
        assert_eq!(
            call_json(handle, json!({"op": "host_status"}))["data"],
            json!({"host": null, "background": null})
        );

        // An old session: frames carry the legacy status, and the old
        // presentation op reaches the sink.
        let mut host = handle.runtime.block_on(async {
            let (viewer, host) = scripted_viewer().await;
            let (_, source) = handle
                .desktop
                .connect_viewer(|_| async { Ok((viewer, invitation())) })
                .await
                .unwrap();
            attach(&handle.desktop, source).unwrap();
            host
        });
        handle.runtime.block_on(async {
            send(
                &mut host,
                json!({"t": "video", "seq": 1, "generation": 1, "frame_id": 7, "keyframe": true,
                    "duplicate": false, "width": 1920, "height": 1080, "pts_us": 5, "host_us": 0}),
            )
            .await;
            host.write_u32(3).await.unwrap();
            host.write_all(&[1, 2, 3]).await.unwrap();
        });
        let buffer = std::iter::repeat_with(|| unsafe {
            pb_mapper_desktop_wait_frame(std::ptr::from_ref(handle).cast_mut(), 0, 0)
        })
        .take(100)
        .find(|buffer| !buffer.is_null())
        .unwrap();
        let presented = json!({"op": "presented", "generation": 1, "frame_id": 7});
        assert_eq!(call_json(handle, presented)["success"], true);
        let status = call_json(handle, json!({"op": "status"}));
        assert_eq!(status["data"]["viewer"]["state"], "streaming");
        assert_eq!(status["data"]["viewer"]["frame_id"], 7);
        assert_eq!(
            handle
                .desktop
                .viewer()
                .unwrap()
                .status()
                .presented
                .unwrap()
                .frame_id,
            7
        );

        // The buffer stays the caller's after a disconnect, until freed.
        drop(host);
        assert_eq!(
            call_json(handle, json!({"op": "disconnect"}))["success"],
            true
        );
        assert!(
            unsafe { pb_mapper_desktop_wait_frame(std::ptr::from_ref(handle).cast_mut(), 0, 0) }
                .is_null()
        );
        let bytes = unsafe { std::slice::from_raw_parts((*buffer).data, (*buffer).len) };
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let metadata: Value = serde_json::from_slice(&bytes[4..4 + length]).unwrap();
        assert_eq!(
            (
                &metadata["frame_id"],
                &metadata["video"]["codec"],
                &metadata["display"]["width"]
            ),
            (&json!(7), &json!("h264"), &json!(1920))
        );
        assert_eq!(&bytes[4 + length..], &[1, 2, 3]);
        unsafe { pb_mapper_desktop_free_frame(buffer) };
    }
}
