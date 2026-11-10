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
    error::CtlError,
    handle::PbMapperHandle,
    response::{err_ctl, err_null_handle, ok_data, parse_c_string},
};
use pb_mapper::desktop::{
    self, Host, Invitation, QualityPreset, Resolution, StopReason, ViewerConfig,
};
use pb_mapper_core::checksum::get_process_credential;
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::c_char;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

mod background;
mod host;
pub(crate) mod trust;
mod viewer;

use background::{
    BackgroundCache, BackgroundVerb, HelperVerb, background_action, enable_host, helper_action,
    revoke_background, start_background, stop_background, with_relay_match,
};
#[cfg(windows)]
use background::{background_enabled, query_background};
use host::{
    HostStart, LocalIdentity, Start, disable_legacy_startup, load_trust, restart_host,
    revoke_trust_with_background, start_local,
};
pub(crate) use viewer::Connected;
use viewer::{DecoderOffer, Live, PendingConnect, Target, connect, inspection, invitation_error};
pub use viewer::{PbDesktopViewV1, pb_mapper_desktop_view_v1, pb_mapper_desktop_viewer_call_json};

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
            server: state.get_config_status().server_address,
            namespace: get_process_credential()
                .ok()
                .map(|credential| credential.key_id()),
        };
        *self.relay.lock() = Some(scope.clone());
        Some(scope)
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

fn err(e: impl std::fmt::Display) -> CtlError {
    CtlError::io(e.to_string())
}

/// A maximum frame rate as `connect` and a remembered computer take it: 0
/// for Auto, otherwise one the user chose, 30..=120.
pub(crate) fn valid_max_fps(max_fps: u32) -> bool {
    max_fps == 0 || (30..=120).contains(&max_fps)
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
    // SAFETY: null or NUL-terminated, as the caller guarantees.
    let request = match unsafe { parse_c_string(request, "request") }.and_then(|text| {
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
                    state.get_config_status().server_address,
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
