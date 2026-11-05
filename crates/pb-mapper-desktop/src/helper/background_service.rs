use super::*;
use crate::{Host, HostIdentity, HostOptions};
use pb_mapper_client::sdk::{Client, ClientConfig};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const STORAGE_LIMIT: u64 = 32 * 1024;
const FILE_NAME: &str = "background-host.dpapi";
const REVOKED_MARKER: &str = "background-host.revoked";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    schema: u32,
    owner_sid: String,
    options: Options,
    identity: HostIdentity,
}
impl Saved {
    fn prepare(options: Options, owner_sid: &str, previous: Option<&Self>) -> Result<Self> {
        let key_id = validate(&options)?;
        let same_identity = previous.is_some_and(|old| {
            old.owner_sid == owner_sid
                && old.options.server == options.server
                && old.options.settings.service == options.settings.service
                && namespace(&old.options).is_ok_and(|old| old == key_id)
        });
        let identity = if same_identity {
            previous
                .ok_or_else(|| error("Missing previous background identity"))?
                .identity
                .clone()
        } else {
            HostIdentity::generate()?
        };
        Ok(Self {
            schema: 1,
            owner_sid: owner_sid.into(),
            options,
            identity,
        })
    }
}
fn validate(options: &Options) -> Result<u64> {
    if options.server.is_empty()
        || options.server.len() > 1024
        || options.credential.len() > 512
        || !crate::valid_service(&options.settings.service)
    {
        return Err(error("Invalid background host configuration"));
    }
    let namespace = namespace(options)?;
    client(options)?;
    Ok(namespace)
}
fn namespace(options: &Options) -> Result<u64> {
    pb_mapper_core::checksum::parse_credential(options.credential.trim())
        .map(|credential| credential.key_id())
        .map_err(|_| error("Invalid relay credential"))
}
fn client(options: &Options) -> Result<Client> {
    Client::new(ClientConfig {
        server: options.server.clone(),
        credential: options.credential.clone(),
        keep_alive: true,
        namespace: None,
    })
    .and_then(|client| client.with_relay_transport(options.relay_transport))
    .map_err(|_| error("Invalid relay configuration"))
}

type Operation = (Command, oneshot::Sender<Result<Snapshot>>);

/// A single actor owns the host and its pending start. Management requests and
/// SCM stop can cancel network startup without waiting for relay readiness.
pub(crate) async fn run(stop: CancellationToken) -> Result<()> {
    let config = Arc::new(configuration()?);
    let root = os::install_root()?;
    let loaded = load(&root, &config.owner_sid);
    let (saved, message) = match loaded {
        Ok(saved) => (saved, String::new()),
        Err(_) => (
            None,
            "Stored background authorization could not be loaded; authorize it again".into(),
        ),
    };
    let mut state = Supervisor {
        enabled: saved
            .as_ref()
            .is_some_and(|saved| saved.options.settings.auto_start),
        saved,
        host: None,
        pending: None,
        session: None,
        message,
        retry_at: Instant::now(),
        retry_delay: Duration::from_secs(1),
        capture_failure_since: None,
        persistence_failed: false,
    };
    let (sender, mut commands) = mpsc::channel::<Operation>(8);
    let mut management = tokio::spawn(manage(config.clone(), sender, stop.clone()));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let outcome = loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break Ok(()),
            result = &mut management => break result.map_err(error).and_then(|result| result),
            Some((command, reply)) = commands.recv() => {
                let result = state.command(command, &root, &config.owner_sid).await;
                let _ = reply.send(result);
            },
            result = async { match state.pending.as_mut() { Some(pending) => pending.await, None => std::future::pending().await } }, if state.pending.is_some() => {
                state.pending.take();
                match result {
                    Ok(Ok(host)) => { state.host = Some(host); state.message.clear(); },
                    _ => {
                        state.message = "Unable to connect to the configured relay; background hosting will retry".into();
                        state.retry_at = Instant::now() + state.retry_delay;
                        state.retry_delay = (state.retry_delay * 2).min(Duration::from_secs(60));
                    },
                }
            },
            _ = tick.tick() => state.reconcile(&config.owner_sid).await,
        }
    };
    // Cancellation is process-local and never depends on the relay being up.
    state.deactivate().await;
    if !management.is_finished() {
        management.abort();
        let _ = management.await;
    }
    outcome
}

struct Supervisor {
    saved: Option<Saved>,
    enabled: bool,
    host: Option<Host>,
    pending: Option<JoinHandle<Result<Host>>>,
    session: Option<u32>,
    message: String,
    retry_at: Instant,
    retry_delay: Duration,
    capture_failure_since: Option<Instant>,
    persistence_failed: bool,
}
impl Supervisor {
    fn snapshot(&self) -> Snapshot {
        let saved = self.saved.as_ref();
        Snapshot {
            configured: saved.is_some(),
            enabled: self.enabled,
            host: self.host.as_ref().map(Host::status),
            invitation: saved.and_then(|saved| {
                saved
                    .identity
                    .invitation(saved.options.settings.service.clone())
                    .encode()
                    .ok()
            }),
            settings: saved.map(|saved| saved.options.settings.clone()),
            server: saved.map(|saved| saved.options.server.clone()),
            namespace: saved.and_then(|saved| namespace(&saved.options).ok()),
            message: self.message.clone(),
        }
    }
    async fn deactivate(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.abort();
            // A completed task may already own a Host. Stopping that result
            // avoids detaching it when stop races relay readiness.
            if let Ok(Ok(host)) = pending.await {
                let _ = host.stop().await;
            }
        }
        if let Some(host) = self.host.take() {
            let _ = host.stop().await;
        }
        self.session = None;
        self.capture_failure_since = None;
    }
    async fn command(
        &mut self,
        command: Command,
        root: &Path,
        owner_sid: &str,
    ) -> Result<Snapshot> {
        match command {
            Command::Status => {}
            Command::Configure { options } => {
                let saved = Saved::prepare(options, owner_sid, self.saved.as_ref())?;
                // Persist before replacing either live state or the identity.
                save(root, &saved)?;
                clear_marker(root)?;
                self.deactivate().await;
                self.saved = Some(saved);
                self.enabled = true;
                self.retry_at = Instant::now();
                self.retry_delay = Duration::from_secs(1);
                self.message = "Connecting background host".into();
                self.persistence_failed = false;
            }
            Command::Start => {
                if self.persistence_failed {
                    return Err(error(
                        "Background authorization could not be persisted; configure it again before starting",
                    ));
                }
                if self.saved.is_none() {
                    return Err(error("No saved background authorization"));
                }
                self.enabled = true;
                self.retry_at = Instant::now();
            }
            Command::Stop => {
                self.enabled = false;
                self.deactivate().await;
                if let Some(saved) = self.saved.as_mut() {
                    saved.options.settings.auto_start = false;
                }
                self.persist_restriction(root).await?;
                self.message.clear();
            }
            Command::Revoke => {
                self.enabled = false;
                self.deactivate().await;
                self.saved = None;
                self.persist_restriction(root).await?;
                self.message.clear();
            }
            Command::Permission { allow_control } => {
                if allow_control && self.persistence_failed {
                    return Err(error(
                        "Configure background authorization again before granting input",
                    ));
                }
                let mut saved = self
                    .saved
                    .clone()
                    .ok_or_else(|| error("No saved background authorization"))?;
                if !allow_control && let Some(host) = &self.host {
                    host.set_control_allowed(false);
                }
                saved.options.settings.allow_control = allow_control;
                if allow_control {
                    save(root, &saved)?;
                    self.saved = Some(saved);
                    if self.pending.is_some() {
                        self.deactivate().await;
                    }
                    if let Some(host) = &self.host {
                        host.set_control_allowed(true);
                    }
                } else {
                    self.saved = Some(saved);
                    if self.pending.is_some() {
                        self.deactivate().await;
                    }
                    self.persist_restriction(root).await?;
                }
            }
        }
        Ok(self.snapshot())
    }
    async fn persist_restriction(&mut self, root: &Path) -> Result<()> {
        // A separate record makes a failed grant replacement/deletion fail
        // closed across service restarts. It is removed only after the new
        // restrictive state is durable. If even the marker cannot be written,
        // retain a sticky in-process denial and report that durability failed.
        self.persistence_failed = true;
        let result = mark_revoked(root)
            .and_then(|()| match &self.saved {
                Some(saved) => save(root, saved),
                None => remove(root),
            })
            .and_then(|()| clear_marker(root));
        if result.is_err() {
            self.enabled = false;
            self.deactivate().await;
            self.message = "Background hosting stopped, but the authorization change could not be saved; configure it again before starting".into();
            return Err(error(&self.message));
        }
        self.persistence_failed = false;
        Ok(())
    }
    async fn reconcile(&mut self, owner_sid: &str) {
        if !self.enabled || self.saved.is_none() {
            return;
        }
        let session = os::background_session(owner_sid).ok();
        if session != self.session && (self.host.is_some() || self.pending.is_some()) {
            self.deactivate().await;
            self.retry_at = Instant::now();
        }
        let Some(session) = session else {
            self.message = "waiting_for_owner_session".into();
            return;
        };
        if let Some(host) = &self.host {
            match host.status().state.as_str() {
                "capture_unavailable" => {
                    let since = self.capture_failure_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_secs(5) {
                        self.deactivate().await;
                        self.message =
                            "Desktop capture stopped; background hosting will reconnect".into();
                        self.retry_at = Instant::now() + self.retry_delay;
                        self.retry_delay = (self.retry_delay * 2).min(Duration::from_secs(60));
                    }
                }
                "streaming" => {
                    self.capture_failure_since = None;
                    self.retry_delay = Duration::from_secs(1);
                }
                _ => self.capture_failure_since = None,
            }
        }
        if self.host.is_some() || self.pending.is_some() || Instant::now() < self.retry_at {
            return;
        }
        let Some(saved) = self.saved.clone() else {
            return;
        };
        self.session = Some(session);
        self.message = "Connecting background host".into();
        self.pending = Some(tokio::spawn(async move {
            let client = client(&saved.options)?;
            let settings = saved.options.settings;
            let display_id = settings.display_id;
            Host::start_with_backend_and_identity(
                &client,
                HostOptions {
                    service: settings.service,
                    display_id,
                    allow_control: settings.allow_control,
                    allow_direct: settings.allow_direct,
                },
                Arc::new(move || super::super::client::service_backend(display_id)),
                None,
                saved.identity,
            )
            .await
        }));
    }
}

async fn manage(
    config: Arc<super::super::Configuration>,
    sender: mpsc::Sender<Operation>,
    stop: CancellationToken,
) -> Result<()> {
    let mut listener = ipc::bind(PIPE, Some(&config.owner_sid), true)?;
    let mut requests = JoinSet::new();
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            Some(_) = requests.join_next(), if !requests.is_empty() => {},
            connected = listener.connect(), if requests.len() < 8 => {
                connected?;
                let next = ipc::bind(PIPE, Some(&config.owner_sid), false)?;
                let mut caller = std::mem::replace(&mut listener, next);
                let config = config.clone();
                let sender = sender.clone();
                requests.spawn(async move {
                    let _owner = os::verify_management_client(&caller, &config)?;
                    let command = tokio::time::timeout(super::super::TIMEOUT, crate::wire::read_json(&mut caller)).await.map_err(error)??;
                    let (reply, result) = oneshot::channel();
                    sender.try_send((command, reply)).map_err(|_| error("Background management is busy"))?;
                    let result = result.await.map_err(error)?;
                    let response = match result {
                        Ok(snapshot) => Response { snapshot: Some(snapshot), error: None },
                        Err(e) => Response { snapshot: None, error: Some(e.to_string()) },
                    };
                    tokio::time::timeout(super::super::TIMEOUT, crate::wire::write_json(&mut caller, &response)).await.map_err(error)??;
                    Ok::<_, std::io::Error>(())
                });
            },
        }
    }
    requests.abort_all();
    while requests.join_next().await.is_some() {}
    Ok(())
}

fn load(root: &Path, owner_sid: &str) -> Result<Option<Saved>> {
    if root.join(REVOKED_MARKER).exists() {
        return Err(error("Background authorization has a pending revocation"));
    }
    let path = root.join(FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }
    private_file(&path)?;
    if std::fs::metadata(&path)?.len() > STORAGE_LIMIT {
        return Err(error("Background authorization exceeds its size limit"));
    }
    let plain = protect(&std::fs::read(&path)?, false)?;
    let saved: Saved =
        serde_json::from_slice(&plain).map_err(|_| error("Invalid background authorization"))?;
    if saved.schema != 1 || saved.owner_sid != owner_sid {
        return Err(error(
            "Background authorization belongs to another installation owner",
        ));
    }
    validate(&saved.options)?;
    saved.identity.acceptor()?;
    Ok(Some(saved))
}
fn save(root: &Path, saved: &Saved) -> Result<()> {
    use std::{io::Write, os::windows::io::FromRawHandle};
    use windows::{
        Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::*},
        core::PCWSTR,
    };
    os::protected_path(root)?;
    let target = root.join(FILE_NAME);
    if target.exists() {
        private_file(&target)?;
    }
    let cipher = protect(&serde_json::to_vec(saved).map_err(error)?, true)?;
    let temporary = root.join(format!("background-{}.tmp", crate::security::secret()?));
    let descriptor = os::Descriptor::new("O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)")?;
    let result = (|| {
        let handle = unsafe {
            CreateFileW(
                PCWSTR(os::wide(&temporary).as_ptr()),
                GENERIC_WRITE.0,
                FILE_SHARE_READ,
                Some(descriptor.attributes()),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(error)?;
        let mut file = unsafe { std::fs::File::from_raw_handle(handle.0) };
        file.write_all(&cipher)?;
        file.sync_all()?;
        drop(file);
        unsafe {
            MoveFileExW(
                PCWSTR(os::wide(&temporary).as_ptr()),
                PCWSTR(os::wide(&target).as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(error)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
fn remove(root: &Path) -> Result<()> {
    let target = root.join(FILE_NAME);
    if target.exists() {
        private_file(&target)?;
        std::fs::remove_file(target)?;
    }
    Ok(())
}

fn mark_revoked(root: &Path) -> Result<()> {
    use std::{io::Write, os::windows::io::FromRawHandle};
    use windows::{
        Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::*},
        core::PCWSTR,
    };
    os::protected_path(root)?;
    let path = root.join(REVOKED_MARKER);
    if path.exists() {
        private_file(&path)?;
        return Ok(());
    }
    let descriptor = os::Descriptor::new("O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)")?;
    let handle = unsafe {
        CreateFileW(
            PCWSTR(os::wide(&path).as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            Some(descriptor.attributes()),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
            None,
        )
    }
    .map_err(error)?;
    let mut file = unsafe { std::fs::File::from_raw_handle(handle.0) };
    file.write_all(b"revoked")?;
    file.sync_all()?;
    Ok(())
}
fn clear_marker(root: &Path) -> Result<()> {
    let path = root.join(REVOKED_MARKER);
    if path.exists() {
        private_file(&path)?;
        std::fs::remove_file(path)?;
    }
    Ok(())
}
pub(crate) fn revoke_installation(root: &Path) -> Result<()> {
    remove(root)?;
    clear_marker(root)
}

fn protect(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use windows::{
        Win32::{
            Foundation::{HLOCAL, LocalFree},
            Security::Cryptography::*,
        },
        core::w,
    };
    if bytes.len() as u64 > STORAGE_LIMIT {
        return Err(error("Background authorization exceeds its size limit"));
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let entropy = b"pb-mapper/background-host/v1";
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy.len() as u32,
        pbData: entropy.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                w!("pb-mapper background host"),
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    }
    .map_err(|_| error("Windows could not protect background authorization"))?;
    let result = if output.pbData.is_null() || output.cbData as u64 > STORAGE_LIMIT {
        Err(error("Windows returned invalid background authorization"))
    } else {
        Ok(unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec())
    };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
    }
    result
}

fn private_file(path: &Path) -> Result<()> {
    use windows::{
        Win32::{
            Foundation::{HLOCAL, LocalFree},
            Security::{Authorization::*, *},
        },
        core::PCWSTR,
    };
    os::protected_file(path)?;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let mut owner = PSID::default();
    let mut acl = std::ptr::null_mut();
    unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(os::wide(path).as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut acl),
            None,
            &mut descriptor,
        )
    }
    .ok()
    .map_err(error)?;
    let result = private_acl(owner, acl);
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    result
}
fn private_acl(
    owner: windows::Win32::Security::PSID,
    acl: *const windows::Win32::Security::ACL,
) -> Result<()> {
    use windows::Win32::Security::*;
    let trusted = |sid| unsafe {
        IsWellKnownSid(sid, WinLocalSystemSid).as_bool()
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid).as_bool()
    };
    if !trusted(owner) || acl.is_null() {
        return Err(error(
            "Background authorization has an unsafe owner or DACL",
        ));
    }
    for index in 0..unsafe { (*acl).AceCount } {
        let mut ace = std::ptr::null_mut();
        unsafe { GetAce(acl, u32::from(index), &mut ace) }.map_err(error)?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType == 0 {
            let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = PSID((&allowed.SidStart as *const u32).cast_mut().cast());
            if !trusted(sid) {
                return Err(error(
                    "Background authorization is readable by another identity",
                ));
            }
        } else if header.AceType != 1 {
            return Err(error("Unsupported background authorization ACL"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use windows::Win32::Security::*;
    fn options() -> Options {
        Options {
            server: "127.0.0.1:7666".into(),
            credential: "background-test-key-000000000000".into(),
            relay_transport: RelayTransport::Tcp,
            settings: Settings {
                service: "desktop:background-test".into(),
                display_id: 1,
                allow_control: false,
                allow_direct: false,
                auto_start: true,
            },
        }
    }
    fn state(saved: Saved) -> Supervisor {
        Supervisor {
            saved: Some(saved),
            enabled: true,
            host: None,
            pending: None,
            session: None,
            message: String::new(),
            retry_at: Instant::now(),
            retry_delay: Duration::from_secs(1),
            capture_failure_since: None,
            persistence_failed: false,
        }
    }
    fn descriptor(sddl: &str) -> Result<()> {
        let value = os::Descriptor::new(sddl)?;
        let security = unsafe { (*value.attributes()).lpSecurityDescriptor };
        let mut owner = PSID::default();
        let mut acl = std::ptr::null_mut();
        let mut defaulted = windows::core::BOOL(0);
        let mut present = windows::core::BOOL(0);
        unsafe {
            GetSecurityDescriptorOwner(PSECURITY_DESCRIPTOR(security), &mut owner, &mut defaulted)
                .map_err(error)?;
            GetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(security),
                &mut present,
                &mut acl,
                &mut defaulted,
            )
            .map_err(error)?;
        }
        private_acl(owner, acl)
    }

    #[test]
    fn machine_storage_rejects_readable_grants_and_preserves_namespace_identity() {
        assert!(descriptor("O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)").is_ok());
        assert!(descriptor("O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GR;;;BU)").is_err());
        let original = Saved::prepare(options(), "owner-one", None).unwrap();
        let original_secret = original.identity.encode_secret().unwrap();
        let plaintext = serde_json::to_vec(&original).unwrap();
        let cipher = protect(&plaintext, true).unwrap();
        assert!(
            !cipher
                .windows(original.options.credential.len())
                .any(|part| part == original.options.credential.as_bytes())
        );
        assert_eq!(protect(&cipher, false).unwrap(), plaintext);
        let mut tampered = cipher;
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(protect(&tampered, false).is_err());
        let mut rotated = options();
        rotated.credential = "background-test-key-111111111111".into();
        let retained = Saved::prepare(rotated, "owner-one", Some(&original)).unwrap();
        assert_eq!(retained.identity.encode_secret().unwrap(), original_secret);
        let mut another_namespace = options();
        another_namespace.credential =
            pb_mapper_core::checksum::encode_temporary_credential(9, &[7; 32]);
        let replaced = Saved::prepare(another_namespace, "owner-one", Some(&original)).unwrap();
        assert_ne!(replaced.identity.encode_secret().unwrap(), original_secret);
        let changed_owner = Saved::prepare(options(), "owner-two", Some(&original)).unwrap();
        assert_ne!(
            changed_owner.identity.encode_secret().unwrap(),
            original_secret
        );
        let snapshot = serde_json::to_vec(&state(original.clone()).snapshot()).unwrap();
        assert!(
            !snapshot
                .windows(original.options.credential.len())
                .any(|part| part == original.options.credential.as_bytes())
        );
        assert!(!format!("{:?}", original.options).contains(&original.options.credential));
        // An interrupted revocation must not fall through to loading an older
        // grant (or silently look like an empty installation).
        let directory = std::env::temp_dir().join(format!(
            "pb-background-marker-{}",
            crate::security::secret().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join(REVOKED_MARKER), b"revoked").unwrap();
        assert!(load(&directory, "owner-one").is_err());
        std::fs::remove_file(directory.join(REVOKED_MARKER)).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn failed_configuration_preserves_pending_host_but_failed_revoke_cancels_it() {
        use tokio::io::AsyncReadExt;
        let blackhole = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut configured = options();
        configured.settings.allow_control = true;
        configured.server = blackhole.local_addr().unwrap().to_string();
        let original = Saved::prepare(configured, "owner-one", None).unwrap();
        let original_secret = original.identity.encode_secret().unwrap();
        let mut supervisor = state(original.clone());
        supervisor.pending = Some(tokio::spawn(async move {
            Host::start_with_backend_and_identity(
                &client(&original.options)?,
                HostOptions {
                    service: original.options.settings.service,
                    display_id: 1,
                    allow_control: false,
                    allow_direct: false,
                },
                Arc::new(|| Err(error("capture must never start in this test"))),
                None,
                original.identity,
            )
            .await
        }));
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(2), blackhole.accept())
            .await
            .unwrap()
            .unwrap();
        // This path is outside the protected installation. Failed persistence
        // must leave the running/pending endpoint and its identity untouched.
        let outside = std::env::temp_dir();
        assert!(
            supervisor
                .command(
                    Command::Configure { options: options() },
                    &outside,
                    "owner-one"
                )
                .await
                .is_err()
        );
        assert!(supervisor.enabled);
        assert!(supervisor.pending.is_some());
        assert_eq!(
            supervisor
                .saved
                .as_ref()
                .unwrap()
                .identity
                .encode_secret()
                .unwrap(),
            original_secret
        );
        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                supervisor.command(
                    Command::Permission {
                        allow_control: false
                    },
                    &outside,
                    "owner-one"
                )
            )
            .await
            .unwrap()
            .is_err()
        );
        assert!(supervisor.pending.is_none());
        assert!(!supervisor.enabled);
        assert!(
            !supervisor
                .saved
                .as_ref()
                .unwrap()
                .options
                .settings
                .allow_control
        );
        assert!(
            supervisor
                .command(Command::Start, &outside, "owner-one")
                .await
                .is_err()
        );
        assert!(
            supervisor
                .command(
                    Command::Permission {
                        allow_control: true
                    },
                    &outside,
                    "owner-one"
                )
                .await
                .is_err()
        );
        // The registration owns its TCP socket even while relay readiness is
        // blocked. Cancellation releases it rather than leaking a connection.
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(supervisor.host.is_none());
    }
}
