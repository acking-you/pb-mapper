//! The Windows service's background host as this app sees it: the cached
//! snapshot, its discovery, and setup, resume, stop and revoke.
#[cfg(windows)]
use super::credential_namespace;
use super::host::{HostSlot, HostStart};
#[cfg(windows)]
use super::host::{disable_legacy_startup, load_trust, revoke_trust, save_trust};
#[cfg(windows)]
use super::trust;
use super::{DesktopState, RelayScope, err};
use crate::{error::CtlError, handle::PbMapperHandle};
#[cfg(windows)]
use crate::{error::ErrorCode, state::process_credential};
#[cfg(windows)]
use pb_mapper::desktop;
use pb_mapper::desktop::HostStatus;
#[cfg(windows)]
use pb_mapper_core::checksum::Credential;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The service's background host as the app reads it: the service's
/// `Snapshot`, and beside it what this app observed itself. Every reply
/// carries it through `serde_json::Value`, so its keys come out sorted.
#[derive(Clone, Default, Serialize)]
pub(super) struct Background {
    pub(super) configured: bool,
    pub(super) enabled: bool,
    pub(super) host: Option<HostStatus>,
    pub(super) invitation: Option<String>,
    pub(super) settings: Option<BackgroundSettings>,
    pub(super) server: Option<String>,
    pub(super) namespace: Option<u64>,
    pub(super) message: String,
    /// Whether this platform has the service: said only by an answer made
    /// here, never by the service's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) supported: Option<bool>,
    /// Whether Windows forbids the service to start: absent until this app
    /// observed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) service_disabled: Option<bool>,
}

/// What the service hosts with.
#[cfg(windows)]
pub(super) type BackgroundSettings = desktop::privileged::background::Settings;
/// Off Windows there is no service, so there are never any settings.
#[cfg(not(windows))]
#[derive(Clone, Serialize)]
pub(super) enum BackgroundSettings {}

#[cfg(windows)]
impl From<desktop::privileged::background::Snapshot> for Background {
    fn from(snapshot: desktop::privileged::background::Snapshot) -> Self {
        let desktop::privileged::background::Snapshot {
            configured,
            enabled,
            host,
            invitation,
            settings,
            server,
            namespace,
            message,
        } = snapshot;
        Self {
            configured,
            enabled,
            host,
            invitation,
            settings,
            server,
            namespace,
            message,
            supported: None,
            service_disabled: None,
        }
    }
}

#[derive(Default)]
pub(super) struct BackgroundCache {
    pub(super) snapshot: Option<Background>,
    checked: Option<Instant>,
    refreshing: bool,
    revision: u64,
}
impl BackgroundCache {
    fn store(&mut self, mut snapshot: Background) -> Background {
        if snapshot.service_disabled == Some(true)
            && let Some(previous) = self
                .snapshot
                .as_ref()
                .filter(|previous| previous.configured)
        {
            // SCM Disabled proves there is no active/restarting service, but
            // does not destroy its saved grant. Preserve that distinction.
            snapshot = Background {
                enabled: false,
                host: None,
                service_disabled: Some(true),
                message: snapshot.message,
                ..previous.clone()
            };
        }
        self.revision += 1;
        self.checked = Some(Instant::now());
        self.snapshot = Some(snapshot.clone());
        snapshot
    }
}

/// How often a status poll asks the service again.
const BACKGROUND_REFRESH: Duration = Duration::from_secs(2);

/// A status poll's refresh while it runs. Ended without an answer, by a
/// panic or a dropped task, it clears `refreshing`, so later polls refresh
/// again rather than show this snapshot for good.
struct Refresh {
    cache: Arc<parking_lot::Mutex<BackgroundCache>>,
    /// The answer was taken, and `refreshing` cleared with it.
    done: bool,
}
impl Drop for Refresh {
    fn drop(&mut self) {
        if !self.done {
            self.cache.lock().refreshing = false;
        }
    }
}

impl DesktopState {
    pub(super) fn cache_background(&self, snapshot: Background) -> Background {
        self.background.lock().store(snapshot)
    }

    pub(super) fn background_status(&self, runtime: &tokio::runtime::Handle) -> Option<Background> {
        let mut cache = self.background.lock();
        let snapshot = cache.snapshot.clone();
        // Discovery is explicit. An unused helper adds no IPC to status polls.
        if snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.configured)
            && !cache.refreshing
            && cache
                .checked
                .is_none_or(|time| time.elapsed() >= BACKGROUND_REFRESH)
        {
            cache.refreshing = true;
            let revision = cache.revision;
            let shared = self.background.clone();
            runtime.spawn(async move {
                let mut refresh = Refresh {
                    cache: shared,
                    done: false,
                };
                let result = query_background().await;
                let mut cache = refresh.cache.lock();
                cache.refreshing = false;
                refresh.done = true;
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

/// `snapshot` with `matches_relay`: whether the background host it describes
/// is published where the settings in `current` name. Null while they have
/// not been read; with nothing configured, nothing is elsewhere.
pub(super) fn with_relay_match(snapshot: &Background, current: Option<&RelayScope>) -> Value {
    #[derive(Serialize)]
    struct Matched<'a> {
        #[serde(flatten)]
        snapshot: &'a Background,
        matches_relay: Option<bool>,
    }
    let matches_relay = current.map(|current| {
        !snapshot.configured
            || snapshot
                .server
                .as_deref()
                .is_some_and(|server| current.holds(server, snapshot.namespace))
    });
    json!(Matched {
        snapshot,
        matches_relay,
    })
}

pub(super) fn unavailable_background(message: impl Into<String>) -> Background {
    Background {
        supported: Some(cfg!(windows)),
        message: message.into(),
        ..Background::default()
    }
}

pub(super) fn failed_background(previous: Option<&Background>, error: CtlError) -> Background {
    Background {
        message: error.to_string(),
        host: None,
        // A previous Disabled observation cannot authorize a new local host after
        // service state becomes unknown. Fresh queries still fail closed.
        service_disabled: Some(false),
        ..previous
            .cloned()
            .unwrap_or_else(|| unavailable_background(""))
    }
}

pub(super) async fn query_background() -> Result<Background, CtlError> {
    #[cfg(windows)]
    {
        let helper = tokio::task::spawn_blocking(desktop::privileged::status)
            .await
            .map_err(err)?;
        query_background_for_helper(helper, async {
            let snapshot = desktop::privileged::background::status()
                .await
                .map_err(err)?;
            Ok(snapshot.into())
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
    query: impl std::future::Future<Output = Result<Background, CtlError>>,
) -> Result<Background, CtlError> {
    if !helper.installed {
        return Ok(unavailable_background(helper.message));
    }
    if helper.startup_disabled && !helper.running {
        return Ok(Background {
            service_disabled: Some(true),
            ..unavailable_background(
                "Background service is disabled; enable it to resume remembered sharing",
            )
        });
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
    query: impl std::future::Future<Output = Result<Background, CtlError>>,
) -> Background {
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
            Ok(with_relay_match(&snapshot, relay.as_ref()))
        }
        #[cfg(windows)]
        BackgroundVerb::Start => {
            let enabled = resume_background(handle, move_relay).await?;
            let relay = match &enabled {
                Enabled::Restart => None,
                Enabled::Hosting(_) => handle.desktop.relay_scope(&handle.state).await,
            };
            Ok(enabled.reply(|snapshot| with_relay_match(snapshot, relay.as_ref())))
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
async fn resume_background(handle: &PbMapperHandle, move_relay: bool) -> Result<Enabled, CtlError> {
    // As `enable_host`: the host slot, then the settings.
    let mut slot = handle.desktop.host.lock().await;
    if slot.host.is_some() {
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
        return enable(handle, &mut slot, service, display_id, allow_control, true).await;
    }
    disable_legacy_startup(handle).await?;
    let snapshot = desktop::privileged::background::configure(options)
        .await
        .map_err(err)?;
    Ok(Enabled::Hosting(
        handle.desktop.cache_background(snapshot.into()),
    ))
}

/// What resuming the service's `saved` host configures: the service, display
/// and permissions it saved, published through the relay, credential (of
/// `namespace`) and carrier the settings name now, with LAN-first as they
/// have it, and restored at startup again. Moving it to another relay or
/// namespace needs `move_relay`.
#[cfg(windows)]
pub(super) fn resume_options(
    saved: &Background,
    config: crate::state::AppConfig,
    namespace: u64,
    credential: String,
    move_relay: bool,
) -> Result<desktop::privileged::background::Options, CtlError> {
    use desktop::privileged::background::{Options, ResumeTarget, Snapshot};
    // Only what a resume reads, from the snapshot the card showed.
    let saved = Snapshot {
        configured: saved.configured,
        settings: saved.settings.clone(),
        server: saved.server.clone(),
        namespace: saved.namespace,
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
    if snapshot.configured && snapshot.service_disabled != Some(true) {
        let snapshot = desktop::privileged::background::stop().await.map_err(err)?;
        state.cache_background(snapshot.into());
        return Ok(());
    }
    state.cache_background(snapshot);
    Ok(())
}

pub(super) async fn revoke_background(handle: &PbMapperHandle) -> Result<Background, CtlError> {
    #[cfg(windows)]
    {
        let server = handle.state.lock().await.get_config_status().server_address;
        let snapshot =
            desktop::privileged::background::revoke_scope(server, credential_namespace())
                .await
                .map_err(err)?;
        Ok(snapshot.into())
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
            HelperVerb::Install if local.host.is_some() => {
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
        let mut slot = handle.desktop.host.lock().await;
        if slot.host.is_some() {
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
                display_device: trust::display_device(start.display_id),
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
            desktop::privileged::background::configure_with_identity(
                options,
                saved.identity().clone(),
            )
            .await
        } else {
            desktop::privileged::background::configure(options).await
        }
        .map_err(err)?;
        let snapshot = handle.desktop.cache_background(snapshot.into());
        slot.identity = None;
        revoke_trust(directory).await?;
        Ok(json!({"invitation": snapshot.invitation, "background": snapshot}))
    }
    #[cfg(not(windows))]
    {
        let _ = handle;
        Err(err("background hosting requires Windows"))
    }
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

/// What enabling the service's host came to.
#[cfg(windows)]
#[expect(
    clippy::large_enum_variant,
    reason = "returned once by a lifecycle op, never stored or queued"
)]
enum Enabled {
    /// Setup installs the service first, and this app restarts into it.
    Restart,
    /// The service hosts it.
    Hosting(Background),
}
#[cfg(windows)]
impl Enabled {
    /// As `host_enable` answers, with the snapshot as `background` shows it.
    fn reply(self, background: impl FnOnce(&Background) -> Value) -> Value {
        match self {
            Self::Restart => json!({"restart": true}),
            Self::Hosting(snapshot) => json!({
                "restart": false,
                "invitation": snapshot.invitation,
                "background": background(&snapshot),
            }),
        }
    }
}

/// `host_enable`, and `host_restore` of a grant the service is to own:
/// `enable`, answered.
#[cfg(windows)]
pub(super) async fn enable_host(
    handle: &PbMapperHandle,
    slot: &mut HostSlot,
    service: String,
    display_id: u32,
    allow_control: bool,
    allow_install: bool,
) -> Result<Value, CtlError> {
    let enabled = enable(
        handle,
        slot,
        service,
        display_id,
        allow_control,
        allow_install,
    )
    .await?;
    Ok(enabled.reply(|snapshot| json!(snapshot)))
}

/// The product has one durable host owner. Setup intent and any existing local
/// identity survive the installed application's restart; only the service owns
/// registration after the handoff. Internal SDK/local-host APIs remain separate.
/// `slot` is the locked host lifecycle slot.
#[cfg(windows)]
async fn enable(
    handle: &PbMapperHandle,
    slot: &mut HostSlot,
    service: String,
    display_id: u32,
    allow_control: bool,
    allow_install: bool,
) -> Result<Enabled, CtlError> {
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
    let local = slot.identity.clone().filter(|local| {
        local.scope.relay == scope.relay
            && local.scope.namespace == scope.namespace
            && slot
                .host
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
    let identity = pending.identity().clone();
    // Fails before anything changes, never after the running host has gone.
    let credential = credential_text(credential)?;
    save_trust(directory.clone(), pending).await?;
    let helper = tokio::task::spawn_blocking(desktop::privileged::status)
        .await
        .map_err(err)?;
    let setup = helper_setup(&helper, allow_install);
    // No two owners may publish the same desktop: the local host stops before
    // a service that could already hold an automatic grant is enabled. It
    // is adopted, and its viewer reconnects, only when the service resumes
    // its invitation at once: the same identity and service, and a helper
    // that needs no setup, which the owner may cancel.
    let adopted = match slot.host.take() {
        Some(host) if local.is_some() && setup.is_none() => Some(host),
        host => {
            if let Some(host) = host {
                host.stop().await.map_err(err)?;
            }
            None
        }
    };
    slot.identity = None;
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
            match previous {
                Some(mut saved) => {
                    if saved.settings.background {
                        saved.settings.auto_start = false;
                    }
                    save_trust(directory, saved).await?;
                }
                None => revoke_trust(directory).await?,
            }
            return Err(failure);
        }
        if matches!(action, desktop::privileged::Action::Install { .. }) {
            desktop::privileged::prepare_application_restart()
                .await
                .map_err(err)?;
            return Ok(Enabled::Restart);
        }
    }
    let options = desktop::privileged::background::Options {
        server: config.server_address,
        credential,
        relay_transport: config.relay_transport,
        settings: desktop::privileged::background::Settings {
            service,
            display_id,
            display_device: trust::display_device(display_id),
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
    let snapshot = handle.desktop.cache_background(snapshot.into());
    // The service now durably owns the key. Retiring the legacy copy also
    // prevents an explicit legacy restore after service revocation.
    revoke_trust(directory).await?;
    Ok(Enabled::Hosting(snapshot))
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
    _: &mut HostSlot,
    _: String,
    _: u32,
    _: bool,
    _: bool,
) -> Result<Value, CtlError> {
    Err(err("persistent desktop hosting currently requires Windows"))
}
