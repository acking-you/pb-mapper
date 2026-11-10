//! The local host this app owns: start, restore, restart and its remembered
//! trust.
use super::background::{
    Background, enable_host, query_background, stop_background, with_relay_match,
};
use super::{credential_namespace, err, trust};
use crate::{error::CtlError, handle::PbMapperHandle, state::process_credential};
use pb_mapper::{
    Client,
    desktop::{self, Access, DisplaySelection, Host, HostOptions, StopReason},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// What the host lifecycle lock guards: the host this app runs, and its
/// identity beside it.
#[derive(Default)]
pub(super) struct HostSlot {
    pub(super) host: Option<Host>,
    /// Retained, even an ephemeral local key, while its host lives, so
    /// ownership can move to the service unchanged.
    pub(super) identity: Option<LocalIdentity>,
}

/// The running local host's identity and settings: what a restart on another
/// display or under another service, and the background handoff, start from.
#[derive(Clone)]
pub(super) struct LocalIdentity {
    pub(super) scope: trust::Scope,
    pub(super) identity: desktop::HostIdentity,
    pub(super) settings: trust::Settings,
    pub(super) remembered: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostStart {
    pub(super) service: String,
    pub(super) display_id: u32,
    pub(super) allow_control: bool,
    #[serde(default)]
    pub(super) relay_only: bool,
    #[serde(default)]
    pub(super) privileged: bool,
    #[serde(default)]
    pub(super) remember: bool,
    #[serde(default)]
    pub(super) auto_start: bool,
    #[serde(default)]
    pub(super) background: bool,
}

/// What a local host starts from.
pub(super) enum Start {
    /// `host_start`: the settings the owner just chose.
    Fresh(HostStart),
    /// `host_restore`: the remembered ones.
    Restore,
}

pub(super) async fn disable_legacy_startup(handle: &PbMapperHandle) -> Result<(), CtlError> {
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

pub(super) async fn revoke_trust_with_background(
    handle: &PbMapperHandle,
    background: impl std::future::Future<Output = Result<Background, CtlError>>,
) -> Result<(), CtlError> {
    // The handoff and its revocation share one lifecycle lock. In particular,
    // observe/revoke the service only after an in-flight Configure has finished;
    // the UI's cached owner is not authoritative here.
    let state = &handle.desktop;
    let mut slot = state.host.lock().await;
    if let Some(host) = slot.host.as_ref() {
        host.set_control_allowed(false);
    }
    let directory = handle.state.lock().await.desktop_config_dir();
    let retired = revoke_trust(directory).await;
    // Retire pending automatic setup before deleting the service grant, so a
    // later startup cannot recreate access from a leftover local identity.
    let revoked = match retired {
        Ok(()) => background
            .await
            .map(|snapshot| state.cache_background(snapshot)),
        Err(failure) => Err(failure),
    };
    // Its viewer hears that access was withdrawn, and does not reconnect.
    let stopped = if let Some(host) = slot.host.take() {
        host.stop_with(StopReason::Revoked).await.map_err(err)
    } else {
        Ok(())
    };
    slot.identity = None;
    stopped?;
    revoked?;
    Ok(())
}

/// The remembered grant, read off the runtime.
pub(super) async fn load_trust(directory: PathBuf) -> Result<Option<trust::Saved>, CtlError> {
    tokio::task::spawn_blocking(move || trust::load(&directory))
        .await
        .map_err(err)?
}

/// `saved` remembered, written off the runtime.
#[cfg(windows)]
pub(super) async fn save_trust(directory: PathBuf, saved: trust::Saved) -> Result<(), CtlError> {
    tokio::task::spawn_blocking(move || trust::save(&directory, &saved))
        .await
        .map_err(err)?
}

/// The remembered grant forgotten, off the runtime.
pub(super) async fn revoke_trust(directory: PathBuf) -> Result<(), CtlError> {
    tokio::task::spawn_blocking(move || trust::revoke(&directory))
        .await
        .map_err(err)?
}

/// `host` once `persisted`, its remembered grant if it has one, is saved.
/// A grant the file system refuses stops the host, which is not to run
/// unremembered; a save that panicked returns at once.
async fn persist_or_stop(
    host: Host,
    persisted: Option<trust::Saved>,
    directory: &Path,
) -> Result<Host, CtlError> {
    if let Some(saved) = persisted {
        let directory = directory.to_owned();
        let stored = tokio::task::spawn_blocking(move || trust::save(&directory, &saved))
            .await
            .map_err(err)?;
        if let Err(failure) = stored {
            let _ = host.stop().await;
            return Err(failure);
        }
    }
    Ok(host)
}

/// `trust_status`: the remembered grant, and whether it is for the relay and
/// namespace the settings name.
pub(super) async fn trust_status(handle: &PbMapperHandle) -> Result<Value, CtlError> {
    let (directory, relay) = {
        let state = handle.state.lock().await;
        (
            state.desktop_config_dir(),
            state.get_config_status().server_address,
        )
    };
    let saved = load_trust(directory).await?;
    let namespace = credential_namespace();
    let matches = saved.as_ref().is_some_and(|saved| {
        namespace.is_some_and(|namespace| saved.matches_relay(&relay, namespace))
    });
    // The invitation resumes sharing without setup, so only a grant
    // for this relay and namespace offers one.
    let saved = match saved {
        Some(saved) => {
            let invitation = if matches {
                let service = saved.settings.service.clone();
                Some(saved.identity().invitation(service).map_err(err)?.encode())
            } else {
                None
            };
            Some(saved.metadata(invitation))
        }
        None => None,
    };
    Ok(json!({"supported": trust::supported(), "saved": saved, "matches_relay": matches}))
}

/// `host_status`: this app's host and the background service's.
pub(super) async fn host_status(handle: &PbMapperHandle) -> Result<Value, CtlError> {
    let state = &handle.desktop;
    // Each host says whether it is published where the settings
    // name (`matches_relay`), and where it is (`server`).
    let relay = state.relay_scope(&handle.state).await;
    let background = state
        .background_status(handle.runtime.handle())
        .map(|snapshot| with_relay_match(&snapshot, relay.as_ref()));
    // A lifecycle call (a display switch, a rename, a helper action
    // waiting on UAC) holds the host lock, but must never delay a
    // status poll. `busy` says the host is then unknown, not absent.
    // The invitation stays beside the status, so nothing that
    // serialises a status carries the secret.
    let (host, busy) = match state.host.try_lock() {
        Ok(slot) => (
            slot.host.as_ref().map(|host| {
                let invitation = host.invitation();
                let mut value = json!({
                    "status": host.status(),
                    "service": invitation.service,
                    "invitation": invitation.encode(),
                });
                // Set beside the host, under the lock held here.
                if let Some(scope) = slot.identity.as_ref().map(|local| &local.scope) {
                    value["server"] = json!(scope.relay);
                    value["matches_relay"] = json!(
                        relay
                            .as_ref()
                            .map(|current| { current.holds(&scope.relay, Some(scope.namespace)) })
                    );
                }
                value
            }),
            false,
        ),
        Err(_) => (None, true),
    };
    Ok(json!({"host": host, "busy": busy, "background": background}))
}

/// `host_restore`: the remembered grant again, through the service when it
/// is the service's.
pub(super) async fn host_restore(handle: &PbMapperHandle) -> Result<Value, CtlError> {
    let directory = handle.state.lock().await.desktop_config_dir();
    if let Some(saved) = load_trust(directory)
        .await?
        .filter(|saved| saved.settings.background)
    {
        let mut slot = handle.desktop.host.lock().await;
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
    start_local(handle, Start::Restore).await
}

/// `host_stop`: this app's host, and unless the app is exiting, the
/// background service's too.
pub(super) async fn host_stop(handle: &PbMapperHandle, on_exit: bool) -> Result<Value, CtlError> {
    let state = &handle.desktop;
    let mut slot = state.host.lock().await;
    let stopped = if let Some(host) = slot.host.take() {
        host.stop().await.map_err(err)
    } else {
        Ok(())
    };
    slot.identity = None;
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
    Ok(json!({}))
}

/// `host_permission`: whether a viewer may take control, of this app's host
/// or else the background service's.
pub(super) async fn host_permission(
    handle: &PbMapperHandle,
    allow_control: bool,
) -> Result<Value, CtlError> {
    let state = &handle.desktop;
    let slot = state.host.lock().await;
    let Some(host) = slot.host.as_ref() else {
        #[cfg(windows)]
        {
            let snapshot = query_background().await?;
            if snapshot.enabled {
                let snapshot = desktop::privileged::background::set_control_allowed(allow_control)
                    .await
                    .map_err(err)?;
                state.cache_background(snapshot.into());
                return Ok(json!({}));
            }
        }
        return Err(err("desktop is not shared"));
    };
    if !allow_control {
        host.set_control_allowed(false);
    }
    if let Some(scope) = slot
        .identity
        .as_ref()
        .filter(|local| local.remembered)
        .map(|local| local.scope.clone())
    {
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
    Ok(json!({}))
}

/// `host_restore_display_mode`: put back the display mode a viewer changed,
/// of this app's host or else the background service's.
pub(super) async fn host_restore_display_mode(handle: &PbMapperHandle) -> Result<Value, CtlError> {
    let state = &handle.desktop;
    let slot = state.host.lock().await;
    if let Some(host) = slot.host.as_ref() {
        return Ok(json!({"restored": host.restore_display_mode()}));
    }
    #[cfg(windows)]
    if state
        .background
        .lock()
        .snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.enabled)
    {
        let snapshot = desktop::privileged::background::restore_display_mode()
            .await
            .map_err(err)?;
        state.cache_background(snapshot.into());
        return Ok(json!({"restored": true}));
    }
    Err(err("desktop is not shared"))
}

/// `host_end_session`: end the viewer's session and keep sharing, on this
/// app's host or else the background service's.
pub(super) async fn host_end_session(handle: &PbMapperHandle) -> Result<Value, CtlError> {
    let state = &handle.desktop;
    let slot = state.host.lock().await;
    if let Some(host) = slot.host.as_ref() {
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
        .is_some_and(|snapshot| snapshot.enabled)
    {
        let snapshot = desktop::privileged::background::end_session()
            .await
            .map_err(err)?;
        state.cache_background(snapshot.into());
        return Ok(json!({}));
    }
    Err(err("desktop is not shared"))
}

/// A host this UI owns, unless the service already owns this desktop.
pub(super) async fn start_local(handle: &PbMapperHandle, start: Start) -> Result<Value, CtlError> {
    // Serialize both owners before loading configuration or talking to the
    // service. A concurrent Stop cannot finish and then be undone by this start.
    let mut slot = handle.desktop.host.lock().await;
    if slot.host.is_some() {
        return Err(err("desktop is already shared"));
    }
    let background = handle.desktop.cache_background(query_background().await?);
    if background.configured {
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
            state.get_config_status(),
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
        saved.identity().clone()
    } else {
        desktop::HostIdentity::generate().map_err(err)?
    };
    let persisted = remember.then(|| trust::Saved::new(&scope, settings.clone(), &identity));
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
    let host = persist_or_stop(host, persisted, &local_identity.scope.directory).await?;
    let invitation = host.invitation().encode();
    slot.host = Some(host);
    slot.identity = Some(local_identity);
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

/// What `restart_host` changes of the running local host.
pub(super) enum HostChange {
    /// `host_set_display`: the host resumes at once with the same
    /// invitation, so the viewer reconnects.
    Display(u32),
    /// `host_rename`: the renamed service is no longer what the viewer
    /// reaches.
    Service(String),
}
impl HostChange {
    /// How the viewer's session ends.
    fn reason(&self) -> StopReason {
        match self {
            Self::Display(_) => StopReason::Restarting,
            Self::Service(_) => StopReason::HostStopped,
        }
    }

    fn apply(self, settings: &mut trust::Settings) {
        match self {
            Self::Display(display_id) => settings.display_id = display_id,
            Self::Service(service) => settings.service = service,
        }
    }
}

/// `host_set_display` and `host_rename`: the running local host again, with
/// its identity, on another display or under another service.
pub(super) async fn restart_host(
    handle: &PbMapperHandle,
    change: HostChange,
) -> Result<Value, CtlError> {
    let state = &handle.desktop;
    let mut slot = state.host.lock().await;
    // The service has no display switch or rename: a background host is
    // stopped and enabled again instead.
    if state.cache_background(query_background().await?).configured {
        return Err(CtlError::invalid_argument("stop the background host first"));
    }
    let (Some(host), Some(local)) = (slot.host.as_ref(), slot.identity.clone()) else {
        return Err(err("desktop is not shared"));
    };
    let mut settings = local.settings.clone();
    // The live policy, which "make view-only" may have changed since.
    settings.allow_control = host.status().control_allowed;
    let reason = change.reason();
    change.apply(&mut settings);
    // Check the new service before the running host goes.
    local
        .identity
        .invitation(settings.service.clone())
        .map_err(|failure| CtlError::invalid_argument(failure.to_string()))?;
    let (config, client) = {
        let state = handle.state.lock().await;
        (state.get_config_status(), state.relay_client()?)
    };
    let credential = process_credential()?;
    if config.server_address != local.scope.relay || credential.key_id() != local.scope.namespace {
        return Err(err(
            "the relay settings changed since sharing started; stop sharing and share again",
        ));
    }
    let persisted = local
        .remembered
        .then(|| trust::Saved::new(&local.scope, settings.clone(), &local.identity));
    let Some(host) = slot.host.take() else {
        return Err(err("desktop is not shared"));
    };
    slot.identity = None;
    // The same identity, source and live policy, on the relay client the
    // settings name now (a rotated key, another carrier); the old
    // registration goes before the new one.
    let options = host_options(&settings, config.lan_first);
    let host = host
        .restart_on(&client, reason, |restarted| *restarted = options)
        .await
        .map_err(err)?;
    let host = persist_or_stop(host, persisted, &local.scope.directory).await?;
    let invitation = host.invitation().encode();
    slot.host = Some(host);
    slot.identity = Some(LocalIdentity { settings, ..local });
    Ok(json!({"invitation": invitation}))
}
