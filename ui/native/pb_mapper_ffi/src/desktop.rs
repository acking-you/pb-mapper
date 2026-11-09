//! The desktop over FFI. Lifecycle JSON (`pb_mapper_desktop_call_json`) runs
//! on Dart worker isolates under `block_on`. The viewer's hot ops
//! (`pb_mapper_desktop_viewer_call_json`) and its per-frame view leaf
//! (`pb_mapper_desktop_view_v1`) are synchronous and safe on the UI isolate:
//! they never enter the runtime or take a tokio lock. Video reaches native
//! consumers through `desktop_video.rs`, never through Dart. Temporary
//! endpoints belong to the app runtime, never a page widget; explicitly
//! configured background hosts belong to the Windows service.
use crate::{
    desktop_video,
    error::{CtlError, ErrorCode},
    handle::PbMapperHandle,
    response::{err_ctl, err_null_handle, ok_data, parse_c_string},
    state::process_credential,
};
use pb_mapper::{
    Client, ConnectOptions, PathPolicy,
    desktop::{
        self, Access, ConnectError, ConnectStage, Decoders, DesktopView, DisplayModeError,
        DisplaySelection, Host, HostOptions, InputError, Invitation, InvitationError,
        InvitationFormat, KeyframeReason, Mode, ModeTarget, QualityPreset, Resolution, StopReason,
        Viewer, ViewerConfig,
    },
};
#[cfg(windows)]
use pb_mapper_core::checksum::Credential;
use pb_mapper_core::checksum::get_process_credential;
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::c_char;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
#[path = "desktop_trust.rs"]
pub(crate) mod trust;

/// The longest hot request: a batch of 16 inputs, text included.
const MAX_VIEWER_REQUEST: usize = 64 * 1024;

#[derive(Default)]
struct BackgroundCache {
    snapshot: Option<Value>,
    checked: Option<Instant>,
    refreshing: bool,
    revision: u64,
}
impl BackgroundCache {
    fn store(&mut self, mut snapshot: Value) -> Value {
        if snapshot["service_disabled"] == true
            && let Some(previous) = self
                .snapshot
                .as_ref()
                .filter(|value| background_configured(value))
        {
            // SCM Disabled proves there is no active/restarting service, but
            // does not destroy its saved grant. Preserve that distinction.
            let message = snapshot["message"].clone();
            snapshot = previous.clone();
            snapshot["enabled"] = json!(false);
            snapshot["host"] = Value::Null;
            snapshot["service_disabled"] = json!(true);
            snapshot["message"] = message;
        }
        self.revision += 1;
        self.checked = Some(Instant::now());
        self.snapshot = Some(snapshot.clone());
        snapshot
    }
}

/// The connected viewer, published once its host accepted.
struct Live {
    viewer: Arc<Viewer>,
    /// Its video source in the registry.
    source: u64,
    /// The invitation that passed the certificate pin: what remembering this
    /// computer stores.
    invitation: Invitation,
}

/// A connect between its registration and its outcome.
struct PendingConnect {
    epoch: u64,
    cancel: CancellationToken,
    /// The last stage the attempt passed, as `stage_code` writes it.
    stage: Arc<AtomicU8>,
    /// The app hid the viewer meanwhile, so the session starts paused.
    paused: bool,
}

#[derive(Default)]
pub(crate) struct DesktopState {
    // Outbound connection setup must never hold the local host's stop/revoke
    // lock. Each endpoint owns its lifecycle independently.
    host: tokio::sync::Mutex<Option<Host>>,
    /// Hot ops read it on the UI isolate, so it is never held across an
    /// await. Where both are taken, `connecting` comes first.
    viewer: parking_lot::RwLock<Option<Live>>,
    connecting: parking_lot::Mutex<Option<PendingConnect>>,
    viewer_epoch: AtomicU64,
    // Guarded by the host lifecycle lock. Retain even an ephemeral local key
    // while its host lives so ownership can move to the service unchanged.
    host_identity: tokio::sync::Mutex<Option<LocalIdentity>>,
    background: Arc<parking_lot::Mutex<BackgroundCache>>,
    /// The settings' relay scope as last read, for a status poll that finds
    /// the settings locked: it never waits for them.
    relay: parking_lot::Mutex<Option<RelayScope>>,
}
/// The relay and credential namespace the settings name: where a host
/// started now is published, and what `matches_relay` compares with.
#[derive(Clone)]
struct RelayScope {
    server: String,
    namespace: Option<u64>,
}
impl RelayScope {
    /// A host published on `server` under `namespace` is where the settings
    /// publish: the same address, compared as the service and the remembered
    /// grant compare it, and the same namespace.
    fn holds(&self, server: &str, namespace: Option<u64>) -> bool {
        self.namespace.is_some() && self.namespace == namespace && self.server == server
    }
}
/// The running local host's identity and settings: what a restart on another
/// display or under another service, and the background handoff, start from.
#[derive(Clone)]
struct LocalIdentity {
    scope: trust::Scope,
    identity: desktop::HostIdentity,
    settings: trust::Settings,
    remembered: bool,
}
impl Drop for DesktopState {
    /// Destroying the handle closes the viewer's video source, so native
    /// consumers exit even when Dart never disconnected.
    fn drop(&mut self) {
        if let Some(live) = self.viewer.get_mut().take() {
            desktop_video::remove_source(live.source);
        }
    }
}
impl DesktopState {
    fn cache_background(&self, snapshot: Value) -> Value {
        self.background.lock().store(snapshot)
    }

    fn background_status(&self, runtime: &tokio::runtime::Handle) -> Option<Value> {
        let mut cache = self.background.lock();
        let snapshot = cache.snapshot.clone();
        // Discovery is explicit. An unused helper adds no IPC to status polls.
        if snapshot.as_ref().is_some_and(background_configured)
            && !cache.refreshing
            && cache
                .checked
                .is_none_or(|time| time.elapsed() >= Duration::from_secs(2))
        {
            cache.refreshing = true;
            let revision = cache.revision;
            let shared = self.background.clone();
            runtime.spawn(async move {
                let result = query_background().await;
                let mut cache = shared.lock();
                cache.refreshing = false;
                if cache.revision == revision {
                    let snapshot = match result {
                        Ok(snapshot) => snapshot,
                        Err(error) => failed_background(cache.snapshot.as_ref(), error),
                    };
                    cache.store(snapshot);
                }
            });
        }
        snapshot
    }

    /// The settings' relay scope without waiting for the settings lock: the
    /// last one read while another call holds it, none before the first.
    async fn relay_scope(
        &self,
        settings: &tokio::sync::Mutex<crate::state::PbMapperState>,
    ) -> Option<RelayScope> {
        let Ok(state) = settings.try_lock() else {
            return self.relay.lock().clone();
        };
        let scope = RelayScope {
            server: state.get_config_status().await.server_address,
            namespace: get_process_credential()
                .ok()
                .map(|credential| credential.key_id()),
        };
        *self.relay.lock() = Some(scope.clone());
        Some(scope)
    }

    pub(crate) fn current_invitation(&self) -> Option<Invitation> {
        self.viewer
            .read()
            .as_ref()
            .map(|live| live.invitation.clone())
    }

    fn viewer(&self) -> Option<Arc<Viewer>> {
        self.viewer.read().as_ref().map(|live| live.viewer.clone())
    }

    /// Registers before any await, so a disconnect cancels the attempt even
    /// before network work starts. `connect` gets the stage cell its progress
    /// writes. The viewer is published with its video source unless a
    /// disconnect came first.
    async fn connect_viewer<F>(
        &self,
        connect: impl FnOnce(Arc<AtomicU8>) -> F,
    ) -> Result<(Arc<Viewer>, u64), CtlError>
    where
        F: std::future::Future<Output = Result<(Viewer, Invitation), CtlError>>,
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
            let (viewer, invitation) = result?;
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
                    invitation,
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
    async fn disconnect_viewer(&self) -> Result<(), CtlError> {
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
    fn viewer_status(&self, local: Option<Mode>) -> Result<Value, CtlError> {
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
fn stage_code(stage: ConnectStage) -> u8 {
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

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Capabilities,
    HostStatus,
    TrustStatus,
    TrustRevoke,
    HostRestore,
    /// Put back the display mode a viewer changed, of whichever host this
    /// app shows: its own, or the background service's.
    HostRestoreDisplayMode,
    HostEnable {
        service: String,
        display_id: u32,
        allow_control: bool,
    },
    HelperAction {
        action: HelperVerb,
    },
    /// `move_relay`: the owner confirmed that a `start` may move the host
    /// to another relay or namespace, with a new invitation.
    BackgroundAction {
        action: BackgroundVerb,
        #[serde(default)]
        move_relay: bool,
    },
    HostStart(HostStart),
    HostStop {
        #[serde(default)]
        on_exit: bool,
    },
    HostPermission {
        allow_control: bool,
    },
    HostEndSession,
    HostSetDisplay {
        display_id: u32,
    },
    HostRename {
        service: String,
    },
    InspectInvitation {
        invitation: String,
    },
    /// Exactly one of `invitation` and `computer_id`, a remembered computer.
    Connect {
        #[serde(default)]
        invitation: Option<String>,
        #[serde(default)]
        computer_id: Option<String>,
        #[serde(default)]
        relay_only: bool,
        #[serde(default)]
        quality: QualityPreset,
        refresh_hz: f64,
        /// What the runner decodes in hardware, as its probe found it, H.265
        /// left out once it failed this run: the offer, and the largest
        /// picture of each decoder, which bounds the limits.
        decoders: DecoderOffer,
        /// Absent: the SDK's default, the display's own size.
        #[serde(default)]
        resolution: Option<Resolution>,
        /// 0 is Auto; otherwise the user's own rate, see `valid_max_fps`.
        #[serde(default)]
        max_fps: u32,
    },
    Disconnect,
}

/// Each decoder's largest picture as `[width, height]`; `null` for none.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecoderOffer {
    h264: Option<[u32; 2]>,
    h265: Option<[u32; 2]>,
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

/// What the helper card asks of Windows. Setup's own install and enable go
/// through `host_enable`.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum HelperVerb {
    Install,
    Disable,
    Uninstall,
}

/// The service's state, or its saved host resumed where the settings name
/// now (`resume_background`), through setup when the helper needs it.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum BackgroundVerb {
    Status,
    Start,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostStart {
    service: String,
    display_id: u32,
    allow_control: bool,
    #[serde(default)]
    relay_only: bool,
    #[serde(default)]
    privileged: bool,
    #[serde(default)]
    remember: bool,
    #[serde(default)]
    auto_start: bool,
    #[serde(default)]
    background: bool,
}

/// What a local host starts from.
enum Start {
    /// `host_start`: the settings the owner just chose.
    Fresh(HostStart),
    /// `host_restore`: the remembered ones.
    Restore,
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

fn err(e: impl std::fmt::Display) -> CtlError {
    CtlError::io(e.to_string())
}

/// A maximum frame rate as `connect` and a remembered computer take it: 0
/// for Auto, otherwise one the user chose, 30..=120.
pub(crate) fn valid_max_fps(max_fps: u32) -> bool {
    max_fps == 0 || (30..=120).contains(&max_fps)
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

fn invitation_error(failure: InvitationError) -> CtlError {
    let code = match failure {
        InvitationError::SetupString => ErrorCode::SetupString,
        _ => ErrorCode::InvalidInvitation,
    };
    CtlError::new(code, failure.to_string())
}

/// What `inspect_invitation` shows of an invitation: never its token.
fn inspection(invitation: &Invitation) -> Value {
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

fn background_configured(snapshot: &Value) -> bool {
    snapshot["configured"].as_bool().unwrap_or(false)
}

#[cfg(windows)]
fn background_enabled(snapshot: &Value) -> bool {
    snapshot["enabled"].as_bool().unwrap_or(false)
}

/// `snapshot` with `matches_relay`: whether the background host it describes
/// is published where the settings in `current` name. Null while they have
/// not been read; with nothing configured, nothing is elsewhere.
fn with_relay_match(mut snapshot: Value, current: Option<&RelayScope>) -> Value {
    let matches = current.map(|current| {
        !background_configured(&snapshot)
            || snapshot["server"]
                .as_str()
                .is_some_and(|server| current.holds(server, snapshot["namespace"].as_u64()))
    });
    snapshot["matches_relay"] = json!(matches);
    snapshot
}

fn unavailable_background(message: impl Into<String>) -> Value {
    json!({"configured": false, "enabled": false, "host": null,
        "invitation": null, "settings": null, "server": null, "namespace": null,
        "supported": cfg!(windows), "message": message.into()})
}

fn failed_background(previous: Option<&Value>, error: CtlError) -> Value {
    let mut snapshot = previous
        .cloned()
        .unwrap_or_else(|| unavailable_background(""));
    snapshot["message"] = json!(error.to_string());
    snapshot["host"] = Value::Null;
    // A previous Disabled observation cannot authorize a new local host after
    // service state becomes unknown. Fresh queries still fail closed.
    snapshot["service_disabled"] = json!(false);
    snapshot
}

async fn query_background() -> Result<Value, CtlError> {
    #[cfg(windows)]
    {
        let helper = tokio::task::spawn_blocking(desktop::privileged::status)
            .await
            .map_err(err)?;
        query_background_for_helper(helper, async {
            let snapshot = desktop::privileged::background::status()
                .await
                .map_err(err)?;
            serde_json::to_value(snapshot).map_err(err)
        })
        .await
    }
    #[cfg(not(windows))]
    Ok(unavailable_background(
        "background hosting requires Windows",
    ))
}

#[cfg(windows)]
async fn query_background_for_helper(
    helper: desktop::privileged::Status,
    query: impl std::future::Future<Output = Result<Value, CtlError>>,
) -> Result<Value, CtlError> {
    if !helper.installed {
        return Ok(unavailable_background(helper.message));
    }
    if helper.startup_disabled && !helper.running {
        let mut snapshot = unavailable_background(
            "Background service is disabled; enable it to resume remembered sharing",
        );
        snapshot["service_disabled"] = json!(true);
        return Ok(snapshot);
    }
    if !helper.running {
        return Err(err(
            "background service is unavailable and may restart; disable it before starting local sharing",
        ));
    }
    if !helper.authorized_application {
        return Err(err(
            "open the installed application to check background sharing, or disable its service before starting local sharing",
        ));
    }
    helper.ensure_ready().map_err(err)?;
    query.await
}

async fn discover_background(
    state: &DesktopState,
    query: impl std::future::Future<Output = Result<Value, CtlError>>,
) -> Value {
    let revision = state.background.lock().revision;
    let result = query.await;
    let mut cache = state.background.lock();
    if cache.revision != revision {
        // A configure/stop/revoke response supersedes an earlier status read.
        return cache
            .snapshot
            .clone()
            .unwrap_or_else(|| unavailable_background(""));
    }
    let snapshot = match result {
        Ok(snapshot) => snapshot,
        // A management failure must not hide a previously enabled grant.
        Err(error) => failed_background(cache.snapshot.as_ref(), error),
    };
    cache.store(snapshot)
}

async fn disable_legacy_startup(handle: &PbMapperHandle) -> Result<(), CtlError> {
    let directory = handle.state.lock().await.desktop_config_dir();
    tokio::task::spawn_blocking(move || {
        if let Some(mut saved) = trust::load(&directory)? {
            saved.settings.auto_start = false;
            trust::save(&directory, &saved)?;
        }
        Ok::<_, CtlError>(())
    })
    .await
    .map_err(err)?
}

/// `status`: the service's state, asked of it. `start`: `resume_background`,
/// answered as `host_enable` is, its `background` with `matches_relay`.
async fn background_action(
    handle: &PbMapperHandle,
    verb: BackgroundVerb,
    move_relay: bool,
) -> Result<Value, CtlError> {
    match verb {
        BackgroundVerb::Status => {
            let snapshot = discover_background(&handle.desktop, query_background()).await;
            let relay = handle.desktop.relay_scope(&handle.state).await;
            Ok(with_relay_match(snapshot, relay.as_ref()))
        }
        #[cfg(windows)]
        BackgroundVerb::Start => {
            let mut reply = resume_background(handle, move_relay).await?;
            if reply["background"].is_object() {
                let relay = handle.desktop.relay_scope(&handle.state).await;
                reply["background"] = with_relay_match(reply["background"].take(), relay.as_ref());
            }
            Ok(reply)
        }
        #[cfg(not(windows))]
        BackgroundVerb::Start => {
            let _ = move_relay;
            Err(err("background hosting requires Windows"))
        }
    }
}

/// An explicit resume of the service's saved host: configured again from
/// the settings, never started with the relay it was saved with, which the
/// owner may have left. On the same relay and namespace the service keeps
/// its identity, and an unchanged configuration only starts it again.
/// Anywhere else it makes a new one, so the invitation changes: refused with
/// `RELAY_CHANGED` unless `move_relay` says the owner was asked. A helper
/// that is not ready, or another build than this app's, resumes through
/// setup instead (`enable_host`, which allows direct connections as setup
/// always does), which installs, updates or enables it behind one consent
/// prompt and may restart the app into it.
#[cfg(windows)]
async fn resume_background(handle: &PbMapperHandle, move_relay: bool) -> Result<Value, CtlError> {
    // As `enable_host`: the host slot, then the settings.
    let mut slot = handle.desktop.host.lock().await;
    if slot.is_some() {
        return Err(err(
            "stop local desktop sharing before starting the background host",
        ));
    }
    let config = handle.state.lock().await.get_config_status().await;
    let credential = process_credential()?;
    let namespace = credential.key_id();
    let credential = credential_text(credential)?;
    let helper = tokio::task::spawn_blocking(desktop::privileged::status)
        .await
        .map_err(err)?;
    // A helper that cannot be asked resumes the host the card showed.
    let saved = if helper.ensure_ready().is_ok() {
        handle.desktop.cache_background(query_background().await?)
    } else {
        handle
            .desktop
            .background
            .lock()
            .snapshot
            .clone()
            .unwrap_or_else(|| unavailable_background(""))
    };
    let options = resume_options(&saved, config, namespace, credential, move_relay)?;
    if helper_setup(&helper, true).is_some() {
        let desktop::privileged::background::Settings {
            service,
            display_id,
            allow_control,
            ..
        } = options.settings;
        return enable_host(handle, &mut slot, service, display_id, allow_control, true).await;
    }
    disable_legacy_startup(handle).await?;
    let snapshot = desktop::privileged::background::configure(options)
        .await
        .map_err(err)?;
    let snapshot = handle
        .desktop
        .cache_background(serde_json::to_value(snapshot).map_err(err)?);
    Ok(json!({"restart": false, "invitation": snapshot["invitation"], "background": snapshot}))
}

/// What resuming the service's `saved` host configures: the service, display
/// and permissions it saved, published through the relay, credential (of
/// `namespace`) and carrier the settings name now, with LAN-first as they
/// have it, and restored at startup again. Moving it to another relay or
/// namespace needs `move_relay`.
#[cfg(windows)]
fn resume_options(
    saved: &Value,
    config: crate::state::AppConfig,
    namespace: u64,
    credential: String,
    move_relay: bool,
) -> Result<desktop::privileged::background::Options, CtlError> {
    use desktop::privileged::background::{Options, ResumeTarget, Snapshot};
    // Only what a resume reads, from the snapshot the card showed.
    let saved = Snapshot {
        configured: background_configured(saved),
        settings: serde_json::from_value(saved["settings"].clone()).ok(),
        server: saved["server"].as_str().map(Into::into),
        namespace: saved["namespace"].as_u64(),
        ..Snapshot::default()
    };
    let target = ResumeTarget {
        server: config.server_address,
        namespace,
        credential,
        relay_transport: config.relay_transport,
        lan_first: config.lan_first,
    };
    Options::resume(&saved, target, move_relay).map_err(|failure| {
        if failure.kind() == std::io::ErrorKind::PermissionDenied {
            CtlError::new(ErrorCode::RelayChanged, failure.to_string())
        } else {
            err(failure)
        }
    })
}

async fn stop_background(state: &DesktopState) -> Result<(), CtlError> {
    let snapshot = query_background().await?;
    #[cfg(windows)]
    if background_configured(&snapshot) && snapshot["service_disabled"] != true {
        let snapshot = desktop::privileged::background::stop().await.map_err(err)?;
        state.cache_background(serde_json::to_value(snapshot).map_err(err)?);
        return Ok(());
    }
    state.cache_background(snapshot);
    Ok(())
}

async fn revoke_background(handle: &PbMapperHandle) -> Result<Value, CtlError> {
    #[cfg(windows)]
    {
        let server = handle
            .state
            .lock()
            .await
            .get_config_status()
            .await
            .server_address;
        let namespace = get_process_credential()
            .ok()
            .map(|credential| credential.key_id());
        let snapshot = desktop::privileged::background::revoke_scope(server, namespace)
            .await
            .map_err(err)?;
        serde_json::to_value(snapshot).map_err(err)
    }
    #[cfg(not(windows))]
    {
        let _ = handle;
        Ok(unavailable_background(
            "background hosting requires Windows",
        ))
    }
}

async fn revoke_trust_with_background(
    handle: &PbMapperHandle,
    background: impl std::future::Future<Output = Result<Value, CtlError>>,
) -> Result<(), CtlError> {
    // The handoff and its revocation share one lifecycle lock. In particular,
    // observe/revoke the service only after an in-flight Configure has finished;
    // the UI's cached owner is not authoritative here.
    let state = &handle.desktop;
    let mut slot = state.host.lock().await;
    if let Some(host) = slot.as_ref() {
        host.set_control_allowed(false);
    }
    let directory = handle.state.lock().await.desktop_config_dir();
    let retired = tokio::task::spawn_blocking(move || trust::revoke(&directory))
        .await
        .map_err(err)
        .and_then(|result| result);
    // Retire pending automatic setup before deleting the service grant, so a
    // later startup cannot recreate access from a leftover local identity.
    let revoked = match retired {
        Ok(()) => background
            .await
            .map(|snapshot| state.cache_background(snapshot)),
        Err(failure) => Err(failure),
    };
    // Its viewer hears that access was withdrawn, and does not reconnect.
    let stopped = if let Some(host) = slot.take() {
        host.stop_with(StopReason::Revoked).await.map_err(err)
    } else {
        Ok(())
    };
    *state.host_identity.lock().await = None;
    stopped?;
    revoked?;
    Ok(())
}

/// The lifecycle ops and the remembered computers. They run under
/// `block_on`, so call this from a background isolate, never the UI one.
/// Anything else is `INVALID_ARGUMENT`. Free the reply with
/// `pb_mapper_free_string`.
///
/// # Safety
/// `handle` must be null or a live handle, and `request` null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_call_json(
    handle: *mut PbMapperHandle,
    request: *const c_char,
) -> *mut c_char {
    if handle.is_null() {
        return err_null_handle();
    }
    let request = match parse_c_string(request, "request").and_then(|text| {
        if text.len() > 16384 {
            return Err(CtlError::invalid_argument("desktop request too large"));
        }
        serde_json::from_str::<Value>(&text)
            .map_err(|_| CtlError::invalid_argument("invalid desktop request"))
    }) {
        Ok(request) => request,
        Err(e) => return err_ctl(&e),
    };
    let handle = unsafe { &*handle };
    // The remembered computers have their own strict request type.
    let result = if request["op"]
        .as_str()
        .is_some_and(|op| op.starts_with("computers_"))
    {
        handle
            .runtime
            .block_on(crate::desktop_computers::call(handle, request))
    } else {
        match serde_json::from_value::<Request>(request) {
            Ok(request) => handle.runtime.block_on(call(handle, request)),
            Err(_) => Err(CtlError::invalid_argument("invalid desktop request")),
        }
    };
    match result {
        Ok(value) => ok_data(value),
        Err(e) => err_ctl(&e),
    }
}

/// The helper card's verbs. Installing replaces this UI after a ready ack.
async fn helper_action(handle: &PbMapperHandle, verb: HelperVerb) -> Result<Value, CtlError> {
    #[cfg(windows)]
    {
        use desktop::privileged::Action;
        // Installing may resume a saved service grant. Keep it serialized
        // with local starts until the helper process exits. An update from
        // here keeps a stopped or Disabled service as it is.
        let local = handle.desktop.host.lock().await;
        let action = match verb {
            HelperVerb::Install if local.is_some() => {
                return Err(err(
                    "stop local sharing before enabling the background service",
                ));
            }
            HelperVerb::Install => Action::Install { enable: false },
            HelperVerb::Disable => Action::Disable,
            HelperVerb::Uninstall => Action::Uninstall,
        };
        let restart = matches!(action, Action::Install { .. });
        tokio::task::spawn_blocking(move || desktop::privileged::request_installation(action))
            .await
            .map_err(err)?
            .map_err(err)?;
        if restart {
            desktop::privileged::prepare_application_restart()
                .await
                .map_err(err)?;
        }
        Ok(json!({"restart": restart}))
    }
    #[cfg(not(windows))]
    {
        let _ = (handle, verb);
        Err(err("the helper requires Windows"))
    }
}

/// The remembered grant, read off the runtime.
async fn load_trust(directory: PathBuf) -> Result<Option<trust::Saved>, CtlError> {
    tokio::task::spawn_blocking(move || trust::load(&directory))
        .await
        .map_err(err)?
}

/// The `capabilities` op's `capabilities`: the keys the app has always read,
/// and why this computer cannot host, when it cannot.
fn capabilities_json(support: desktop::HostSupport) -> Value {
    json!({
        "host": support.host,
        "viewer": support.viewer,
        "secure_desktop": support.secure_desktop,
        "mode": if support.os == Some(desktop::HostOs::Windows) {
            "windows_desktop"
        } else {
            "native_viewer"
        },
        "reason": support.reason,
    })
}

async fn call(handle: &PbMapperHandle, request: Request) -> Result<Value, CtlError> {
    let state = &handle.desktop;
    match request {
        Request::Capabilities => {
            return tokio::task::spawn_blocking(|| {
                // The app's first question: once per process, put back a
                // display mode a host of this app left changed when it died.
                static RECOVERED: std::sync::Once = std::sync::Once::new();
                RECOVERED.call_once(|| {
                    if let Err(failure) = desktop::recover_display_modes(None) {
                        tracing::warn!(event = "display_modes_not_recovered", %failure);
                    }
                });
                let displays = desktop::displays();
                #[cfg(windows)]
                let helper = serde_json::to_value(desktop::privileged::status()).ok();
                #[cfg(not(windows))]
                let helper: Option<Value> = None;
                json!({"capabilities": capabilities_json(desktop::host_support()), "helper": helper, "displays": displays.as_ref().ok(), "message": displays.err().map(|e| e.to_string())})
            }).await.map_err(err);
        }
        Request::HelperAction { action } => return helper_action(handle, action).await,
        Request::TrustStatus => {
            let (directory, relay) = {
                let state = handle.state.lock().await;
                (
                    state.desktop_config_dir(),
                    state.get_config_status().await.server_address,
                )
            };
            let saved = load_trust(directory).await?;
            let namespace = get_process_credential()
                .ok()
                .map(|credential| credential.key_id());
            let matches = saved.as_ref().is_some_and(|saved| {
                namespace.is_some_and(|namespace| {
                    saved.matches(&trust::Scope {
                        directory: Default::default(),
                        relay: relay.clone(),
                        namespace,
                    })
                })
            });
            // The invitation resumes sharing without setup, so only a grant
            // for this relay and namespace offers one.
            let saved = match saved {
                Some(saved) => {
                    let invitation = if matches {
                        let service = saved.settings.service.clone();
                        Some(saved.identity()?.invitation(service).map_err(err)?.encode())
                    } else {
                        None
                    };
                    Some(saved.metadata(invitation))
                }
                None => None,
            };
            return Ok(
                json!({"supported": trust::supported(), "saved": saved, "matches_relay": matches}),
            );
        }
        Request::TrustRevoke => {
            revoke_trust_with_background(handle, revoke_background(handle)).await?;
        }
        Request::HostStatus => {
            // Each host says whether it is published where the settings
            // name (`matches_relay`), and where it is (`server`).
            let relay = state.relay_scope(&handle.state).await;
            let background = state
                .background_status(handle.runtime.handle())
                .map(|snapshot| with_relay_match(snapshot, relay.as_ref()));
            // A lifecycle call (a display switch, a rename, a helper action
            // waiting on UAC) holds the host lock, but must never delay a
            // status poll. `busy` says the host is then unknown, not absent.
            // The invitation stays beside the status, so nothing that
            // serialises a status carries the secret.
            let (host, busy) = match state.host.try_lock() {
                Ok(slot) => (
                    slot.as_ref().map(|host| {
                        let invitation = host.invitation();
                        let mut value = json!({"status": host.status(), "service": invitation.service, "invitation": invitation.encode()});
                        // Set beside the host, under the lock held here.
                        if let Ok(local) = state.host_identity.try_lock()
                            && let Some(scope) = local.as_ref().map(|local| &local.scope)
                        {
                            value["server"] = json!(scope.relay);
                            value["matches_relay"] = json!(relay.as_ref().map(|current| {
                                current.holds(&scope.relay, Some(scope.namespace))
                            }));
                        }
                        value
                    }),
                    false,
                ),
                Err(_) => (None, true),
            };
            return Ok(json!({"host": host, "busy": busy, "background": background}));
        }
        Request::BackgroundAction { action, move_relay } => {
            return background_action(handle, action, move_relay).await;
        }
        Request::HostEnable {
            service,
            display_id,
            allow_control,
        } => {
            let mut slot = state.host.lock().await;
            return enable_host(handle, &mut slot, service, display_id, allow_control, true).await;
        }
        Request::HostRestore => {
            let directory = handle.state.lock().await.desktop_config_dir();
            if let Some(saved) = load_trust(directory)
                .await?
                .filter(|saved| saved.settings.background)
            {
                let mut slot = state.host.lock().await;
                return enable_host(
                    handle,
                    &mut slot,
                    saved.settings.service,
                    saved.settings.display_id,
                    saved.settings.allow_control,
                    false,
                )
                .await;
            }
            return start_local(handle, Start::Restore).await;
        }
        Request::HostStart(start) if start.background => {
            return start_background(handle, start).await;
        }
        Request::HostStart(start) => return start_local(handle, Start::Fresh(start)).await,
        Request::HostStop { on_exit } => {
            let mut slot = state.host.lock().await;
            let stopped = if let Some(host) = slot.take() {
                host.stop().await.map_err(err)
            } else {
                Ok(())
            };
            *state.host_identity.lock().await = None;
            if on_exit {
                // The system service owns its host. Closing this UI only
                // releases the temporary host owned by this FFI handle.
                stopped?;
                return Ok(json!({}));
            }
            let disabled_startup = disable_legacy_startup(handle).await;
            let background_stopped = stop_background(state).await;
            stopped?;
            disabled_startup?;
            background_stopped?;
        }
        Request::HostPermission { allow_control } => {
            let slot = state.host.lock().await;
            let Some(host) = slot.as_ref() else {
                #[cfg(windows)]
                {
                    let snapshot = query_background().await?;
                    if background_enabled(&snapshot) {
                        let snapshot =
                            desktop::privileged::background::set_control_allowed(allow_control)
                                .await
                                .map_err(err)?;
                        state.cache_background(serde_json::to_value(snapshot).map_err(err)?);
                        return Ok(json!({}));
                    }
                }
                return Err(err("desktop is not shared"));
            };
            if !allow_control {
                host.set_control_allowed(false);
            }
            if let Some(local) = state
                .host_identity
                .lock()
                .await
                .clone()
                .filter(|local| local.remembered)
            {
                let scope = local.scope;
                let service = host.invitation().service.clone();
                tokio::task::spawn_blocking(move || {
                    let mut saved = trust::load(&scope.directory)?
                        .ok_or_else(|| err("remembered trust was revoked"))?;
                    if !saved.matches(&scope) || saved.settings.service != service {
                        return Err(err("remembered trust changed; restart sharing"));
                    }
                    saved.settings.allow_control = allow_control;
                    trust::save(&scope.directory, &saved)
                })
                .await
                .map_err(err)??;
            }
            host.set_control_allowed(allow_control);
        }
        Request::HostRestoreDisplayMode => {
            let slot = state.host.lock().await;
            if let Some(host) = slot.as_ref() {
                return Ok(json!({"restored": host.restore_display_mode()}));
            }
            #[cfg(windows)]
            if state
                .background
                .lock()
                .snapshot
                .as_ref()
                .is_some_and(background_enabled)
            {
                let snapshot = desktop::privileged::background::restore_display_mode()
                    .await
                    .map_err(err)?;
                state.cache_background(serde_json::to_value(snapshot).map_err(err)?);
                return Ok(json!({"restored": true}));
            }
            return Err(err("desktop is not shared"));
        }
        Request::HostEndSession => {
            let slot = state.host.lock().await;
            if let Some(host) = slot.as_ref() {
                host.end_session();
                return Ok(json!({}));
            }
            // The background host the UI shows: the service ends its session
            // and keeps sharing.
            #[cfg(windows)]
            if state
                .background
                .lock()
                .snapshot
                .as_ref()
                .is_some_and(background_enabled)
            {
                let snapshot = desktop::privileged::background::end_session()
                    .await
                    .map_err(err)?;
                state.cache_background(serde_json::to_value(snapshot).map_err(err)?);
                return Ok(json!({}));
            }
            return Err(err("desktop is not shared"));
        }
        // A display switch resumes at once with the same invitation, so the
        // viewer reconnects; a renamed service is no longer what it reaches.
        Request::HostSetDisplay { display_id } => {
            return restart_host(handle, StopReason::Restarting, |settings| {
                settings.display_id = display_id;
            })
            .await;
        }
        Request::HostRename { service } => {
            return restart_host(handle, StopReason::HostStopped, |settings| {
                settings.service = service;
            })
            .await;
        }
        Request::InspectInvitation { invitation } => {
            return Ok(inspection(
                &Invitation::decode(&invitation).map_err(invitation_error)?,
            ));
        }
        Request::Connect {
            invitation,
            computer_id,
            relay_only,
            quality,
            refresh_hz,
            decoders,
            resolution,
            max_fps,
        } => {
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
            return Ok(json!({"video_source": source, "codec": viewer.status().stats.codec}));
        }
        Request::Disconnect => {
            state.disconnect_viewer().await?;
        }
    }
    Ok(json!({}))
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
            let (client, lan_first) = {
                let state = handle.state.lock().await;
                (
                    state.relay_client()?,
                    state.get_config_status().await.lan_first,
                )
            };
            let options = ConnectOptions {
                path: if relay_only {
                    PathPolicy::RelayOnly
                } else {
                    PathPolicy::Auto
                },
                lan_first,
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
            Ok((viewer, invitation))
        })
        .await
}

/// `host_start` with `background`: the service takes the grant, never this UI.
async fn start_background(handle: &PbMapperHandle, start: HostStart) -> Result<Value, CtlError> {
    if !start.privileged || !start.remember {
        return Err(CtlError::invalid_argument(
            "background hosting requires explicit helper and remembered-trust consent",
        ));
    }
    #[cfg(windows)]
    {
        // Serialize both owners before loading configuration or talking to the
        // service. A concurrent Stop cannot finish and then be undone by this start.
        let slot = handle.desktop.host.lock().await;
        if slot.is_some() {
            return Err(err("desktop is already shared"));
        }
        handle.desktop.cache_background(query_background().await?);
        let (directory, config) = {
            let state = handle.state.lock().await;
            (state.desktop_config_dir(), state.get_config_status().await)
        };
        let credential = process_credential()?;
        let namespace = credential.key_id();
        let credential = credential_text(credential)?;
        // Keep the old identity for an explicit restore, but never allow a
        // later app launch to resurrect its independent automatic grant.
        disable_legacy_startup(handle).await?;
        let legacy = load_trust(directory.clone()).await?;
        let options = desktop::privileged::background::Options {
            server: config.server_address,
            credential,
            relay_transport: config.relay_transport,
            settings: desktop::privileged::background::Settings {
                service: start.service.clone(),
                display_id: start.display_id,
                display_device: display_device(start.display_id),
                allow_control: start.allow_control,
                allow_direct: !start.relay_only,
                auto_start: start.auto_start,
            },
            lan_first: config.lan_first,
        };
        let scope = trust::Scope {
            directory: directory.clone(),
            relay: options.server.clone(),
            namespace,
        };
        let snapshot = if let Some(saved) =
            legacy.filter(|saved| saved.matches(&scope) && saved.settings.service == start.service)
        {
            desktop::privileged::background::configure_with_identity(options, saved.identity()?)
                .await
        } else {
            desktop::privileged::background::configure(options).await
        }
        .map_err(err)?;
        let snapshot = handle
            .desktop
            .cache_background(serde_json::to_value(snapshot).map_err(err)?);
        *handle.desktop.host_identity.lock().await = None;
        tokio::task::spawn_blocking(move || trust::revoke(&directory))
            .await
            .map_err(err)??;
        Ok(json!({"invitation": snapshot["invitation"], "background": snapshot}))
    }
    #[cfg(not(windows))]
    {
        let _ = handle;
        Err(err("background hosting requires Windows"))
    }
}

/// The device name of the display `display_id` names now, which the service
/// keeps beside the id: a reboot or a mode change may give the display
/// another id, never another name.
#[cfg(windows)]
fn display_device(display_id: u32) -> Option<String> {
    desktop::displays()
        .ok()?
        .into_iter()
        .find(|display| display.id == display_id)
        .map(|display| display.name)
}

/// The process credential, as the service stores it.
#[cfg(windows)]
fn credential_text(credential: Credential) -> Result<String, CtlError> {
    match credential {
        Credential::Admin(key) => String::from_utf8(key.to_vec())
            .map_err(|_| err("invalid administrator credential encoding")),
        Credential::Temporary { key_id, key } => Ok(
            pb_mapper_core::checksum::encode_temporary_credential(key_id, &key),
        ),
    }
}

/// A host this UI owns, unless the service already owns this desktop.
async fn start_local(handle: &PbMapperHandle, start: Start) -> Result<Value, CtlError> {
    // Serialize both owners before loading configuration or talking to the
    // service. A concurrent Stop cannot finish and then be undone by this start.
    let mut slot = handle.desktop.host.lock().await;
    if slot.is_some() {
        return Err(err("desktop is already shared"));
    }
    let background = handle.desktop.cache_background(query_background().await?);
    if background_configured(&background) {
        return Err(err(match start {
            Start::Fresh(_) => {
                "this desktop belongs to the background service; enable it to reuse its invitation"
            }
            Start::Restore => {
                "use the configured background host instead of restoring local sharing"
            }
        }));
    }
    // Read under the same lock as the relay settings the trust scope records,
    // so the two cannot disagree. The client is the window's cached one.
    let (directory, config, client) = {
        let state = handle.state.lock().await;
        (
            state.desktop_config_dir(),
            state.get_config_status().await,
            state.relay_client()?,
        )
    };
    let credential = process_credential()?;
    let scope = trust::Scope {
        directory,
        relay: config.server_address.clone(),
        namespace: credential.key_id(),
    };
    let (settings, remember, saved) = match start {
        Start::Fresh(start) => {
            if start.auto_start && !start.remember {
                return Err(err(
                    "automatic sharing requires explicitly remembered trust",
                ));
            }
            let saved = if start.remember {
                load_trust(scope.directory.clone()).await?
            } else {
                None
            };
            let settings = trust::Settings {
                service: start.service,
                display_id: start.display_id,
                allow_control: start.allow_control,
                relay_only: start.relay_only,
                privileged: start.privileged,
                auto_start: start.auto_start,
                background: false,
            };
            (settings, start.remember, saved)
        }
        Start::Restore => {
            let saved = load_trust(scope.directory.clone())
                .await?
                .ok_or_else(|| err("no remembered desktop trust"))?;
            if !saved.matches(&scope) {
                return Err(err(
                    "saved desktop belongs to another relay or namespace; restore the original connection settings first",
                ));
            }
            (saved.settings.clone(), true, Some(saved))
        }
    };
    if remember && !trust::supported() {
        return Err(err("remembered hosting currently requires Windows"));
    }
    let identity = if remember
        && let Some(saved) = saved
            .as_ref()
            .filter(|saved| saved.matches(&scope) && saved.settings.service == settings.service)
    {
        saved.identity()?
    } else {
        desktop::HostIdentity::generate().map_err(err)?
    };
    let persisted = remember
        .then(|| trust::Saved::new(&scope, settings.clone(), &identity))
        .transpose()?;
    let local_identity = LocalIdentity {
        scope,
        identity: identity.clone(),
        settings,
        remembered: remember,
    };
    let host = launch(
        &client,
        &local_identity.settings,
        config.lan_first,
        identity,
    )
    .await?;
    if let Some(saved) = persisted {
        let directory = local_identity.scope.directory.clone();
        let stored = tokio::task::spawn_blocking(move || trust::save(&directory, &saved))
            .await
            .map_err(err)?;
        if let Err(failure) = stored {
            let _ = host.stop().await;
            return Err(failure);
        }
    }
    let invitation = host.invitation().encode();
    *slot = Some(host);
    *handle.desktop.host_identity.lock().await = Some(local_identity);
    Ok(json!({"invitation": invitation}))
}

/// A local host for `settings` under `identity`, through the helper when
/// privileged, with this computer's LAN-first as the config has it now.
async fn launch(
    client: &Client,
    settings: &trust::Settings,
    lan_first: bool,
    identity: desktop::HostIdentity,
) -> Result<Host, CtlError> {
    Host::builder(host_options(settings, lan_first))
        .identity(identity)
        .start(client)
        .await
        .map_err(err)
}

/// The SDK's options for the app's saved `settings`.
fn host_options(settings: &trust::Settings, lan_first: bool) -> HostOptions {
    let mut options = HostOptions::new(settings.service.clone());
    options.display = DisplaySelection::Id(settings.display_id);
    options.access = if settings.privileged {
        Access::Privileged
    } else {
        Access::Ordinary
    };
    options.allow_control = settings.allow_control;
    options.allow_direct = !settings.relay_only;
    options.lan_first = lan_first;
    options
}

/// `host_set_display` and `host_rename`: the running local host again, with
/// its identity, on another display or under another service. Its viewer's
/// session ends with `reason`.
async fn restart_host(
    handle: &PbMapperHandle,
    reason: StopReason,
    change: impl FnOnce(&mut trust::Settings),
) -> Result<Value, CtlError> {
    let state = &handle.desktop;
    let mut slot = state.host.lock().await;
    // The service has no display switch or rename: a background host is
    // stopped and enabled again instead.
    if background_configured(&state.cache_background(query_background().await?)) {
        return Err(CtlError::invalid_argument("stop the background host first"));
    }
    let local = state.host_identity.lock().await.clone();
    let (Some(host), Some(local)) = (slot.as_ref(), local) else {
        return Err(err("desktop is not shared"));
    };
    let mut settings = local.settings.clone();
    // The live policy, which "make view-only" may have changed since.
    settings.allow_control = host.status().control_allowed;
    change(&mut settings);
    // Check the new service before the running host goes.
    local
        .identity
        .invitation(settings.service.clone())
        .map_err(|failure| CtlError::invalid_argument(failure.to_string()))?;
    let (config, client) = {
        let state = handle.state.lock().await;
        (state.get_config_status().await, state.relay_client()?)
    };
    let credential = process_credential()?;
    if config.server_address != local.scope.relay || credential.key_id() != local.scope.namespace {
        return Err(err(
            "the relay settings changed since sharing started; stop sharing and share again",
        ));
    }
    let persisted = local
        .remembered
        .then(|| trust::Saved::new(&local.scope, settings.clone(), &local.identity))
        .transpose()?;
    let Some(host) = slot.take() else {
        return Err(err("desktop is not shared"));
    };
    *state.host_identity.lock().await = None;
    // The same identity, source and live policy, on the relay client the
    // settings name now (a rotated key, another carrier); the old
    // registration goes before the new one.
    let options = host_options(&settings, config.lan_first);
    let host = host
        .restart_on(&client, reason, |restarted| *restarted = options)
        .await
        .map_err(err)?;
    if let Some(saved) = persisted {
        let directory = local.scope.directory.clone();
        let stored = tokio::task::spawn_blocking(move || trust::save(&directory, &saved))
            .await
            .map_err(err)?;
        if let Err(failure) = stored {
            let _ = host.stop().await;
            return Err(failure);
        }
    }
    let invitation = host.invitation().encode();
    *slot = Some(host);
    *state.host_identity.lock().await = Some(LocalIdentity { settings, ..local });
    Ok(json!({"invitation": invitation}))
}

/// The product has one durable host owner. Setup intent and any existing local
/// identity survive the installed application's restart; only the service owns
/// registration after the handoff. Internal SDK/local-host APIs remain separate.
/// `slot` is the locked host lifecycle slot.
#[cfg(windows)]
async fn enable_host(
    handle: &PbMapperHandle,
    slot: &mut Option<Host>,
    service: String,
    display_id: u32,
    allow_control: bool,
    allow_install: bool,
) -> Result<Value, CtlError> {
    let (directory, config) = {
        let state = handle.state.lock().await;
        (state.desktop_config_dir(), state.get_config_status().await)
    };
    let credential = process_credential()?;
    let scope = trust::Scope {
        directory: directory.clone(),
        relay: config.server_address.clone(),
        namespace: credential.key_id(),
    };
    let previous = load_trust(directory.clone()).await?;
    if !allow_install
        && !previous.as_ref().is_some_and(|saved| {
            saved.matches(&scope) && saved.settings.background && saved.settings.auto_start
        })
    {
        // Recheck under the lifecycle lock: Stop may have cancelled the pending
        // action after HostRestore first observed it, while waiting for setup.
        return Err(err(
            "background setup was stopped or belongs to another relay",
        ));
    }
    let local = handle.desktop.host_identity.lock().await.clone();
    let local = local.filter(|local| {
        local.scope.relay == scope.relay
            && local.scope.namespace == scope.namespace
            && slot
                .as_ref()
                .is_some_and(|host| host.invitation().service == service)
    });
    let pending = trust::Saved::for_background(
        &scope,
        service.clone(),
        display_id,
        allow_control,
        previous.as_ref(),
        local.as_ref().map(|local| &local.identity),
    )?;
    let identity = pending.identity()?;
    // Fails before anything changes, never after the running host has gone.
    let credential = credential_text(credential)?;
    tokio::task::spawn_blocking({
        let directory = directory.clone();
        move || trust::save(&directory, &pending)
    })
    .await
    .map_err(err)??;
    let helper = tokio::task::spawn_blocking(desktop::privileged::status)
        .await
        .map_err(err)?;
    let setup = helper_setup(&helper, allow_install);
    // No two owners may publish the same desktop: the local host stops before
    // a service that could already hold an automatic grant is enabled. It
    // is adopted, and its viewer reconnects, only when the service resumes
    // its invitation at once: the same identity and service, and a helper
    // that needs no setup, which the owner may cancel.
    let adopted = match slot.take() {
        Some(host) if local.is_some() && setup.is_none() => Some(host),
        host => {
            if let Some(host) = host {
                host.stop().await.map_err(err)?;
            }
            None
        }
    };
    *handle.desktop.host_identity.lock().await = None;
    if let Some(action) = setup {
        if !allow_install {
            return Err(err(
                "desktop setup is waiting for the installed background service",
            ));
        }
        let installed =
            tokio::task::spawn_blocking(move || desktop::privileged::request_installation(action))
                .await
                .map_err(err)?
                .map_err(err);
        if let Err(failure) = installed {
            // A cancelled/failed first installation must not silently resume
            // the pending action on a later application launch.
            tokio::task::spawn_blocking(move || match previous {
                Some(mut saved) => {
                    if saved.settings.background {
                        saved.settings.auto_start = false;
                    }
                    trust::save(&directory, &saved)
                }
                None => trust::revoke(&directory),
            })
            .await
            .map_err(err)??;
            return Err(failure);
        }
        if matches!(action, desktop::privileged::Action::Install { .. }) {
            desktop::privileged::prepare_application_restart()
                .await
                .map_err(err)?;
            return Ok(json!({"restart": true}));
        }
    }
    let options = desktop::privileged::background::Options {
        server: config.server_address,
        credential,
        relay_transport: config.relay_transport,
        settings: desktop::privileged::background::Settings {
            service,
            display_id,
            display_device: display_device(display_id),
            allow_control,
            allow_direct: true,
            auto_start: true,
        },
        lan_first: config.lan_first,
    };
    let snapshot = match adopted {
        Some(host) => desktop::privileged::background::adopt(host, options, true).await,
        None => desktop::privileged::background::configure_with_identity(options, identity).await,
    }
    .map_err(err)?;
    let snapshot = handle
        .desktop
        .cache_background(serde_json::to_value(snapshot).map_err(err)?);
    // The service now durably owns the key. Retiring the legacy copy also
    // prevents an explicit legacy restore after service revocation.
    tokio::task::spawn_blocking(move || trust::revoke(&directory))
        .await
        .map_err(err)??;
    Ok(json!({"restart": false, "invitation": snapshot["invitation"], "background": snapshot}))
}

/// What setup asks of Windows before the service can host, if anything: for
/// a helper that is not ready, and, where the owner may be asked
/// (`allow_install`), for another build than this app's, which still works
/// but holds the old host. Startup's restore never asks.
#[cfg(windows)]
fn helper_setup(
    helper: &desktop::privileged::Status,
    allow_install: bool,
) -> Option<desktop::privileged::Action> {
    (helper.ensure_ready().is_err() || (allow_install && helper.update_available))
        .then(|| helper_setup_action(helper))
}

/// What setup asks of Windows, in one prompt. Installing again also updates
/// an outdated helper, or another build, in place (D2); enabling leaves it
/// as it is. Setup's install also turns a stopped or Disabled service on,
/// which an update alone would leave off.
#[cfg(windows)]
fn helper_setup_action(helper: &desktop::privileged::Status) -> desktop::privileged::Action {
    if !helper.installed
        || helper.update_required
        || helper.update_available
        || !helper.authorized_application
        || (helper.running && !helper.identity_verified)
    {
        desktop::privileged::Action::Install { enable: true }
    } else {
        desktop::privileged::Action::Enable
    }
}

#[cfg(not(windows))]
async fn enable_host(
    _: &PbMapperHandle,
    _: &mut Option<Host>,
    _: String,
    _: u32,
    _: bool,
    _: bool,
) -> Result<Value, CtlError> {
    Err(err("persistent desktop hosting currently requires Windows"))
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
        let text = parse_c_string(request, "request")?;
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use pb_mapper::desktop::{VideoCodec, VideoLimits};
    use std::{
        ffi::{CStr, CString},
        future::Future,
        task::{Context, Waker},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    fn test_handle(name: &str) -> PbMapperHandle {
        let root = std::env::temp_dir().join(format!("pb-desktop-{name}-{}", std::process::id()));
        // A new state writes the process credential, which a live relay's
        // framing reads, so it is made under the lock such tests hold.
        let _credential_guard =
            pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK.blocking_lock();
        PbMapperHandle {
            desktop: Default::default(),
            tunnel_events: Default::default(),
            state: Arc::new(tokio::sync::Mutex::new(crate::state::PbMapperState::new(
                Some(root.display().to_string()),
            ))),
            control_shutdown: Default::default(),
            runtime: tokio::runtime::Runtime::new().unwrap(),
        }
    }

    fn reply(text: *mut c_char) -> Value {
        let value = serde_json::from_slice(unsafe { CStr::from_ptr(text) }.to_bytes()).unwrap();
        unsafe { crate::logging::pb_mapper_free_string(text) };
        value
    }

    /// The lifecycle export, as a Dart worker calls it.
    fn call_json(handle: &PbMapperHandle, request: Value) -> Value {
        let request = CString::new(request.to_string()).unwrap();
        reply(unsafe {
            pb_mapper_desktop_call_json(std::ptr::from_ref(handle).cast_mut(), request.as_ptr())
        })
    }

    /// The hot export, as the UI isolate calls it.
    fn hot(handle: &PbMapperHandle, request: Value) -> Value {
        let request = CString::new(request.to_string()).unwrap();
        reply(unsafe {
            pb_mapper_desktop_viewer_call_json(
                std::ptr::from_ref(handle).cast_mut(),
                request.as_ptr(),
            )
        })
    }

    fn view(handle: &PbMapperHandle) -> (i32, PbDesktopViewV1) {
        let mut view = PbDesktopViewV1 {
            generation: 9,
            ..Default::default()
        };
        let code =
            unsafe { pb_mapper_desktop_view_v1(std::ptr::from_ref(handle).cast_mut(), &mut view) };
        (code, view)
    }

    fn invitation() -> Invitation {
        desktop::HostIdentity::generate()
            .unwrap()
            .invitation("desktop:test".into())
            .unwrap()
    }

    /// One host record: a `u32` big-endian length, then JSON.
    async fn send(host: &mut DuplexStream, message: Value) {
        let bytes = serde_json::to_vec(&message).unwrap();
        host.write_u32(bytes.len() as u32).await.unwrap();
        host.write_all(&bytes).await.unwrap();
    }

    /// A viewer whose scripted host accepted at once and streams the first
    /// generation. The host side stays open and silent, so `stop()` waits out
    /// the viewer's close grace, seconds long: a held stop.
    async fn scripted_viewer() -> (Viewer, DuplexStream) {
        let (stream, mut host) = tokio::io::duplex(1 << 20);
        let connecting = tokio::spawn(Viewer::over_stream_for_test(
            stream,
            "t".repeat(43),
            ViewerConfig {
                codecs: vec![VideoCodec::H264],
                limits: VideoLimits::for_viewer(60.0),
                fps_override: false,
                quality: QualityPreset::Balanced,
                progress: None,
            },
        ));
        let authenticate = host.read_u32().await.unwrap();
        host.read_exact(&mut vec![0; authenticate as usize])
            .await
            .unwrap();
        send(
            &mut host,
            json!({"t": "accepted", "codec": "h264", "nominal_fps": 60, "quality": "balanced",
                "limits": {"max_width": 1920, "max_height": 1080, "max_fps": 60}, "capabilities": []}),
        )
        .await;
        send(
            &mut host,
            json!({"t": "state", "state": "streaming", "message": "", "control_allowed": true,
                "controlling": false, "generation": 1, "desktop": "ordinary",
                "display": {"id": 1, "name": "scripted", "x": 0, "y": 0, "width": 1920, "height": 1080},
                "video": {"width": 1920, "height": 1080}}),
        )
        .await;
        (connecting.await.unwrap().unwrap(), host)
    }

    fn eventually(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(Instant::now() < deadline, "condition not reached");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn hot_ops_never_wait_for_a_slow_disconnect() {
        let handle = test_handle("hot-ops");
        let handle = &handle;
        let _host = handle.runtime.block_on(async {
            let (viewer, host) = scripted_viewer().await;
            let (open, opened) = tokio::sync::oneshot::channel::<()>();
            let connecting = handle.desktop.connect_viewer(|_| async {
                opened.await.unwrap();
                Ok((viewer, invitation()))
            });
            // Hidden while connecting: the session starts paused.
            let hiding = async {
                let status = hot(handle, json!({"op": "viewer_status"}));
                assert_eq!(
                    status["data"],
                    json!({"state": "connecting", "stage": null})
                );
                assert_eq!(hot(handle, json!({"op": "pause"}))["success"], true);
                open.send(()).unwrap();
            };
            let (connected, ()) = tokio::join!(connecting, hiding);
            connected.unwrap();
            host
        });
        assert_eq!(view(handle).1.state, DesktopView::STATE_PAUSED);
        assert_eq!(hot(handle, json!({"op": "resume"}))["success"], true);

        // Live: every op reaches the viewer.
        eventually(|| view(handle).1.state == DesktopView::STATE_STREAMING);
        let (code, live) = view(handle);
        assert_eq!(code, 1);
        assert_eq!(
            (live.generation, live.display_width, live.video_height),
            (1, 1920, 1080)
        );
        assert_eq!(
            hot(handle, json!({"op": "viewer_status"}))["data"]["state"],
            "streaming"
        );
        let input =
            json!({"op": "input", "generation": 1, "inputs": [{"kind": "move", "x": 1, "y": 1}]});
        assert_eq!(hot(handle, input.clone())["code"], "NOT_FRESH");
        // This host offers no display modes, and says nothing of its OS.
        let fit =
            json!({"op": "display_mode", "target": {"fit_local": {"width": 1280, "height": 720}}});
        assert_eq!(hot(handle, fit.clone())["code"], "DISPLAY_MODE_NOT_OFFERED");
        let local = json!({"op": "viewer_status", "local": {"width": 1280, "height": 720}});
        let status = &hot(handle, local)["data"];
        assert!(status.get("modes").is_none() && status["host_os"].is_null());
        for request in [
            json!({"op": "control", "enabled": true}),
            json!({"op": "keyframe"}),
            json!({"op": "quality", "preset": "smooth"}),
            json!({"op": "pause"}),
            json!({"op": "resume"}),
            json!({"op": "release"}),
            json!({"op": "control", "enabled": false}),
        ] {
            assert_eq!(hot(handle, request.clone())["success"], true, "{request}");
        }
        assert_eq!(
            hot(
                handle,
                json!({"op": "presented", "generation": 1, "frame_id": 1})
            )["code"],
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            unsafe { pb_mapper_desktop_view_v1(std::ptr::null_mut(), &mut Default::default()) },
            -1
        );
        assert_eq!(
            unsafe {
                pb_mapper_desktop_view_v1(
                    std::ptr::from_ref(handle).cast_mut(),
                    std::ptr::null_mut(),
                )
            },
            -1
        );

        let source = handle.desktop.viewer.read().as_ref().unwrap().source;
        let consumer = desktop_video::attach(source);
        assert!(consumer > 0);
        std::thread::scope(|scope| {
            let pulling = scope.spawn(move || {
                let mut meta = desktop_video::PbDesktopVideoMetaV1::default();
                let mut buffer = [0u8; 16];
                let code = unsafe {
                    desktop_video::pb_mapper_desktop_video_next_v1(
                        consumer as u64,
                        1000,
                        buffer.as_mut_ptr(),
                        16,
                        &mut meta,
                    )
                };
                (code, Instant::now())
            });
            std::thread::sleep(Duration::from_millis(50));
            let disconnected_at = Instant::now();
            let disconnecting = scope.spawn(|| call_json(handle, json!({"op": "disconnect"})));
            // The source closes before the viewer's stop starts.
            let (code, returned) = pulling.join().unwrap();
            assert_eq!(code, desktop_video::CLOSED);
            assert!(
                returned.saturating_duration_since(disconnected_at) < Duration::from_millis(100)
            );

            // Inside the runtime, where entering it again would panic.
            handle.runtime.block_on(async {
                for (request, expected) in [
                    (
                        json!({"op": "viewer_status"}),
                        json!({"success": true, "data": null}),
                    ),
                    (
                        json!({"op": "control", "enabled": false}),
                        json!({"success": true, "data": {}}),
                    ),
                    (
                        json!({"op": "release"}),
                        json!({"success": true, "data": {}}),
                    ),
                    (
                        json!({"op": "keyframe"}),
                        json!({"success": true, "data": {"requested": false}}),
                    ),
                    (
                        json!({"op": "quality", "preset": "quality"}),
                        json!({"success": true, "data": {}}),
                    ),
                    (json!({"op": "pause"}), json!({"success": true, "data": {}})),
                    (
                        json!({"op": "resume"}),
                        json!({"success": true, "data": {}}),
                    ),
                ] {
                    let started = Instant::now();
                    assert_eq!(hot(handle, request.clone()), expected, "{request}");
                    assert!(started.elapsed() < Duration::from_millis(10), "{request}");
                }
                for request in [json!({"op": "control", "enabled": true}), input, fit] {
                    let started = Instant::now();
                    assert_eq!(hot(handle, request.clone())["code"], "IO", "{request}");
                    assert!(started.elapsed() < Duration::from_millis(10), "{request}");
                }
                let started = Instant::now();
                assert_eq!(view(handle), (0, PbDesktopViewV1::default()));
                assert!(started.elapsed() < Duration::from_millis(10));
            });
            assert!(
                !disconnecting.is_finished(),
                "the viewer's stop was meant to be still running"
            );
            assert_eq!(disconnecting.join().unwrap()["success"], true);
        });
    }

    #[test]
    fn inspect_invitation_classifies_without_network() {
        // Nothing listens on the default relay, so any network use would fail.
        let handle = test_handle("inspect");
        let inspect = |text: &str| {
            call_json(
                &handle,
                json!({"op": "inspect_invitation", "invitation": text}),
            )
        };
        let invitation = desktop::HostIdentity::generate()
            .unwrap()
            .invitation("desktop:study-pc".into())
            .unwrap();
        assert_eq!(
            inspect(&invitation.encode())["data"],
            json!({"service": "desktop:study-pc", "name": "study-pc",
                "fingerprint": invitation.short_fingerprint(), "format": "pbd1"})
        );
        let base64url = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let v1 = json!({"version": 1, "service": "desktop:study-pc",
            "certificate": base64url.encode(b"certificate"), "token": base64url.encode([7u8; 32])});
        let v1 = inspect(&v1.to_string());
        assert_eq!(
            (&v1["data"]["name"], &v1["data"]["format"]),
            (&json!("study-pc"), &json!("json_v1"))
        );
        let setup = base64::engine::general_purpose::STANDARD
            .encode(json!({"serverAddress": "relay:7666", "msgHeaderKey": "key"}).to_string());
        assert_eq!(inspect(&setup)["code"], "SETUP_STRING");
        assert_eq!(inspect("not an invitation")["code"], "INVALID_INVITATION");
    }

    #[test]
    fn background_consent_and_ui_ownership_boundaries() {
        let handle = test_handle("background-boundaries");
        handle.runtime.block_on(async {
            let config = handle.state.lock().await;
            let lifecycle = handle.desktop.host.lock().await;
            // Neither an old client nor partial consent may reach the service
            // or even wait for application configuration.
            for (privileged, remember) in [(false, true), (true, false)] {
                let request = Request::HostStart(HostStart {
                    service: "desktop:background-test".into(),
                    display_id: 1,
                    allow_control: false,
                    relay_only: false,
                    privileged,
                    remember,
                    auto_start: true,
                    background: true,
                });
                let failure = tokio::time::timeout(Duration::from_secs(1), call(&handle, request))
                    .await
                    .unwrap()
                    .unwrap_err();
                assert_eq!(failure.code(), ErrorCode::InvalidArgument);
            }

            let mut snapshot = unavailable_background("waiting for an interactive session");
            snapshot["configured"] = json!(true);
            snapshot["enabled"] = json!(true);
            handle.desktop.cache_background(snapshot.clone());
            // A blocked service mutation cannot hold up host status, which
            // reads the held lock as busy and still shows the pending
            // background grant.
            let status =
                tokio::time::timeout(Duration::from_secs(1), call(&handle, Request::HostStatus))
                    .await
                    .unwrap()
                    .unwrap();
            assert!(status["host"].is_null());
            assert_eq!(status["busy"], true);
            assert_eq!(status["background"]["enabled"], true);
            drop(lifecycle);

            // UI exit must not load legacy trust, issue background IPC, or
            // clear the service-owned grant, even while settings are locked.
            tokio::time::timeout(
                Duration::from_secs(1),
                call(&handle, Request::HostStop { on_exit: true }),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                handle.desktop.background.lock().snapshot.as_ref(),
                Some(&snapshot)
            );
            drop(config);
        });
    }

    #[tokio::test]
    async fn late_background_discovery_cannot_hide_a_new_grant() {
        let state = DesktopState::default();
        let (reply, pending) = tokio::sync::oneshot::channel();
        let mut discovery = std::pin::pin!(discover_background(&state, async {
            pending.await.map_err(err)
        }));
        assert!(
            discovery
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        let mut configured = unavailable_background("");
        configured["configured"] = json!(true);
        configured["enabled"] = json!(true);
        state.cache_background(configured.clone());
        reply.send(unavailable_background("")).unwrap();
        assert_eq!(discovery.await, configured);
        assert_eq!(state.background.lock().snapshot.as_ref(), Some(&configured));
    }

    #[test]
    fn a_host_matches_the_settings_only_on_their_relay_and_namespace() {
        let current = RelayScope {
            server: "relay:7666".into(),
            namespace: Some(1),
        };
        assert!(current.holds("relay:7666", Some(1)));
        assert!(!current.holds("old-relay:7666", Some(1)));
        assert!(!current.holds("relay:7666", Some(2)));
        // The background service's host says so; until the settings are
        // read, it says nothing.
        let mut elsewhere = unavailable_background("");
        elsewhere["configured"] = json!(true);
        elsewhere["server"] = json!("old-relay:7666");
        elsewhere["namespace"] = json!(1);
        assert_eq!(
            with_relay_match(elsewhere.clone(), Some(&current))["matches_relay"],
            false
        );
        assert_eq!(
            with_relay_match(elsewhere, None)["matches_relay"],
            Value::Null
        );
    }

    /// Resuming publishes the saved host where the settings name now: the
    /// bug was a resume that kept publishing on the relay the owner had left.
    /// Moving it there changes its invitation, so only a confirmed resume
    /// may, whatever the card showed.
    #[cfg(windows)]
    #[test]
    fn resume_configures_the_saved_host_from_the_current_settings() {
        use desktop::privileged::background::{Options, Settings};
        use pb_mapper_client::sdk::RelayTransport;
        let config = crate::state::AppConfig {
            server_address: "new-relay:7666".into(),
            relay_transport: RelayTransport::Quic,
            lan_first: false,
            ..Default::default()
        };
        let nothing = unavailable_background("");
        assert!(resume_options(&nothing, config.clone(), 1, "key".into(), true).is_err());
        let mut saved = unavailable_background("");
        saved["configured"] = json!(true);
        saved["server"] = json!("old-relay:7666");
        saved["namespace"] = json!(1);
        saved["settings"] = json!({"service": "desktop:study", "display_id": 2,
            "allow_control": true, "allow_direct": false, "auto_start": false});
        let unconfirmed = resume_options(&saved, config.clone(), 1, "new-key".into(), false);
        assert_eq!(unconfirmed.unwrap_err().code(), ErrorCode::RelayChanged);
        // The same relay in another namespace is elsewhere too; in the same
        // one it resumes unasked.
        let mut here = saved.clone();
        here["server"] = json!("new-relay:7666");
        let other = resume_options(&here, config.clone(), 2, "new-key".into(), false);
        assert_eq!(other.unwrap_err().code(), ErrorCode::RelayChanged);
        assert!(resume_options(&here, config.clone(), 1, "new-key".into(), false).is_ok());
        assert_eq!(
            resume_options(&saved, config, 1, "new-key".into(), true).unwrap(),
            Options {
                server: "new-relay:7666".into(),
                credential: "new-key".into(),
                relay_transport: RelayTransport::Quic,
                settings: Settings {
                    service: "desktop:study".into(),
                    display_id: 2,
                    display_device: None,
                    allow_control: true,
                    allow_direct: false,
                    auto_start: true,
                },
                lan_first: false,
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn revocation_waits_for_handoff_and_uses_the_actual_owner() {
        let handle = test_handle("revoke-handoff");
        handle.runtime.block_on(async {
            let directory = handle.state.lock().await.desktop_config_dir();
            let scope = trust::Scope {
                directory: directory.clone(),
                relay: "test-relay:7666".into(),
                namespace: 1,
            };
            let pending =
                trust::Saved::for_background(&scope, "desktop:handoff".into(), 1, true, None, None)
                    .unwrap();
            trust::save(&directory, &pending).unwrap();
            let lifecycle = handle.desktop.host.lock().await;
            let actual_owner = parking_lot::Mutex::new(false);
            let mut revoke = std::pin::pin!(revoke_trust_with_background(&handle, async {
                assert!(*actual_owner.lock(), "the pending handoff completed first");
                assert!(trust::load(&directory).unwrap().is_none());
                *actual_owner.lock() = false;
                Ok(unavailable_background(""))
            }));
            assert!(
                revoke
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
            // Configure completes while the UI still has no background snapshot.
            *actual_owner.lock() = true;
            assert!(handle.desktop.background.lock().snapshot.is_none());
            drop(lifecycle);
            revoke.await.unwrap();
            assert!(!*actual_owner.lock());

            // Failed service revocation stays visible, retaining the observed
            // grant; the removed local pending record cannot resurrect it.
            let configured = json!({"configured": true, "enabled": true});
            handle.desktop.cache_background(configured.clone());
            assert!(
                revoke_trust_with_background(&handle, async {
                    Err(err("service management unavailable"))
                })
                .await
                .is_err()
            );
            assert_eq!(
                handle.desktop.background.lock().snapshot.as_ref(),
                Some(&configured)
            );
            assert!(trust::load(&directory).unwrap().is_none());
            std::fs::remove_dir(&directory).unwrap();
        });
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn disabled_service_allows_local_host_but_unknown_background_stays_blocked() {
        let state = DesktopState::default();
        let mut configured = unavailable_background("");
        configured["configured"] = json!(true);
        configured["enabled"] = json!(true);
        configured["settings"] = json!({"auto_start": true});
        state.cache_background(configured);

        // A portable UI may share normally when SCM explicitly forbids service
        // restart. No management IPC is required and the saved grant survives.
        let disabled = tokio::time::timeout(
            Duration::from_secs(1),
            query_background_for_helper(
                desktop::privileged::Status {
                    installed: true,
                    startup_disabled: true,
                    ..Default::default()
                },
                std::future::pending(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let disabled = state.cache_background(disabled);
        assert!(background_configured(&disabled));
        assert!(!background_enabled(&disabled));
        assert_eq!(disabled["settings"]["auto_start"], true);
        assert_eq!(disabled["service_disabled"], true);

        // Stopped with automatic recovery pending and Running under another
        // authorized application both leave a potentially active grant unknown.
        for (running, startup_disabled) in [(false, false), (true, true)] {
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                query_background_for_helper(
                    desktop::privileged::Status {
                        installed: true,
                        running,
                        startup_disabled,
                        ..Default::default()
                    },
                    std::future::pending(),
                ),
            )
            .await
            .unwrap();
            assert!(result.is_err());
            let observed = discover_background(&state, async { result }).await;
            assert!(background_configured(&observed));
            assert_eq!(observed["service_disabled"], false);
        }

        let mut active = disabled;
        active["enabled"] = json!(true);
        active["service_disabled"] = json!(false);
        state.cache_background(active);
        // An installed/running service is not usable if its image cannot be
        // verified. Fail before management IPC and preserve the unknown grant.
        let unverified = query_background_for_helper(
            desktop::privileged::Status {
                installed: true,
                running: true,
                authorized_application: true,
                ..Default::default()
            },
            async { panic!("unverified service must not receive management requests") },
        )
        .await;
        assert!(unverified.is_err());
        let observed = discover_background(&state, async { unverified }).await;
        assert!(background_configured(&observed));
        assert!(background_enabled(&observed));
        let disconnected = query_background_for_helper(
            desktop::privileged::Status {
                installed: true,
                running: true,
                authorized_application: true,
                identity_verified: true,
                ..Default::default()
            },
            async { Err(err("management connection lost")) },
        )
        .await;
        assert!(disconnected.is_err());
        let observed = discover_background(&state, async { disconnected }).await;
        assert!(background_configured(&observed));
        assert!(background_enabled(&observed));
    }

    #[cfg(windows)]
    #[test]
    fn setup_installs_an_outdated_helper_again_and_turns_it_on() {
        use desktop::privileged::Action;
        // Of an older protocol: enabling it again would still refuse to
        // capture, and updating it alone would leave a stopped one off.
        let outdated = desktop::privileged::Status {
            installed: true,
            running: false,
            authorized_application: true,
            identity_verified: true,
            protocol: 2,
            update_required: true,
            ..Default::default()
        };
        assert_eq!(
            helper_setup_action(&outdated),
            Action::Install { enable: true }
        );
        let current = desktop::privileged::Status {
            protocol: desktop::privileged::HELPER_PROTOCOL,
            update_required: false,
            ..outdated
        };
        assert_eq!(helper_setup_action(&current), Action::Enable);
        // Another build, ready: it hosts the old code, so an action the owner
        // started installs this one; startup's restore uses it as it is.
        let other_build = desktop::privileged::Status {
            running: true,
            update_available: true,
            ..current
        };
        assert_eq!(
            helper_setup(&other_build, true),
            Some(Action::Install { enable: true })
        );
        assert_eq!(helper_setup(&other_build, false), None);
        let ready = desktop::privileged::Status {
            update_available: false,
            ..other_build
        };
        assert_eq!(helper_setup(&ready, true), None);
    }

    #[test]
    fn disconnect_cancels_configuration_wait_without_cancelling_reconnect() {
        let handle = test_handle("config-cancel");
        handle.runtime.block_on(async {
            // Contend on the real app-state lock used by the FFI call. Setup
            // must be cancelled while the lock is still held, before loading
            // credentials or contacting a relay.
            let config = handle.state.lock().await;
            let connect_at = |invitation: String, max_fps| {
                call(
                    &handle,
                    Request::Connect {
                        invitation: Some(invitation),
                        computer_id: None,
                        relay_only: false,
                        quality: QualityPreset::Balanced,
                        refresh_hz: 60.0,
                        decoders: DecoderOffer {
                            h264: Some([3840, 2160]),
                            h265: Some([2560, 1440]),
                        },
                        resolution: Some(Resolution::P1440),
                        max_fps,
                    },
                )
            };
            let connect = |invitation| connect_at(invitation, 0);
            let invalid = tokio::time::timeout(Duration::from_secs(1), connect(String::new()))
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(invalid.code(), ErrorCode::InvalidInvitation);
            let invitation = invitation().encode();
            // A frame rate no viewer asks for is refused before anything waits.
            let unbounded =
                tokio::time::timeout(Duration::from_secs(1), connect_at(invitation.clone(), 240))
                    .await
                    .unwrap()
                    .unwrap_err();
            assert_eq!(unbounded.code(), ErrorCode::InvalidArgument);
            assert!(handle.desktop.connecting.lock().is_none());
            let mut first = std::pin::pin!(connect(invitation.clone()));
            let mut context = Context::from_waker(Waker::noop());
            assert!(first.as_mut().poll(&mut context).is_pending());
            tokio::time::timeout(Duration::from_secs(1), call(&handle, Request::Disconnect))
                .await
                .unwrap()
                .unwrap();

            // A new attempt can start before the cancelled call resumes.
            // Finishing the old call must not unregister the new token.
            let mut second = std::pin::pin!(connect(invitation));
            assert!(second.as_mut().poll(&mut context).is_pending());
            assert!(
                tokio::time::timeout(Duration::from_secs(1), first)
                    .await
                    .unwrap()
                    .is_err()
            );
            assert!(handle.desktop.connecting.lock().is_some());
            call(&handle, Request::Disconnect).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(1), second)
                    .await
                    .unwrap()
                    .is_err()
            );
            drop(config);
            assert!(handle.desktop.viewer.read().is_none());
            assert!(handle.desktop.connecting.lock().is_none());
        });
    }

    /// Without a credential no retry can succeed: the attempt ends with a
    /// reason the person can act on, not an IO failure the viewer retries.
    #[test]
    fn connect_without_a_credential_ends_the_attempt() {
        let handle = test_handle("no-credential");
        let _credential_guard =
            pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK.blocking_lock();
        pb_mapper_core::checksum::set_process_msg_header_key(None).unwrap();
        let failure = handle
            .runtime
            .block_on(call(
                &handle,
                Request::Connect {
                    invitation: Some(invitation().encode()),
                    computer_id: None,
                    relay_only: false,
                    quality: QualityPreset::Balanced,
                    refresh_hz: 60.0,
                    decoders: DecoderOffer {
                        h264: None,
                        h265: None,
                    },
                    resolution: None,
                    max_fps: 0,
                },
            ))
            .unwrap_err();
        assert_eq!(failure.code(), ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn disconnect_cancels_setup_while_the_last_viewer_stops() {
        let state = Arc::new(DesktopState::default());
        let (viewer, _host) = scripted_viewer().await;
        state
            .connect_viewer(|_| async { Ok((viewer, invitation())) })
            .await
            .unwrap();
        // The last viewer's stop is held: its host never closes.
        let stopping = tokio::spawn({
            let state = state.clone();
            async move { state.disconnect_viewer().await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.viewer.read().is_some() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // A new attempt registers at once, and the next disconnect cancels it
        // without waiting for that stop.
        let (entered, started) = tokio::sync::oneshot::channel();
        let attempt = tokio::spawn({
            let state = state.clone();
            async move {
                state
                    .connect_viewer(|_| async {
                        let _ = entered.send(());
                        std::future::pending::<Result<(Viewer, Invitation), CtlError>>().await
                    })
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), started)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), state.disconnect_viewer())
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), attempt)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(!stopping.is_finished());
        stopping.await.unwrap().unwrap();
        assert!(state.viewer.read().is_none());
        assert!(state.connecting.lock().is_none());
    }

    #[tokio::test]
    async fn disconnect_cancels_pending_setup_without_waiting_for_network() {
        let state = Arc::new(DesktopState::default());
        let (entered, started) = tokio::sync::oneshot::channel();
        let connecting = state.clone();
        let attempt = tokio::spawn(async move {
            connecting
                .connect_viewer(|stage| async move {
                    stage.store(stage_code(ConnectStage::Identity), Ordering::Release);
                    let _ = entered.send(());
                    std::future::pending::<Result<(Viewer, Invitation), CtlError>>().await
                })
                .await
        });
        started.await.unwrap();
        // The attempt reports how far it got.
        assert_eq!(
            state.viewer_status(None).unwrap(),
            json!({"state": "connecting", "stage": "identity"})
        );
        tokio::time::timeout(Duration::from_secs(1), state.disconnect_viewer())
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), attempt)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(state.viewer.read().is_none());
        assert!(state.connecting.lock().is_none());
        assert_eq!(state.viewer_status(None).unwrap(), Value::Null);
    }
}
