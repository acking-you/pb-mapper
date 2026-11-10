//! The Windows service's background host as this app sees it: the cached
//! snapshot, its discovery, and setup, resume, stop and revoke.
use super::host::HostStart;
#[cfg(windows)]
use super::host::{disable_legacy_startup, load_trust};
#[cfg(windows)]
use super::trust;
use super::{DesktopState, RelayScope, err};
use crate::{error::CtlError, handle::PbMapperHandle};
#[cfg(windows)]
use crate::{error::ErrorCode, state::process_credential};
#[cfg(windows)]
use pb_mapper::desktop;
use pb_mapper::desktop::Host;
#[cfg(windows)]
use pb_mapper_core::checksum::{Credential, get_process_credential};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct BackgroundCache {
    pub(super) snapshot: Option<Value>,
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

impl DesktopState {
    pub(super) fn cache_background(&self, snapshot: Value) -> Value {
        self.background.lock().store(snapshot)
    }

    pub(super) fn background_status(&self, runtime: &tokio::runtime::Handle) -> Option<Value> {
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
}

/// What the helper card asks of Windows. Setup's own install and enable go
/// through `host_enable`.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(super) enum HelperVerb {
    Install,
    Disable,
    Uninstall,
}

/// The service's state, or its saved host resumed where the settings name
/// now (`resume_background`), through setup when the helper needs it.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(super) enum BackgroundVerb {
    Status,
    Start,
}

pub(super) fn background_configured(snapshot: &Value) -> bool {
    snapshot["configured"].as_bool().unwrap_or(false)
}

#[cfg(windows)]
pub(super) fn background_enabled(snapshot: &Value) -> bool {
    snapshot["enabled"].as_bool().unwrap_or(false)
}

/// `snapshot` with `matches_relay`: whether the background host it describes
/// is published where the settings in `current` name. Null while they have
/// not been read; with nothing configured, nothing is elsewhere.
pub(super) fn with_relay_match(mut snapshot: Value, current: Option<&RelayScope>) -> Value {
    let matches = current.map(|current| {
        !background_configured(&snapshot)
            || snapshot["server"]
                .as_str()
                .is_some_and(|server| current.holds(server, snapshot["namespace"].as_u64()))
    });
    snapshot["matches_relay"] = json!(matches);
    snapshot
}

pub(super) fn unavailable_background(message: impl Into<String>) -> Value {
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

pub(super) async fn query_background() -> Result<Value, CtlError> {
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
pub(super) async fn query_background_for_helper(
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
    // Whose it is comes first: running or not, another user's helper, or one
    // whose installation cannot be read, stays out of this app's reach.
    if !helper.authorized_application && !helper.owned_by_user {
        return Err(helper_not_ready(desktop::privileged::NotReady::OtherOwner));
    }
    if !helper.running {
        return Err(CtlError::new(
            ErrorCode::HelperStopped,
            "background service is unavailable and may restart; disable it before starting local sharing",
        ));
    }
    if !helper.authorized_application {
        return Err(CtlError::new(
            ErrorCode::HelperForeignApp,
            "open the installed application to check background sharing, or disable its service before starting local sharing",
        ));
    }
    if let Some(reason) = helper.not_ready() {
        return Err(helper_not_ready(reason));
    }
    query.await
}

/// A helper that cannot be used, with the code the window explains it by.
#[cfg(windows)]
fn helper_not_ready(reason: desktop::privileged::NotReady) -> CtlError {
    use desktop::privileged::NotReady;
    let code = match &reason {
        NotReady::Outdated => ErrorCode::HelperOutdated,
        NotReady::NotInstalled => ErrorCode::HelperMissing,
        NotReady::Stopped => ErrorCode::HelperStopped,
        NotReady::ForeignApplication => ErrorCode::HelperForeignApp,
        NotReady::OtherOwner => ErrorCode::HelperOtherOwner,
        NotReady::Unverified(_) => ErrorCode::HelperUnverified,
    };
    CtlError::new(code, reason.to_string())
}

pub(super) async fn discover_background(
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

/// `status`: the service's state, asked of it. `start`: `resume_background`,
/// answered as `host_enable` is, its `background` with `matches_relay`.
pub(super) async fn background_action(
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
    let config = handle.state.lock().await.get_config_status();
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
pub(super) fn resume_options(
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

pub(super) async fn stop_background(state: &DesktopState) -> Result<(), CtlError> {
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

pub(super) async fn revoke_background(handle: &PbMapperHandle) -> Result<Value, CtlError> {
    #[cfg(windows)]
    {
        let server = handle.state.lock().await.get_config_status().server_address;
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

/// The helper card's verbs. Installing replaces this UI after a ready ack.
pub(super) async fn helper_action(
    handle: &PbMapperHandle,
    verb: HelperVerb,
) -> Result<Value, CtlError> {
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

/// `host_start` with `background`: the service takes the grant, never this UI.
pub(super) async fn start_background(
    handle: &PbMapperHandle,
    start: HostStart,
) -> Result<Value, CtlError> {
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
            (state.desktop_config_dir(), state.get_config_status())
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

/// The product has one durable host owner. Setup intent and any existing local
/// identity survive the installed application's restart; only the service owns
/// registration after the handoff. Internal SDK/local-host APIs remain separate.
/// `slot` is the locked host lifecycle slot.
#[cfg(windows)]
pub(super) async fn enable_host(
    handle: &PbMapperHandle,
    slot: &mut Option<Host>,
    service: String,
    display_id: u32,
    allow_control: bool,
    allow_install: bool,
) -> Result<Value, CtlError> {
    let (directory, config) = {
        let state = handle.state.lock().await;
        (state.desktop_config_dir(), state.get_config_status())
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
pub(super) fn helper_setup(
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
pub(super) fn helper_setup_action(
    helper: &desktop::privileged::Status,
) -> desktop::privileged::Action {
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
pub(super) async fn enable_host(
    _: &PbMapperHandle,
    _: &mut Option<Host>,
    _: String,
    _: u32,
    _: bool,
    _: bool,
) -> Result<Value, CtlError> {
    Err(err("persistent desktop hosting currently requires Windows"))
}
