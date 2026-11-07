//! JSON control plus explicitly owned binary frames. Both are called on Dart
//! workers. Temporary endpoints belong to the app runtime, never a page widget;
//! explicitly configured background hosts belong to the Windows service.
use crate::{
    error::CtlError,
    handle::PbMapperHandle,
    response::{err_ctl, err_null_handle, ok_data, parse_c_string},
};
use pb_mapper::{
    Client, ConnectOptions, PathPolicy,
    desktop::{self, Host, HostOptions, Invitation, Viewer},
};
use pb_mapper_core::checksum::get_process_credential;
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::c_char;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
#[path = "desktop_trust.rs"]
mod trust;

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

#[derive(Default)]
pub(crate) struct DesktopState {
    // Outbound connection setup must never hold the local host's stop/revoke
    // lock. Each endpoint owns its lifecycle independently.
    host: tokio::sync::Mutex<Option<Host>>,
    viewer: tokio::sync::Mutex<Option<Viewer>>,
    connecting: parking_lot::Mutex<Option<(u64, CancellationToken)>>,
    viewer_epoch: AtomicU64,
    // Guarded by the host lifecycle lock. Retain even an ephemeral local key
    // while its host lives so ownership can move to the service unchanged.
    host_identity: tokio::sync::Mutex<Option<LocalIdentity>>,
    background: Arc<parking_lot::Mutex<BackgroundCache>>,
}
#[derive(Clone)]
struct LocalIdentity {
    scope: trust::Scope,
    #[cfg(windows)]
    identity: desktop::HostIdentity,
    remembered: bool,
}
impl DesktopState {
    fn cache_background(&self, snapshot: Value) -> Value {
        self.background.lock().store(snapshot)
    }

    fn background_status(&self) -> Option<Value> {
        let mut cache = self.background.lock();
        let snapshot = cache.snapshot.clone();
        // Discovery is explicit. An unused helper adds no IPC to viewer polls.
        if snapshot.as_ref().is_some_and(background_configured)
            && !cache.refreshing
            && cache
                .checked
                .is_none_or(|time| time.elapsed() >= Duration::from_secs(2))
        {
            cache.refreshing = true;
            let revision = cache.revision;
            let shared = self.background.clone();
            tokio::spawn(async move {
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

    async fn connect_viewer(
        &self,
        connect: impl std::future::Future<Output = Result<Viewer, CtlError>>,
    ) -> Result<(), CtlError> {
        // Register before any await, including the viewer/configuration locks:
        // a disconnect must invalidate setup even before network work starts.
        let cancel = CancellationToken::new();
        let epoch = {
            let mut pending = self.connecting.lock();
            if pending.is_some() {
                return Err(err("desktop connection is already pending"));
            }
            let epoch = self.viewer_epoch.fetch_add(1, Ordering::AcqRel) + 1;
            *pending = Some((epoch, cancel.clone()));
            epoch
        };
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(err("desktop connection cancelled")),
            result = async {
                if self.viewer.lock().await.is_some() {
                    return Err(err("disconnect the current desktop first"));
                }
                connect.await
            } => result,
        };
        {
            let mut pending = self.connecting.lock();
            if pending.as_ref().is_some_and(|(id, _)| *id == epoch) {
                pending.take();
            }
        }
        let viewer = result?;
        let mut slot = self.viewer.lock().await;
        if cancel.is_cancelled()
            || self.viewer_epoch.load(Ordering::Acquire) != epoch
            || slot.is_some()
        {
            drop(slot);
            viewer.stop().await.map_err(err)?;
            return Err(err("desktop connection cancelled"));
        }
        *slot = Some(viewer);
        Ok(())
    }
    async fn disconnect_viewer(&self) -> Result<(), CtlError> {
        {
            let mut pending = self.connecting.lock();
            self.viewer_epoch.fetch_add(1, Ordering::AcqRel);
            if let Some((_, token)) = pending.take() {
                token.cancel();
            }
        }
        if let Some(viewer) = self.viewer.lock().await.take() {
            viewer.stop().await.map_err(err)?;
        }
        Ok(())
    }
    async fn wait_frame(&self) -> Option<desktop::Frame> {
        let (mut changes, epoch) = {
            let slot = self.viewer.lock().await;
            let viewer = slot.as_ref()?;
            let mut changes = viewer.frames();
            changes.borrow_and_update();
            if let Some(frame) = viewer.next_frame() {
                return Some(frame);
            }
            (changes, self.viewer_epoch.load(Ordering::Acquire))
        };
        // Release the viewer lock while waiting: disconnect and input must not
        // queue behind an idle renderer. Bound the wait so the Dart worker can
        // service cancellation and feedback even on a static desktop.
        tokio::time::timeout(std::time::Duration::from_millis(16), changes.changed())
            .await
            .ok()?
            .ok()?;
        let slot = self.viewer.lock().await;
        if self.viewer_epoch.load(Ordering::Acquire) != epoch {
            return None;
        }
        slot.as_ref()?.next_frame()
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Capabilities,
    Status,
    TrustStatus,
    TrustRevoke,
    HostRestore,
    HostEnable {
        service: String,
        display_id: u32,
    },
    HelperAction {
        action: String,
    },
    BackgroundAction {
        action: String,
    },
    HostStart {
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
    },
    HostStop {
        #[serde(default)]
        on_exit: bool,
    },
    HostPermission {
        allow_control: bool,
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
    Disconnect,
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
}
fn err(e: impl std::fmt::Display) -> CtlError {
    CtlError::io(e.to_string())
}

fn background_configured(snapshot: &Value) -> bool {
    snapshot["configured"].as_bool().unwrap_or(false)
}

fn background_enabled(snapshot: &Value) -> bool {
    snapshot["enabled"].as_bool().unwrap_or(false)
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

fn background_host(snapshot: &Value) -> Option<Value> {
    if !background_enabled(snapshot) {
        return None;
    }
    if !snapshot["host"].is_null() {
        return Some(snapshot["host"].clone());
    }
    // The service may be waiting for a console session or retrying the relay.
    // It still owns an enabled grant, so the global Stop action must stay live.
    Some(json!(desktop::SessionStatus {
        state: "waiting".into(),
        message: snapshot["message"].as_str().unwrap_or_default().into(),
        control_allowed: snapshot["settings"]["allow_control"]
            .as_bool()
            .unwrap_or(false),
        ..Default::default()
    }))
}

async fn query_background() -> Result<Value, CtlError> {
    #[cfg(windows)]
    {
        let helper = tokio::task::spawn_blocking(desktop::helper::status)
            .await
            .map_err(err)?;
        query_background_for_helper(helper, async {
            let snapshot = desktop::helper::background::status().await.map_err(err)?;
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
    helper: desktop::helper::Status,
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

async fn background_action(handle: &PbMapperHandle, action: &str) -> Result<Value, CtlError> {
    if action == "status" {
        return Ok(discover_background(&handle.desktop, query_background()).await);
    }
    if !matches!(action, "start" | "stop" | "revoke") {
        return Err(CtlError::invalid_argument("unknown background action"));
    }
    #[cfg(windows)]
    {
        let slot = handle.desktop.host.lock().await;
        let snapshot = match action {
            "start" => {
                if slot.is_some() {
                    return Err(err(
                        "stop local desktop sharing before starting the background host",
                    ));
                }
                disable_legacy_startup(handle).await?;
                desktop::helper::background::start().await
            }
            "stop" => {
                let disabled = disable_legacy_startup(handle).await;
                let stopped = desktop::helper::background::stop().await;
                disabled?;
                stopped
            }
            "revoke" => {
                // Retire any pending handoff before the service grant so a
                // later app startup cannot recreate explicitly revoked access.
                let directory = handle.state.lock().await.desktop_config_dir();
                let retired = tokio::task::spawn_blocking(move || trust::revoke(&directory))
                    .await
                    .map_err(err)?;
                let revoked = desktop::helper::background::revoke().await;
                retired?;
                revoked
            }
            _ => unreachable!(),
        }
        .map_err(err)?;
        Ok(handle
            .desktop
            .cache_background(serde_json::to_value(snapshot).map_err(err)?))
    }
    #[cfg(not(windows))]
    Err(err("background hosting requires Windows"))
}

async fn stop_background(state: &DesktopState) -> Result<(), CtlError> {
    let snapshot = query_background().await?;
    #[cfg(windows)]
    if background_configured(&snapshot) && snapshot["service_disabled"] != true {
        let snapshot = desktop::helper::background::stop().await.map_err(err)?;
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
        let snapshot = desktop::helper::background::revoke_scope(server, namespace)
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
    let stopped = if let Some(host) = slot.take() {
        host.stop().await.map_err(err)
    } else {
        Ok(())
    };
    *state.host_identity.lock().await = None;
    stopped?;
    revoked?;
    Ok(())
}

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
        serde_json::from_str::<Request>(&text)
            .map_err(|_| CtlError::invalid_argument("invalid desktop request"))
    }) {
        Ok(request) => request,
        Err(e) => return err_ctl(&e),
    };
    let handle = unsafe { &*handle };
    match handle.runtime.block_on(call(handle, request)) {
        Ok(value) => ok_data(value),
        Err(e) => err_ctl(&e),
    }
}

async fn call(handle: &PbMapperHandle, request: Request) -> Result<Value, CtlError> {
    if matches!(request, Request::Capabilities) {
        return tokio::task::spawn_blocking(|| {
            let displays = desktop::displays();
            #[cfg(windows)]
            let helper = serde_json::to_value(desktop::helper::status()).ok();
            #[cfg(not(windows))]
            let helper: Option<Value> = None;
            json!({"capabilities": desktop::capabilities(), "helper": helper, "displays": displays.as_ref().ok(), "message": displays.err().map(|e| e.to_string())})
        }).await.map_err(err);
    }
    if let Request::HelperAction { action } = request {
        #[cfg(windows)]
        {
            // Enabling/reinstalling may resume a saved service grant. Keep it
            // serialized with local starts until the helper process exits.
            // Old callers used `open` to spawn a second UI. Always install the
            // current bundle instead, then replace this UI after a ready ack.
            let action = if action == "open" {
                "install".into()
            } else {
                action
            };
            let local = handle.desktop.host.lock().await;
            if matches!(action.as_str(), "enable" | "install") && local.is_some() {
                return Err(err(
                    "stop local sharing before enabling the background service",
                ));
            }
            let restart = action == "install";
            tokio::task::spawn_blocking(move || desktop::helper::request_installation(&action))
                .await
                .map_err(err)?
                .map_err(err)?;
            if restart {
                desktop::helper::prepare_application_restart()
                    .await
                    .map_err(err)?;
            }
            return Ok(json!({"restart": restart}));
        }
        #[cfg(not(windows))]
        {
            let _ = action;
            return Err(err("the helper requires Windows"));
        }
    }
    let state = &handle.desktop;
    match request {
        Request::TrustStatus => {
            let (directory, relay) = {
                let state = handle.state.lock().await;
                (
                    state.desktop_config_dir(),
                    state.get_config_status().await.server_address,
                )
            };
            let saved = tokio::task::spawn_blocking(move || trust::load(&directory))
                .await
                .map_err(err)??;
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
            return Ok(
                json!({"supported": trust::supported(), "saved": saved.as_ref().map(trust::Saved::metadata), "matches_relay": matches}),
            );
        }
        Request::TrustRevoke => {
            revoke_trust_with_background(handle, revoke_background(handle)).await?;
        }
        Request::Status => {
            let background = state.background_status();
            // A slow helper management call holds the host lifecycle lock, but
            // must never delay the independent remote viewer's status.
            let host = state
                .host
                .try_lock()
                .ok()
                .and_then(|slot| {
                    slot.as_ref()
                        .map(|host| json!(desktop::legacy_host_status(&host.status())))
                })
                .or_else(|| background.as_ref().and_then(background_host));
            let viewer = state.viewer.lock().await.as_ref().map(Viewer::status);
            return Ok(json!({"host": host, "viewer": viewer, "background": background}));
        }
        Request::BackgroundAction { action } => {
            return background_action(handle, &action).await;
        }
        Request::HostEnable {
            service,
            display_id,
        } => {
            return enable_host(handle, service, display_id, true).await;
        }
        Request::HostRestore => {
            let directory = handle.state.lock().await.desktop_config_dir();
            let saved = tokio::task::spawn_blocking(move || trust::load(&directory))
                .await
                .map_err(err)??;
            if let Some(saved) = saved.filter(|saved| saved.settings.background) {
                return enable_host(
                    handle,
                    saved.settings.service,
                    saved.settings.display_id,
                    false,
                )
                .await;
            }
            return start_host(handle, Request::HostRestore).await;
        }
        start @ Request::HostStart { .. } => {
            return start_host(handle, start).await;
        }
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
                            desktop::helper::background::set_control_allowed(allow_control)
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
        Request::Connect {
            invitation,
            relay_only,
            native_video,
            video,
        } => {
            let invitation = Invitation::decode(&invitation)
                .map_err(|failure| CtlError::invalid_argument(failure.to_string()))?;
            state
                .connect_viewer(async {
                    // Configuration can wait behind other app operations. Keep
                    // that wait inside the registered, cancellable attempt.
                    let config = handle.state.lock().await.get_config_status().await;
                    let client = Client::from_credential(
                        &config.server_address,
                        get_process_credential().map_err(err)?,
                        config.keep_alive_enabled,
                        None,
                    )
                    .with_relay_transport(config.relay_transport)
                    .map_err(err)?;
                    let options = ConnectOptions {
                        path: if relay_only {
                            PathPolicy::RelayOnly
                        } else {
                            PathPolicy::Auto
                        },
                        ..Default::default()
                    };
                    let preferences = video.unwrap_or_default();
                    if native_video {
                        Viewer::connect_with_presentation_feedback(
                            &client,
                            invitation,
                            options,
                            vec![desktop::VideoCodec::H264, desktop::VideoCodec::Jpeg],
                            preferences,
                        )
                        .await
                    } else {
                        Viewer::connect_with_video_preferences(
                            &client,
                            invitation,
                            options,
                            vec![desktop::VideoCodec::Jpeg],
                            preferences,
                        )
                        .await
                    }
                    .map_err(err)
                })
                .await?;
        }
        Request::Disconnect => {
            state.disconnect_viewer().await?;
        }
        Request::Control { enabled } => state
            .viewer
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| err("desktop is disconnected"))?
            .request_control(enabled)
            .map_err(err)?,
        Request::Release => state
            .viewer
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| err("desktop is disconnected"))?
            .release_input()
            .map_err(err)?,
        Request::Keyframe => state
            .viewer
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| err("desktop is disconnected"))?
            .request_keyframe(),
        Request::Input { event } => state
            .viewer
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| err("desktop is disconnected"))?
            .input(event)
            .map_err(err)?,
        Request::Presented {
            generation,
            frame_id,
        } => state
            .viewer
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| err("desktop is disconnected"))?
            .presented(generation, frame_id),
        Request::Capabilities | Request::HelperAction { .. } => unreachable!(),
    }
    Ok(json!({}))
}

async fn start_host(handle: &PbMapperHandle, request: Request) -> Result<Value, CtlError> {
    if let Request::HostStart {
        background: true,
        privileged,
        remember,
        ..
    } = &request
    {
        if !privileged || !remember {
            return Err(CtlError::invalid_argument(
                "background hosting requires explicit helper and remembered-trust consent",
            ));
        }
        #[cfg(not(windows))]
        return Err(err("background hosting requires Windows"));
    }
    // Serialize both owners before loading configuration or talking to the
    // service. A concurrent Stop cannot finish and then be undone by this start.
    let mut slot = handle.desktop.host.lock().await;
    if slot.is_some() {
        return Err(err("desktop is already shared"));
    }
    let background = handle.desktop.cache_background(query_background().await?);
    if matches!(request, Request::HostRestore) && background_configured(&background) {
        return Err(err(
            "use the configured background host instead of restoring local sharing",
        ));
    }
    if !matches!(
        request,
        Request::HostStart {
            background: true,
            ..
        }
    ) && background_configured(&background)
    {
        return Err(err(
            "this desktop belongs to the background service; enable it to reuse its invitation",
        ));
    }
    let (directory, config) = {
        let state = handle.state.lock().await;
        (state.desktop_config_dir(), state.get_config_status().await)
    };
    let credential = get_process_credential().map_err(err)?;
    #[cfg(windows)]
    if let Request::HostStart {
        background: true,
        service,
        display_id,
        allow_control,
        relay_only,
        auto_start,
        ..
    } = &request
    {
        let namespace = credential.key_id();
        let credential = match credential {
            pb_mapper_core::checksum::Credential::Admin(key) => String::from_utf8(key.to_vec())
                .map_err(|_| err("invalid administrator credential encoding"))?,
            pb_mapper_core::checksum::Credential::Temporary { key_id, key } => {
                pb_mapper_core::checksum::encode_temporary_credential(key_id, &key)
            }
        };
        // Keep the old identity for an explicit restore, but never allow a
        // later app launch to resurrect its independent automatic grant.
        disable_legacy_startup(handle).await?;
        let legacy = tokio::task::spawn_blocking({
            let directory = directory.clone();
            move || trust::load(&directory)
        })
        .await
        .map_err(err)??;
        let options = desktop::helper::background::Options {
            server: config.server_address,
            credential,
            relay_transport: config.relay_transport,
            settings: desktop::helper::background::Settings {
                service: service.clone(),
                display_id: *display_id,
                allow_control: *allow_control,
                allow_direct: !relay_only,
                auto_start: *auto_start,
            },
        };
        let scope = trust::Scope {
            directory: directory.clone(),
            relay: options.server.clone(),
            namespace,
        };
        let snapshot = if let Some(saved) =
            legacy.filter(|saved| saved.matches(&scope) && saved.settings.service == *service)
        {
            desktop::helper::background::configure_with_identity(options, saved.identity()?).await
        } else {
            desktop::helper::background::configure(options).await
        }
        .map_err(err)?;
        let snapshot = handle
            .desktop
            .cache_background(serde_json::to_value(snapshot).map_err(err)?);
        *handle.desktop.host_identity.lock().await = None;
        tokio::task::spawn_blocking(move || trust::revoke(&directory))
            .await
            .map_err(err)??;
        return Ok(json!({"invitation": snapshot["invitation"], "background": snapshot}));
    }
    let scope = trust::Scope {
        directory,
        relay: config.server_address.clone(),
        namespace: credential.key_id(),
    };
    let client = Client::from_credential(
        &config.server_address,
        credential,
        config.keep_alive_enabled,
        None,
    )
    .with_relay_transport(config.relay_transport)
    .map_err(err)?;
    let saved = if matches!(
        request,
        Request::HostRestore | Request::HostStart { remember: true, .. }
    ) {
        let directory = scope.directory.clone();
        tokio::task::spawn_blocking(move || trust::load(&directory))
            .await
            .map_err(err)??
    } else {
        None
    };
    let (settings, remember) = match request {
        Request::HostStart {
            service,
            display_id,
            allow_control,
            relay_only,
            privileged,
            remember,
            auto_start,
            background: _,
        } => {
            if auto_start && !remember {
                return Err(err(
                    "automatic sharing requires explicitly remembered trust",
                ));
            }
            (
                trust::Settings {
                    service,
                    display_id,
                    allow_control,
                    relay_only,
                    privileged,
                    auto_start,
                    background: false,
                },
                remember,
            )
        }
        Request::HostRestore => {
            let saved = saved
                .as_ref()
                .ok_or_else(|| err("no remembered desktop trust"))?;
            if !saved.matches(&scope) {
                return Err(err(
                    "saved desktop belongs to another relay or namespace; restore the original connection settings first",
                ));
            }
            (saved.settings.clone(), true)
        }
        _ => unreachable!(),
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
    let display_id = settings.display_id;
    let options = HostOptions {
        service: settings.service.clone(),
        display_id,
        allow_control: settings.allow_control,
        allow_direct: !settings.relay_only,
    };
    let persisted = remember
        .then(|| trust::Saved::new(&scope, settings.clone(), &identity))
        .transpose()?;
    let local_identity = LocalIdentity {
        scope,
        #[cfg(windows)]
        identity: identity.clone(),
        remembered: remember,
    };
    let host = if settings.privileged {
        #[cfg(windows)]
        {
            desktop::helper::status().ensure_ready().map_err(err)?;
            Host::start_with_backend_and_identity(
                &client,
                options,
                std::sync::Arc::new(move || desktop::helper::backend(display_id)),
                None,
                identity,
            )
            .await
            .map_err(err)?
        }
        #[cfg(not(windows))]
        {
            return Err(err("the helper requires Windows"));
        }
    } else {
        Host::start_with_identity(&client, options, identity)
            .await
            .map_err(err)?
    };
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

/// The product has one durable host owner. Setup intent and any existing local
/// identity survive the installed application's restart; only the service owns
/// registration after the handoff. Internal SDK/local-host APIs remain separate.
#[cfg(windows)]
async fn enable_host(
    handle: &PbMapperHandle,
    service: String,
    display_id: u32,
    allow_install: bool,
) -> Result<Value, CtlError> {
    let mut slot = handle.desktop.host.lock().await;
    let (directory, config) = {
        let state = handle.state.lock().await;
        (state.desktop_config_dir(), state.get_config_status().await)
    };
    let credential = get_process_credential().map_err(err)?;
    let scope = trust::Scope {
        directory: directory.clone(),
        relay: config.server_address.clone(),
        namespace: credential.key_id(),
    };
    let previous = tokio::task::spawn_blocking({
        let directory = directory.clone();
        move || trust::load(&directory)
    })
    .await
    .map_err(err)??;
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
        previous.as_ref(),
        local.as_ref().map(|local| &local.identity),
    )?;
    let identity = pending.identity()?;
    tokio::task::spawn_blocking({
        let directory = directory.clone();
        move || trust::save(&directory, &pending)
    })
    .await
    .map_err(err)??;
    // Stop the old registration before enabling a service that could already
    // hold an automatic grant. No two owners may publish the same desktop.
    if let Some(host) = slot.take() {
        host.stop().await.map_err(err)?;
    }
    *handle.desktop.host_identity.lock().await = None;
    let helper = tokio::task::spawn_blocking(desktop::helper::status)
        .await
        .map_err(err)?;
    if helper.ensure_ready().is_err() {
        if !allow_install {
            return Err(err(
                "desktop setup is waiting for the installed background service",
            ));
        }
        let action = if !helper.installed
            || !helper.authorized_application
            || (helper.running && !helper.identity_verified)
        {
            "install"
        } else {
            "enable"
        };
        let installed =
            tokio::task::spawn_blocking(move || desktop::helper::request_installation(action))
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
        if action == "install" {
            desktop::helper::prepare_application_restart()
                .await
                .map_err(err)?;
            return Ok(json!({"restart": true}));
        }
    }
    let credential = match credential {
        pb_mapper_core::checksum::Credential::Admin(key) => String::from_utf8(key.to_vec())
            .map_err(|_| err("invalid administrator credential encoding"))?,
        pb_mapper_core::checksum::Credential::Temporary { key_id, key } => {
            pb_mapper_core::checksum::encode_temporary_credential(key_id, &key)
        }
    };
    let snapshot = desktop::helper::background::configure_with_identity(
        desktop::helper::background::Options {
            server: config.server_address,
            credential,
            relay_transport: config.relay_transport,
            settings: desktop::helper::background::Settings {
                service,
                display_id,
                allow_control: true,
                allow_direct: true,
                auto_start: true,
            },
        },
        identity,
    )
    .await
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

#[cfg(not(windows))]
async fn enable_host(_: &PbMapperHandle, _: String, _: u32, _: bool) -> Result<Value, CtlError> {
    Err(err("persistent desktop hosting currently requires Windows"))
}

/// One allocation containing u32 little-endian metadata length, UTF-8 status,
/// and encoded video bytes. Media is never encoded as JSON/base64. A null
/// pointer means no newer frame; free every non-null returned pointer.
#[repr(C)]
pub struct DesktopBuffer {
    pub data: *const u8,
    pub len: usize,
}

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
    let frame = handle.runtime.block_on(async {
        handle
            .desktop
            .viewer
            .lock()
            .await
            .as_ref()
            .and_then(Viewer::next_frame)
    });
    export_frame(frame)
}

/// Bounded wait variant for a dedicated media worker. The buffer ownership is
/// identical to `pb_mapper_desktop_frame`; never call this on the UI thread.
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
    export_frame(handle.runtime.block_on(handle.desktop.wait_frame()))
}

fn export_frame(frame: Option<desktop::Frame>) -> *mut DesktopBuffer {
    // next_frame consumes an event exactly once. Cursor/status events can share
    // the next picture's frame id; filtering by the caller's last id would lose
    // observations or a predictive access unit and corrupt its reference chain.
    // Retain the legacy arguments in the C ABI, but let the queue own delivery.
    let Some(frame) = frame else {
        return std::ptr::null_mut();
    };
    let Ok(metadata) = serde_json::to_vec(&frame.status) else {
        return std::ptr::null_mut();
    };
    let mut data = Vec::with_capacity(4 + metadata.len() + frame.bytes.len());
    data.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    data.extend_from_slice(&metadata);
    data.extend_from_slice(&frame.bytes);
    let data = data.into_boxed_slice();
    let buffer = DesktopBuffer {
        len: data.len(),
        data: Box::into_raw(data).cast::<u8>(),
    };
    Box::into_raw(Box::new(buffer))
}

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
    use super::*;
    use std::{
        ffi::{CStr, CString},
        future::Future,
        sync::Arc,
        task::{Context, Waker},
    };

    fn test_handle(name: &str) -> PbMapperHandle {
        let root = std::env::temp_dir().join(format!("pb-desktop-{name}-{}", std::process::id()));
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

    #[test]
    fn status_and_video_with_the_same_id_both_cross_the_binary_boundary() {
        let status = desktop::SessionStatus {
            state: "streaming".into(),
            generation: 3,
            frame_id: 9,
            cursor: Some(desktop::CursorPosition {
                x: 4,
                y: 5,
                visible: true,
            }),
            ..Default::default()
        };
        for payload in [vec![], vec![1, 2, 3]] {
            let buffer = export_frame(Some(desktop::Frame {
                status: status.clone(),
                bytes: payload.clone().into(),
            }));
            assert!(!buffer.is_null());
            let bytes = unsafe { std::slice::from_raw_parts((*buffer).data, (*buffer).len) };
            let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
            let metadata: Value = serde_json::from_slice(&bytes[4..4 + length]).unwrap();
            assert_eq!(metadata["cursor"]["x"], 4);
            assert_eq!(&bytes[4 + length..], payload);
            unsafe { pb_mapper_desktop_free_frame(buffer) };
        }
        assert!(export_frame(None).is_null());
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
                let request = Request::HostStart {
                    service: "desktop:background-test".into(),
                    display_id: 1,
                    allow_control: false,
                    relay_only: false,
                    privileged,
                    remember,
                    auto_start: true,
                    background: true,
                };
                let failure = tokio::time::timeout(Duration::from_secs(1), call(&handle, request))
                    .await
                    .unwrap()
                    .unwrap_err();
                assert_eq!(failure.code(), crate::error::ErrorCode::InvalidArgument);
            }

            let mut snapshot = unavailable_background("waiting for an interactive session");
            snapshot["configured"] = json!(true);
            snapshot["enabled"] = json!(true);
            handle.desktop.cache_background(snapshot.clone());
            // A blocked service mutation cannot hold up remote viewer status;
            // the pending background grant still exposes the global Stop state.
            let status =
                tokio::time::timeout(Duration::from_secs(1), call(&handle, Request::Status))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(status["host"]["state"], "waiting");
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
                trust::Saved::for_background(&scope, "desktop:handoff".into(), 1, None, None)
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
                desktop::helper::Status {
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
                    desktop::helper::Status {
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
            desktop::helper::Status {
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
            desktop::helper::Status {
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

    #[test]
    fn disconnect_cancels_configuration_wait_without_cancelling_reconnect() {
        let handle = test_handle("config-cancel");
        handle.runtime.block_on(async {
            // Contend on the real app-state lock used by the FFI call. Setup
            // must be cancelled while the lock is still held, before loading
            // credentials or contacting a relay.
            let config = handle.state.lock().await;
            let invalid = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                call(
                    &handle,
                    Request::Connect {
                        invitation: String::new(),
                        relay_only: false,
                        native_video: false,
                        video: None,
                    },
                ),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert_eq!(invalid.code(), crate::error::ErrorCode::InvalidArgument);
            assert!(handle.desktop.connecting.lock().is_none());
            let invitation = desktop::HostIdentity::generate()
                .unwrap()
                .invitation("desktop:test".into())
                .unwrap()
                .encode();
            let connect = || {
                call(
                    &handle,
                    Request::Connect {
                        invitation: invitation.clone(),
                        relay_only: false,
                        native_video: false,
                        video: None,
                    },
                )
            };
            let mut first = std::pin::pin!(connect());
            let mut context = Context::from_waker(Waker::noop());
            assert!(first.as_mut().poll(&mut context).is_pending());
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                call(&handle, Request::Disconnect),
            )
            .await
            .unwrap()
            .unwrap();

            // A new attempt can start before the cancelled call resumes.
            // Finishing the old call must not unregister the new token.
            let mut second = std::pin::pin!(connect());
            assert!(second.as_mut().poll(&mut context).is_pending());
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(1), first)
                    .await
                    .unwrap()
                    .is_err()
            );
            assert!(handle.desktop.connecting.lock().is_some());
            call(&handle, Request::Disconnect).await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(1), second)
                    .await
                    .unwrap()
                    .is_err()
            );
            drop(config);
            assert!(handle.desktop.viewer.lock().await.is_none());
            assert!(handle.desktop.connecting.lock().is_none());
        });
    }

    #[tokio::test]
    async fn disconnect_cancels_setup_waiting_for_viewer_lock() {
        let state = DesktopState::default();
        let viewer = state.viewer.lock().await;
        let mut attempt = std::pin::pin!(
            state.connect_viewer(std::future::pending::<Result<Viewer, CtlError>>())
        );
        let mut context = Context::from_waker(Waker::noop());
        assert!(attempt.as_mut().poll(&mut context).is_pending());
        let mut disconnect = std::pin::pin!(state.disconnect_viewer());
        assert!(disconnect.as_mut().poll(&mut context).is_pending());
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), attempt)
                .await
                .unwrap()
                .is_err()
        );
        drop(viewer);
        disconnect.await.unwrap();
        assert!(state.viewer.lock().await.is_none());
        assert!(state.connecting.lock().is_none());
    }

    #[tokio::test]
    async fn disconnect_cancels_pending_setup_without_waiting_for_network() {
        let state = Arc::new(DesktopState::default());
        let (entered, started) = tokio::sync::oneshot::channel();
        let connecting = state.clone();
        let attempt = tokio::spawn(async move {
            connecting
                .connect_viewer(async {
                    let _ = entered.send(());
                    std::future::pending::<Result<Viewer, CtlError>>().await
                })
                .await
        });
        started.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), state.disconnect_viewer())
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), attempt)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(state.viewer.lock().await.is_none());
        assert!(state.connecting.lock().is_none());
    }
    struct TestDesktop;
    impl desktop::Backend for TestDesktop {
        fn display(&mut self) -> desktop::Result<desktop::Display> {
            Ok(desktop::Display {
                id: 1,
                name: "FFI fixture".into(),
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            })
        }
        fn capture(&mut self) -> desktop::Result<Vec<u8>> {
            Ok(vec![1, 2, 3, 4])
        }
        fn input(&mut self, _: &desktop::Input) -> desktop::Result<()> {
            Ok(())
        }
        fn release(&mut self) {}
    }
    #[test]
    #[ignore = "v1 pipeline; rewritten for v2 after the G1 merges"]
    fn binary_frame_is_owned_until_explicit_release_after_disconnect() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _credential =
            runtime.block_on(pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK.lock());
        let root = std::env::temp_dir().join(format!("pb-desktop-ffi-{}", std::process::id()));
        let mut handle = PbMapperHandle {
            desktop: Default::default(),
            tunnel_events: Default::default(),
            state: Arc::new(tokio::sync::Mutex::new(crate::state::PbMapperState::new(
                Some(root.display().to_string()),
            ))),
            control_shutdown: Default::default(),
            runtime,
        };
        let relay = handle.runtime.block_on(async {
            let relay = pb_mapper_testkit::Relay::start("desktop-ffi").await;
            let client = Client::from_credential(
                relay.addr().to_string(),
                pb_mapper_testkit::admin_credential(),
                true,
                None,
            );
            let host = Host::start_with_backend(
                &client,
                HostOptions {
                    service: "desktop:ffi".into(),
                    display_id: 1,
                    allow_control: false,
                    allow_direct: false,
                },
                Arc::new(|| Ok(Box::new(TestDesktop))),
            )
            .await
            .unwrap();
            let viewer = Viewer::connect(
                &client,
                host.invitation().clone(),
                ConnectOptions {
                    path: PathPolicy::RelayOnly,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let mut frames = viewer.frames();
            tokio::time::timeout(std::time::Duration::from_secs(5), frames.changed())
                .await
                .unwrap()
                .unwrap();
            *handle.desktop.host.lock().await = Some(host);
            *handle.desktop.viewer.lock().await = Some(viewer);
            relay
        });
        let buffer = unsafe { pb_mapper_desktop_wait_frame(&mut handle, 0, 0) };
        assert!(!buffer.is_null());
        let disconnect = CString::new(r#"{"op":"disconnect"}"#).unwrap();
        let reply = unsafe { pb_mapper_desktop_call_json(&mut handle, disconnect.as_ptr()) };
        let response: Value =
            serde_json::from_slice(unsafe { CStr::from_ptr(reply) }.to_bytes()).unwrap();
        unsafe {
            crate::logging::pb_mapper_free_string(reply);
        }
        assert_eq!(response["success"], true);
        assert!(unsafe { pb_mapper_desktop_wait_frame(&mut handle, 0, 0) }.is_null());
        let bytes = unsafe { std::slice::from_raw_parts((*buffer).data, (*buffer).len) };
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let metadata: Value = serde_json::from_slice(&bytes[4..4 + length]).unwrap();
        assert_eq!(metadata["display"]["width"], 10);
        assert_eq!(&bytes[4 + length..], &[1, 2, 3, 4]);
        unsafe {
            pb_mapper_desktop_free_frame(buffer);
        }
        handle.runtime.block_on(async {
            // An outbound operation may hold its viewer slot while waiting
            // for a network peer. Local stop must remain available meanwhile.
            let _outbound = handle.desktop.viewer.lock().await;
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                call(&handle, Request::HostStop { on_exit: true }),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(handle.desktop.host.lock().await.is_none());
            drop(relay);
        });
    }
}
