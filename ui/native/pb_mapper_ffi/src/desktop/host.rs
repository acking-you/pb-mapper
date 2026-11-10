//! The local host this app owns: start, restore, restart and its remembered
//! trust.
use super::background::{background_configured, query_background};
use super::{err, trust};
use crate::{error::CtlError, handle::PbMapperHandle, state::process_credential};
use pb_mapper::{
    Client,
    desktop::{self, Access, DisplaySelection, Host, HostOptions, StopReason},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

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

/// The remembered grant, read off the runtime.
pub(super) async fn load_trust(directory: PathBuf) -> Result<Option<trust::Saved>, CtlError> {
    tokio::task::spawn_blocking(move || trust::load(&directory))
        .await
        .map_err(err)?
}

/// A host this UI owns, unless the service already owns this desktop.
pub(super) async fn start_local(handle: &PbMapperHandle, start: Start) -> Result<Value, CtlError> {
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
pub(super) async fn restart_host(
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
