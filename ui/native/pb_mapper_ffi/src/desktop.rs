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
use pb_mapper::desktop::{self, Invitation};
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
    revoke_background, start_background,
};
use host::{
    HostChange, HostSlot, HostStart, Start, host_end_session, host_permission, host_restore,
    host_restore_display_mode, host_status, host_stop, restart_host, revoke_trust_with_background,
    start_local, trust_status,
};
pub(crate) use viewer::Connected;
use viewer::{ConnectRequest, Live, PendingConnect, connect_request, inspection, invitation_error};
pub use viewer::{PbDesktopViewV1, pb_mapper_desktop_view_v1, pb_mapper_desktop_viewer_call_json};

#[derive(Default)]
pub(crate) struct DesktopState {
    // Outbound connection setup must never hold the local host's stop/revoke
    // lock. Each endpoint owns its lifecycle independently.
    host: tokio::sync::Mutex<HostSlot>,
    /// Hot ops read it on the UI isolate, so it is never held across an
    /// await. Where both are taken, `connecting` comes first.
    viewer: parking_lot::RwLock<Option<Live>>,
    connecting: parking_lot::Mutex<Option<PendingConnect>>,
    viewer_epoch: AtomicU64,
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
            namespace: credential_namespace(),
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
    Connect(ConnectRequest),
    Disconnect,
}

/// The namespace the process credential owns, if there is one.
fn credential_namespace() -> Option<u64> {
    get_process_credential()
        .ok()
        .map(|credential| credential.key_id())
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

/// The `capabilities` op: what this computer can do, its displays and the
/// helper's state.
async fn capabilities() -> Result<Value, CtlError> {
    tokio::task::spawn_blocking(|| {
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
        json!({
            "capabilities": capabilities_json(desktop::host_support()),
            "helper": helper,
            "displays": displays.as_ref().ok(),
            "message": displays.err().map(|e| e.to_string()),
        })
    })
    .await
    .map_err(err)
}

async fn call(handle: &PbMapperHandle, request: Request) -> Result<Value, CtlError> {
    match request {
        Request::Capabilities => capabilities().await,
        Request::HelperAction { action } => helper_action(handle, action).await,
        Request::TrustStatus => trust_status(handle).await,
        Request::TrustRevoke => revoke_trust_with_background(handle, revoke_background(handle))
            .await
            .map(|()| json!({})),
        Request::HostStatus => host_status(handle).await,
        Request::BackgroundAction { action, move_relay } => {
            background_action(handle, action, move_relay).await
        }
        Request::HostEnable {
            service,
            display_id,
            allow_control,
        } => {
            let mut slot = handle.desktop.host.lock().await;
            enable_host(handle, &mut slot, service, display_id, allow_control, true).await
        }
        Request::HostRestore => host_restore(handle).await,
        Request::HostStart(start) if start.background => start_background(handle, start).await,
        Request::HostStart(start) => start_local(handle, Start::Fresh(start)).await,
        Request::HostStop { on_exit } => host_stop(handle, on_exit).await,
        Request::HostPermission { allow_control } => host_permission(handle, allow_control).await,
        Request::HostRestoreDisplayMode => host_restore_display_mode(handle).await,
        Request::HostEndSession => host_end_session(handle).await,
        Request::HostSetDisplay { display_id } => {
            restart_host(handle, HostChange::Display(display_id)).await
        }
        Request::HostRename { service } => restart_host(handle, HostChange::Service(service)).await,
        Request::InspectInvitation { invitation } => Ok(inspection(
            &Invitation::decode(&invitation).map_err(invitation_error)?,
        )),
        Request::Connect(request) => connect_request(handle, request).await,
        Request::Disconnect => handle.desktop.disconnect_viewer().await.map(|()| json!({})),
    }
}

#[cfg(test)]
mod tests;
