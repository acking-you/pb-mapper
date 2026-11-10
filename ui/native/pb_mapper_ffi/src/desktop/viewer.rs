//! The viewer: its connect, its state machine and the two hot exports.
use super::{DesktopState, err, valid_max_fps};
use crate::{
    desktop_video,
    error::{CtlError, ErrorCode},
    handle::PbMapperHandle,
    response::{err_ctl, err_null_handle, ok_data, parse_c_string},
};
use pb_mapper::{
    ConnectOptions, PathPolicy,
    desktop::{
        self, ConnectError, ConnectStage, Decoders, DesktopView, DisplayModeError, InputError,
        Invitation, InvitationError, InvitationFormat, KeyframeReason, Mode, ModeTarget,
        QualityPreset, Resolution, Viewer, ViewerConfig,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::c_char;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio_util::sync::CancellationToken;

/// The longest hot request: a batch of 16 inputs, text included.
const MAX_VIEWER_REQUEST: usize = 64 * 1024;

/// The connected viewer, published once its host accepted.
pub(super) struct Live {
    viewer: Arc<Viewer>,
    /// Its video source in the registry.
    pub(super) source: u64,
    /// What remembering this computer stores.
    connected: Connected,
}

/// What a viewer connected to, and the relay scope of the attempt that
/// connected it: what remembering the computer stores. A remembered computer
/// belongs to the relay and credential its session went through, not to the
/// settings as they are when it is remembered.
#[derive(Clone)]
pub(crate) struct Connected {
    /// The invitation that passed the certificate pin.
    pub(crate) invitation: Invitation,
    /// The relay address the attempt's relay client was made for.
    pub(crate) relay: String,
    /// The namespace that client's credential owns: a temporary key's id, 0
    /// for the administrator key.
    pub(crate) namespace: u64,
}

/// A connect between its registration and its outcome.
pub(super) struct PendingConnect {
    epoch: u64,
    cancel: CancellationToken,
    /// The last stage the attempt passed, as `stage_code` writes it.
    stage: Arc<AtomicU8>,
    /// The app hid the viewer meanwhile, so the session starts paused.
    paused: bool,
}

/// A registered connect until it settles. Dropped before that, its future
/// abandoned, it unregisters the attempt, unless a newer one took its place,
/// so a later connect is not refused as already pending.
struct Registration<'a> {
    state: &'a DesktopState,
    epoch: u64,
    /// The attempt settled, and took its registration with it.
    settled: bool,
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let mut pending = self.state.connecting.lock();
        if pending
            .as_ref()
            .is_some_and(|attempt| attempt.epoch == self.epoch)
        {
            *pending = None;
        }
    }
}

impl DesktopState {
    /// The live viewer's computer and the relay scope it was reached through.
    pub(crate) fn connected(&self) -> Option<Connected> {
        self.viewer
            .read()
            .as_ref()
            .map(|live| live.connected.clone())
    }

    fn viewer(&self) -> Option<Arc<Viewer>> {
        self.viewer.read().as_ref().map(|live| live.viewer.clone())
    }

    /// Registers before any await, so a disconnect cancels the attempt even
    /// before network work starts. `connect` gets the stage cell its progress
    /// writes. The viewer is published with its video source unless a
    /// disconnect came first.
    pub(super) async fn connect_viewer<F>(
        &self,
        connect: impl FnOnce(Arc<AtomicU8>) -> F,
    ) -> Result<(Arc<Viewer>, u64), CtlError>
    where
        F: std::future::Future<Output = Result<(Viewer, Connected), CtlError>>,
    {
        let cancel = CancellationToken::new();
        let stage = Arc::new(AtomicU8::new(STAGE_NONE));
        let epoch = {
            let mut pending = self.connecting.lock();
            if pending.is_some() {
                return Err(err("desktop connection is already pending"));
            }
            let epoch = self.viewer_epoch.fetch_add(1, Ordering::AcqRel) + 1;
            *pending = Some(PendingConnect {
                epoch,
                cancel: cancel.clone(),
                stage: stage.clone(),
                paused: false,
            });
            epoch
        };
        let mut registration = Registration {
            state: self,
            epoch,
            settled: false,
        };
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(connect_error(ConnectError::Cancelled)),
            result = async {
                if self.viewer.read().is_some() {
                    return Err(err("disconnect the current desktop first"));
                }
                connect(stage).await
            } => result,
        };
        // Settle under both locks, so a hot op sees either the attempt or the
        // published viewer, and a pause in between is not lost.
        let refused = {
            let mut pending = self.connecting.lock();
            let paused = if pending
                .as_ref()
                .is_some_and(|attempt| attempt.epoch == epoch)
            {
                pending.take().is_some_and(|attempt| attempt.paused)
            } else {
                false
            };
            registration.settled = true;
            let (viewer, connected) = result?;
            let mut slot = self.viewer.write();
            if cancel.is_cancelled()
                || self.viewer_epoch.load(Ordering::Acquire) != epoch
                || slot.is_some()
            {
                viewer
            } else {
                if paused {
                    viewer.pause();
                }
                let viewer = Arc::new(viewer);
                let source = desktop_video::add_source(viewer.video_sink());
                *slot = Some(Live {
                    viewer: viewer.clone(),
                    source,
                    connected,
                });
                return Ok((viewer, source));
            }
        };
        refused.stop().await.map_err(err)?;
        Err(connect_error(ConnectError::Cancelled))
    }

    /// Cancels a pending connect, then unpublishes the viewer and closes its
    /// video source, and only then stops it, with no lock held: hot ops and
    /// native consumers never wait for a slow stop.
    pub(super) async fn disconnect_viewer(&self) -> Result<(), CtlError> {
        {
            let mut pending = self.connecting.lock();
            self.viewer_epoch.fetch_add(1, Ordering::AcqRel);
            if let Some(attempt) = pending.take() {
                attempt.cancel.cancel();
            }
        }
        let live = self.viewer.write().take();
        if let Some(live) = live {
            desktop_video::remove_source(live.source);
            live.viewer.stop().await.map_err(err)?;
        }
        Ok(())
    }

    /// `local`, this computer's display, adds the mode that fits it best to
    /// the remote display's modes, as `modes.fit_local`.
    pub(super) fn viewer_status(&self, local: Option<Mode>) -> Result<Value, CtlError> {
        // In the connect's order, so it sees the attempt or the viewer.
        let (viewer, stage) = {
            let pending = self.connecting.lock();
            let stage = pending
                .as_ref()
                .map(|attempt| stage_of(attempt.stage.load(Ordering::Acquire)));
            (self.viewer(), stage)
        };
        if let Some(viewer) = viewer {
            let status = viewer.status();
            let fit_local = status
                .modes
                .as_ref()
                .zip(local)
                .and_then(|(modes, local)| modes.fit_local(local));
            let mut value = serde_json::to_value(status)
                .map_err(|failure| CtlError::internal(failure.to_string()))?;
            if let Some(modes) = value.get_mut("modes").and_then(Value::as_object_mut) {
                modes.insert("fit_local".into(), json!(fit_local));
            }
            return Ok(value);
        }
        Ok(match stage {
            Some(stage) => json!({"state": "connecting", "stage": stage}),
            None => Value::Null,
        })
    }

    /// Pauses or resumes the viewer. A pending attempt remembers the choice
    /// for the viewer it publishes.
    fn set_paused(&self, paused: bool) {
        let mut pending = self.connecting.lock();
        match self.viewer() {
            Some(viewer) if paused => viewer.pause(),
            Some(viewer) => viewer.resume(),
            None => {
                if let Some(attempt) = pending.as_mut() {
                    attempt.paused = paused;
                }
            }
        }
    }
}

/// `ConnectStage` in the pending attempt's cell; 0 until the first stage.
const STAGE_NONE: u8 = 0;
pub(super) fn stage_code(stage: ConnectStage) -> u8 {
    match stage {
        ConnectStage::Path => 1,
        ConnectStage::Identity => 2,
        ConnectStage::Authorizing => 3,
    }
}
fn stage_of(code: u8) -> Option<ConnectStage> {
    match code {
        1 => Some(ConnectStage::Path),
        2 => Some(ConnectStage::Identity),
        3 => Some(ConnectStage::Authorizing),
        _ => None,
    }
}

/// The `connect` op: exactly one of `invitation` and `computer_id`, a
/// remembered computer.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConnectRequest {
    #[serde(default)]
    pub(super) invitation: Option<String>,
    #[serde(default)]
    pub(super) computer_id: Option<String>,
    #[serde(default)]
    pub(super) relay_only: bool,
    #[serde(default)]
    pub(super) quality: QualityPreset,
    pub(super) refresh_hz: f64,
    /// What the runner decodes in hardware, as its probe found it, H.265
    /// left out once it failed this run: the offer, and the largest
    /// picture of each decoder, which bounds the limits.
    pub(super) decoders: DecoderOffer,
    /// Absent: the SDK's default, the display's own size.
    #[serde(default)]
    pub(super) resolution: Option<Resolution>,
    /// 0 is Auto; otherwise the user's own rate, see `valid_max_fps`.
    #[serde(default)]
    pub(super) max_fps: u32,
}

/// Each decoder's largest picture as `[width, height]`; `null` for none.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DecoderOffer {
    pub(super) h264: Option<[u32; 2]>,
    pub(super) h265: Option<[u32; 2]>,
}
impl DecoderOffer {
    /// The decoders as offered. On Windows without Media Foundation (N and
    /// KN editions without the Media Feature Pack) the runner decodes
    /// nothing natively, whatever Dart passed (an unanswered probe
    /// assumes H.264): the picture is JPEG, which Dart decodes.
    fn decoders(&self) -> Result<Decoders, CtlError> {
        let size = |largest: Option<[u32; 2]>| match largest {
            Some([width, height]) if width == 0 || height == 0 => Err(CtlError::invalid_argument(
                "a decoder's largest picture must not be empty",
            )),
            largest => Ok(largest.map(|[width, height]| (width, height))),
        };
        let decoders = Decoders {
            h264: size(self.h264)?,
            h265: size(self.h265)?,
        };
        #[cfg(windows)]
        if !desktop::windows::media_foundation() {
            return Ok(Decoders::default());
        }
        Ok(decoders)
    }
}

/// The hot export's closed op set.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum ViewerRequest {
    ViewerStatus {
        #[serde(default)]
        local: Option<Mode>,
    },
    /// `"original"`, `{"fit_local": {"width", "height"}}` (this computer's
    /// display) or `{"size": {"width", "height"}}`.
    DisplayMode {
        target: DisplayModeTarget,
    },
    Control {
        enabled: bool,
    },
    Release,
    Input {
        generation: u64,
        inputs: Vec<desktop::Input>,
    },
    Keyframe,
    Quality {
        preset: QualityPreset,
    },
    Pause,
    Resume,
}

/// `ModeTarget` as the hot op takes it.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum DisplayModeTarget {
    Original,
    FitLocal(Mode),
    Size(Mode),
}
impl From<DisplayModeTarget> for ModeTarget {
    fn from(target: DisplayModeTarget) -> Self {
        match target {
            DisplayModeTarget::Original => Self::Original,
            DisplayModeTarget::FitLocal(local) => Self::FitLocal(local),
            DisplayModeTarget::Size(mode) => Self::Size(mode),
        }
    }
}

fn disconnected() -> CtlError {
    err("desktop is disconnected")
}

/// Each typed failure has its code; nothing is matched on text.
fn connect_error(failure: ConnectError) -> CtlError {
    let code = match &failure {
        ConnectError::IdentityChanged => ErrorCode::IdentityChanged,
        ConnectError::VersionMismatch => ErrorCode::VersionMismatch,
        ConnectError::Busy => ErrorCode::HostBusy,
        ConnectError::Denied => ErrorCode::AccessDenied,
        ConnectError::InvalidRequest => ErrorCode::InvalidArgument,
        ConnectError::Protocol(_) => ErrorCode::Protocol,
        ConnectError::Cancelled | ConnectError::Transport(_) => ErrorCode::Io,
    };
    CtlError::new(code, failure.to_string())
}

fn input_error(failure: InputError) -> CtlError {
    let code = match &failure {
        InputError::NotFresh => ErrorCode::NotFresh,
        InputError::NotControlling => ErrorCode::NotControlling,
        InputError::Congestion => ErrorCode::InputCongestion,
        InputError::Invalid(_) => ErrorCode::InvalidArgument,
        InputError::Closed => ErrorCode::Io,
    };
    CtlError::new(code, failure.to_string())
}

fn display_mode_error(failure: DisplayModeError) -> CtlError {
    let (code, message) = match failure {
        DisplayModeError::NotOffered => (
            ErrorCode::DisplayModeNotOffered,
            "the remote computer cannot change its resolution",
        ),
        DisplayModeError::NotControlling => (
            ErrorCode::NotControlling,
            "control of the remote computer is needed to change its resolution",
        ),
        DisplayModeError::NoFit => (
            ErrorCode::DisplayModeNoFit,
            "no resolution of the remote display fits this display",
        ),
        DisplayModeError::NotListed => (
            ErrorCode::DisplayModeNotListed,
            "the remote display does not offer that resolution",
        ),
        // As every hot op of a closed session.
        _ => (ErrorCode::Io, "desktop is disconnected"),
    };
    CtlError::new(code, message)
}

pub(super) fn invitation_error(failure: InvitationError) -> CtlError {
    let code = match failure {
        InvitationError::SetupString => ErrorCode::SetupString,
        _ => ErrorCode::InvalidInvitation,
    };
    CtlError::new(code, failure.to_string())
}

/// What `inspect_invitation` shows of an invitation: never its token.
pub(super) fn inspection(invitation: &Invitation) -> Value {
    let service = &invitation.service;
    json!({
        "service": service,
        "name": service.strip_prefix("desktop:").unwrap_or(service),
        "fingerprint": invitation.short_fingerprint(),
        "format": match invitation.format() {
            InvitationFormat::Pbd1 => "pbd1",
            InvitationFormat::JsonV1 => "json_v1",
        },
    })
}

/// The `connect` op: the viewer and its video source, published once its
/// host accepted.
pub(super) async fn connect_request(
    handle: &PbMapperHandle,
    request: ConnectRequest,
) -> Result<Value, CtlError> {
    let ConnectRequest {
        invitation,
        computer_id,
        relay_only,
        quality,
        refresh_hz,
        decoders,
        resolution,
        max_fps,
    } = request;
    if !valid_max_fps(max_fps) {
        return Err(CtlError::invalid_argument(
            "max_fps must be 0 (Auto) or 30..=120",
        ));
    }
    let decoders = decoders.decoders()?;
    // A pasted invitation is checked before anything waits; a
    // remembered one is read inside the cancellable attempt.
    let target = match (invitation, computer_id) {
        (Some(text), None) => {
            Target::Invitation(Invitation::decode(&text).map_err(invitation_error)?)
        }
        (None, Some(id)) => Target::Remembered(id),
        _ => {
            return Err(CtlError::invalid_argument(
                "connect takes exactly one of an invitation and a computer id",
            ));
        }
    };
    // The runner's own decoders, which Dart probed: none where it
    // has no native video, and Dart decodes JPEG.
    let chosen_fps = (max_fps != 0).then_some(max_fps);
    let (viewer, source) = connect(handle, target, relay_only, |progress| ViewerConfig {
        quality,
        progress: Some(progress),
        ..ViewerConfig::for_decoders(
            decoders,
            resolution.unwrap_or_default(),
            refresh_hz,
            chosen_fps,
        )
    })
    .await?;
    Ok(json!({"video_source": source, "codec": viewer.status().stats.codec}))
}

/// Whom `connect` reaches.
enum Target {
    Invitation(Invitation),
    /// A remembered computer's id, resolved in Rust: its token never
    /// reaches Dart.
    Remembered(String),
}

/// Resolves the target and the relay inside the registered, cancellable
/// attempt, then connects with `config`, whose progress callback reports the
/// stage to `viewer_status`.
async fn connect(
    handle: &PbMapperHandle,
    target: Target,
    relay_only: bool,
    config: impl FnOnce(Arc<dyn Fn(ConnectStage) + Send + Sync>) -> ViewerConfig,
) -> Result<(Arc<Viewer>, u64), CtlError> {
    handle
        .desktop
        .connect_viewer(|stage| async move {
            let invitation = match target {
                Target::Invitation(invitation) => invitation,
                Target::Remembered(id) => crate::desktop_computers::invitation_for(handle, &id)
                    .await?
                    .ok_or_else(|| CtlError::not_found("remembered computer not found"))?,
            };
            // Configuration can wait behind other app operations. Keep that
            // wait inside the registered, cancellable attempt.
            let (client, app) = {
                let state = handle.state.lock().await;
                (state.relay_client()?, state.get_config_status())
            };
            let options = ConnectOptions {
                path: if relay_only {
                    PathPolicy::RelayOnly
                } else {
                    PathPolicy::Auto
                },
                lan_first: app.lan_first,
                lan_transport: app.lan_transport,
                ..Default::default()
            };
            let progress = Arc::new(move |next: ConnectStage| {
                stage.store(stage_code(next), Ordering::Release);
            });
            let viewer = Viewer::connect(&client, &invitation, options, config(progress))
                .await
                .map_err(|failure| {
                    tracing::warn!(event = "desktop_connect_failed", failure = ?failure);
                    connect_error(failure)
                })?;
            // The scope this attempt's client was made for, read under the
            // same lock: a later change of the settings is not this session's.
            let connected = Connected {
                invitation,
                relay: app.server_address,
                namespace: client.credential_namespace(),
            };
            Ok((viewer, connected))
        })
        .await
}

/// The viewer's hot ops. Synchronous: never `block_on`, never a tokio lock,
/// never an await, so safe on the Dart UI isolate. A closed op set; anything
/// else is `INVALID_ARGUMENT`. Free the reply with `pb_mapper_free_string`.
///
/// # Safety
/// `handle` must be null or a live handle, and `request` null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_viewer_call_json(
    handle: *mut PbMapperHandle,
    request: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }
    let handle = unsafe { &*handle };
    let failed = || CtlError::internal("desktop viewer call failed");
    let result = desktop_video::guard(Err(failed()), || {
        // SAFETY: null or NUL-terminated, as the caller guarantees.
        let text = unsafe { parse_c_string(request, "request") }?;
        if text.len() > MAX_VIEWER_REQUEST {
            return Err(CtlError::invalid_argument(
                "desktop viewer request too large",
            ));
        }
        let request = serde_json::from_str(&text)
            .map_err(|_| CtlError::invalid_argument("invalid desktop viewer request"))?;
        viewer_call(&handle.desktop, request)
    });
    match result {
        Ok(value) => ok_data(value),
        Err(e) => err_ctl(&e),
    }
}

/// Without a viewer, giving up control, releasing, pausing and choosing a
/// preset have nothing to do; asking for control or sending input fail.
fn viewer_call(state: &DesktopState, request: ViewerRequest) -> Result<Value, CtlError> {
    let viewer = state.viewer();
    match request {
        ViewerRequest::ViewerStatus { local } => return state.viewer_status(local),
        ViewerRequest::DisplayMode { target } => {
            let mode = viewer
                .ok_or_else(disconnected)?
                .set_display_mode(target.into())
                .map_err(display_mode_error)?;
            return Ok(json!({ "mode": mode }));
        }
        ViewerRequest::Control { enabled } => match viewer {
            Some(viewer) => viewer.request_control(enabled).map_err(err)?,
            None if enabled => return Err(disconnected()),
            None => {}
        },
        ViewerRequest::Release => {
            if let Some(viewer) = viewer {
                viewer.release_input().map_err(err)?;
            }
        }
        ViewerRequest::Input { generation, inputs } => viewer
            .ok_or_else(disconnected)?
            .input(generation, &inputs)
            .map_err(input_error)?,
        ViewerRequest::Keyframe => {
            let requested =
                viewer.is_some_and(|viewer| viewer.request_keyframe(KeyframeReason::User));
            return Ok(json!({"requested": requested}));
        }
        ViewerRequest::Quality { preset } => {
            if let Some(viewer) = viewer {
                viewer.set_quality(preset);
            }
        }
        ViewerRequest::Pause => state.set_paused(true),
        ViewerRequest::Resume => state.set_paused(false),
    }
    Ok(json!({}))
}

/// What the UI isolate reads every frame: `DesktopView` in a fixed layout.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PbDesktopViewV1 {
    pub generation: u64,
    /// 0 = none.
    pub presented_frame_id: u64,
    /// Advances with every new cursor position.
    pub cursor_seq: u64,
    /// `u32::MAX` = none.
    pub presented_age_ms: u32,
    /// Physical pixels of `generation`: the input space.
    pub display_width: u32,
    pub display_height: u32,
    /// The encoded size.
    pub video_width: u32,
    pub video_height: u32,
    /// Physical pixels relative to the display.
    pub cursor_x: i32,
    pub cursor_y: i32,
    /// 0 none, 1 connecting, 2 streaming, 3 capture_unavailable, 4 paused,
    /// 5 stopped.
    pub state: u8,
    /// Bit 0 cursor_visible, 1 controlling, 2 control_allowed,
    /// 3 secure_desktop, 4 fresh, 5 paused.
    pub flags: u8,
    pub reserved: [u8; 2],
}
// The layout of perf 7.4, field by field: `From` fills it by name, so a
// same-size reorder would otherwise compile. desktop_ffi_test.dart pins Dart's
// mirror to the same offsets.
const _: () = {
    use std::mem::offset_of;
    type V = PbDesktopViewV1;
    assert!(size_of::<V>() == 56 && align_of::<V>() == 8);
    assert!(offset_of!(V, generation) == 0);
    assert!(offset_of!(V, presented_frame_id) == 8);
    assert!(offset_of!(V, cursor_seq) == 16);
    assert!(offset_of!(V, presented_age_ms) == 24);
    assert!(offset_of!(V, display_width) == 28);
    assert!(offset_of!(V, display_height) == 32);
    assert!(offset_of!(V, video_width) == 36);
    assert!(offset_of!(V, video_height) == 40);
    assert!(offset_of!(V, cursor_x) == 44);
    assert!(offset_of!(V, cursor_y) == 48);
    assert!(offset_of!(V, state) == 52);
    assert!(offset_of!(V, flags) == 53);
    assert!(offset_of!(V, reserved) == 54);
};

impl From<DesktopView> for PbDesktopViewV1 {
    /// The state and flag codes are `DesktopView`'s own.
    fn from(view: DesktopView) -> Self {
        Self {
            generation: view.generation,
            presented_frame_id: view.presented_frame_id,
            cursor_seq: view.cursor_seq,
            presented_age_ms: view.presented_age_ms,
            display_width: view.display_width,
            display_height: view.display_height,
            video_width: view.video_width,
            video_height: view.video_height,
            cursor_x: view.cursor_x,
            cursor_y: view.cursor_y,
            state: view.state,
            flags: view.flags,
            reserved: [0; 2],
        }
    }
}

/// 1 with a viewer, 0 without one (`out` zeroed), -1 for a null pointer or a
/// panic. A leaf: no allocation, no tokio, `parking_lot` locks only. Call it
/// on the isolate that also destroys the handle.
///
/// # Safety
/// `handle` must be null or a live handle, and `out` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_view_v1(
    handle: *mut PbMapperHandle,
    out: *mut PbDesktopViewV1,
) -> i32 {
    if handle.is_null() || out.is_null() {
        return -1;
    }
    desktop_video::guard(-1, || {
        let handle = unsafe { &*handle };
        let (code, view) = match handle.desktop.viewer() {
            Some(viewer) => (1, viewer.view().into()),
            None => (0, PbDesktopViewV1::default()),
        };
        // SAFETY: checked non-null; the caller provides a writable struct.
        unsafe { out.write(view) };
        code
    })
}
