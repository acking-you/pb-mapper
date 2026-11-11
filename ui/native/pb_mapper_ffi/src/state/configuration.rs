//! User-writable configuration storage and in-memory state initialization.
//!
//! ```text
//! app directory -> pb-mapper-ui/config.json -> AppConfig -> process credential
//!               -> services.json / clients.json -> remembered tunnel definitions
//! ```
//!
//! An explicit Flutter app directory wins on every platform. This same directory is
//! the root for relay authentication state, so desktop and mobile UI processes never
//! depend on root-owned `/var/lib` paths.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex as SyncMutex;
use pb_mapper_core::checksum::set_process_msg_header_key;
use pb_mapper_core::durable_file::{self, NewFileMode, ReplaceStep};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::stored::{Unknown, normalize_msg_header_key};
use super::{
    AppConfig, ClientConfigData, ClientConfigStore, ConnectSpec, DirectOptions, PbMapperState,
    ServiceConfigData, ServiceConfigStore, ShareSpec,
};
use crate::error::CtlError;

impl PbMapperState {
    pub(crate) fn desktop_config_dir(&self) -> PathBuf {
        self.config_dir.clone()
    }
    /// A state that has read nothing: the default configuration and no
    /// credential, until `set_app_directory_path` names its directory. Every
    /// client sends that first (the app an empty path on desktop, meaning the
    /// platform's own), so the FFI handle starts here rather than reading a
    /// directory it may never be pointed at.
    pub fn unloaded() -> Self {
        Self {
            embedded_relay: None,
            relay_client: SyncMutex::new(None),
            quic_offer: None,
            service_runtime: HashMap::new(),
            client_runtime: HashMap::new(),
            config: AppConfig::default(),
            config_dir: Self::get_config_dir(&None),
            app_directory_path: None,
            local_server_status: Arc::default(),
            registering: Arc::new(SyncMutex::new(HashSet::new())),
            connecting: Arc::new(SyncMutex::new(HashSet::new())),
        }
    }

    /// A state that loads the configuration in `app_directory_path` at once.
    #[cfg(test)]
    pub fn new(app_directory_path: Option<String>) -> Self {
        let mut state = Self::unloaded();
        if let Err(e) = state.set_app_directory_path(app_directory_path) {
            tracing::error!("Failed to apply MSG_HEADER_KEY during init: {}", e);
        }
        state
    }

    pub fn set_app_directory_path(&mut self, path: Option<String>) -> Result<(), CtlError> {
        let config_dir = Self::get_config_dir(&path);
        // Said here, where it is set, rather than on every load and save:
        // every file this state reads or writes is in it.
        let whose = if path.is_some() {
            "the app's"
        } else {
            "the platform's"
        };
        tracing::info!("Using {whose} config directory: {}", config_dir.display());
        self.app_directory_path = path;
        self.config_dir = config_dir;

        // Reload config from new location if exists
        match self.load_config() {
            Ok(config) => self.config = config,
            Err(e) => {
                tracing::warn!("Failed to reload config after setting app dir: {}", e);
            }
        }
        tracing::info!(
            "Loaded configuration: server_address={}, keep_alive={}, msg_header_key_set={}",
            self.config.server_address,
            self.config.keep_alive_enabled,
            !self.config.msg_header_key.is_empty()
        );
        self.apply_msg_header_key_env()?;

        Ok(())
    }

    pub(super) fn apply_msg_header_key_env(&self) -> Result<(), CtlError> {
        let key = (!self.config.msg_header_key.is_empty()).then_some(&*self.config.msg_header_key);
        // The library validates the key's length and shape; a rejection here is
        // the stored setting being wrong, not something going wrong.
        set_process_msg_header_key(key).map_err(CtlError::invalid_argument)
    }

    pub(crate) fn get_config_dir(app_directory_path: &Option<String>) -> PathBuf {
        // An explicit path wins everywhere. Mobile is where it normally comes
        // from — Flutter hands it over, because there is no OS config dir to
        // discover — but honouring it on desktop too is what lets a test point
        // a state at a temporary directory instead of the user's real config.
        if let Some(app_dir) = app_directory_path {
            return PathBuf::from(app_dir).join("pb-mapper-ui");
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            tracing::warn!("No app directory provided for mobile platform, using relative path");
            PathBuf::from("pb-mapper-ui")
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            if let Some(config_dir) = dirs::config_dir() {
                config_dir.join("pb-mapper-ui")
            } else if let Some(home_dir) = dirs::home_dir() {
                home_dir.join(".config").join("pb-mapper-ui")
            } else {
                tracing::warn!("Could not determine home directory, using current directory");
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join("pb-mapper-ui-config")
            }
        }
    }

    /// `config.json` in the config directory, which is made if need be. One
    /// that cannot be made is an error: the file holds the credential, and
    /// belongs nowhere else, the directory the app runs in least of all.
    fn get_config_file_path(&self) -> Result<PathBuf, CtlError> {
        let config_dir = &self.config_dir;
        std::fs::create_dir_all(config_dir).map_err(|e| {
            CtlError::io(format!(
                "Failed to create config directory {}: {e}",
                config_dir.display()
            ))
        })?;
        Ok(config_dir.join("config.json"))
    }

    pub fn load_config(&self) -> Result<AppConfig, CtlError> {
        let config_path = self.get_config_file_path()?;
        if config_path.exists() {
            let contents =
                fs::read_to_string(config_path).map_err(|e| CtlError::io(e.to_string()))?;
            let mut config: AppConfig =
                serde_json::from_str(&contents).map_err(|e| CtlError::io(e.to_string()))?;
            config.msg_header_key = normalize_msg_header_key(config.msg_header_key)?;
            Ok(config)
        } else {
            Ok(AppConfig::default())
        }
    }

    pub(super) fn write_config_file(&self, config: &AppConfig) -> Result<(), CtlError> {
        let config_path = self.get_config_file_path()?;
        let contents =
            serde_json::to_string_pretty(config).map_err(|e| CtlError::io(e.to_string()))?;
        replace_contents(&config_path, contents.as_bytes())
            .map_err(|e| CtlError::io(e.to_string()))?;
        Ok(())
    }

    fn get_service_config_path(&self) -> PathBuf {
        self.config_dir.join("services.json")
    }

    fn get_client_config_path(&self) -> PathBuf {
        self.config_dir.join("clients.json")
    }

    pub fn load_service_configs(&self) -> Result<ServiceConfigStore, CtlError> {
        read_store(&self.get_service_config_path(), "config")
    }

    pub fn save_service_configs(&self, store: &ServiceConfigStore) -> Result<(), CtlError> {
        write_store(&self.get_service_config_path(), store, "config")
    }

    pub fn save_service_config(&self, spec: &ShareSpec) -> Result<(), CtlError> {
        let ShareSpec {
            service_key,
            local_address,
            protocol,
            enable_encryption,
            enable_keep_alive,
            direct:
                DirectOptions {
                    allow_direct,
                    port_mapping,
                },
        } = spec;
        let mut store = self.load_service_configs()?;
        let saved = store.services.remove(service_key);
        // What the entry saved before holds besides its settings is its own.
        let (created_at, updated_at, unknown) = kept(
            saved
                .as_ref()
                .map(|saved| (saved.created_at, saved.updated_at, &saved.unknown)),
        );

        let mut config = ServiceConfigData {
            service_key: service_key.to_string(),
            local_address: local_address.to_string(),
            protocol: protocol.to_string(),
            enable_encryption: *enable_encryption,
            enable_keep_alive: *enable_keep_alive,
            created_at,
            updated_at,
            allow_direct: *allow_direct,
            port_mapping: *port_mapping,
            unknown,
        };
        match &saved {
            Some(saved) if *saved == config => return Ok(()),
            Some(_) => config.updated_at = Some(SystemTime::now()),
            None => {}
        }

        store.services.insert(service_key.to_string(), config);
        self.save_service_configs(&store)
    }

    pub fn delete_service_config(&self, service_key: &str) -> Result<(), CtlError> {
        let mut store = self.load_service_configs()?;
        store.services.remove(service_key);
        self.save_service_configs(&store)
    }

    pub fn load_client_configs(&self) -> Result<ClientConfigStore, CtlError> {
        read_store(&self.get_client_config_path(), "client config")
    }

    pub fn save_client_configs(&self, store: &ClientConfigStore) -> Result<(), CtlError> {
        write_store(&self.get_client_config_path(), store, "client config")
    }

    pub fn save_client_config(&self, spec: &ConnectSpec) -> Result<(), CtlError> {
        let ConnectSpec {
            service_key,
            local_address,
            protocol,
            enable_keep_alive,
            namespace,
            direct:
                DirectOptions {
                    allow_direct,
                    port_mapping,
                },
        } = spec;
        let mut store = self.load_client_configs()?;
        let saved = store.clients.remove(service_key);
        let (created_at, updated_at, unknown) = kept(
            saved
                .as_ref()
                .map(|saved| (saved.created_at, saved.updated_at, &saved.unknown)),
        );

        let mut config = ClientConfigData {
            service_key: service_key.to_string(),
            local_address: local_address.to_string(),
            protocol: protocol.to_string(),
            enable_keep_alive: *enable_keep_alive,
            created_at,
            updated_at,
            namespace: *namespace,
            allow_direct: *allow_direct,
            port_mapping: *port_mapping,
            unknown,
        };
        match &saved {
            Some(saved) if *saved == config => return Ok(()),
            Some(_) => config.updated_at = Some(SystemTime::now()),
            None => {}
        }

        store.clients.insert(service_key.to_string(), config);
        self.save_client_configs(&store)
    }

    pub fn delete_client_config(&self, service_key: &str) -> Result<(), CtlError> {
        let mut store = self.load_client_configs()?;
        store.clients.remove(service_key);
        self.save_client_configs(&store)
    }
}

/// What a tunnel's entry keeps from the one `saved` before it, besides the
/// settings: when it was created and last changed, and the keys a later
/// build wrote; or, for a tunnel saved for the first time, now, never and
/// none. An entry that comes out as it was saved is not written again, so a
/// tunnel started as it was saved, as the window resumes one, starts on a
/// list that cannot be written; one that changed is written as changed now.
fn kept(
    saved: Option<(SystemTime, Option<SystemTime>, &Unknown)>,
) -> (SystemTime, Option<SystemTime>, Unknown) {
    saved.map_or_else(
        || (SystemTime::now(), None, Unknown::new()),
        |(created_at, updated_at, unknown)| (created_at, updated_at, unknown.clone()),
    )
}

/// The store saved at `path`, or an empty one when there is none or the file
/// holds nothing, as an interrupted in-place write can leave it (empty, or
/// zero-filled after a crash); `noun` names it in the errors. One that
/// cannot be read or does not parse is an error, never an empty store: a
/// save would then write over every tunnel in it.
fn read_store<T: DeserializeOwned + Default>(path: &Path, noun: &str) -> Result<T, CtlError> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => {
            return Err(CtlError::io(format!(
                "Failed to read {noun} file {}: {e}",
                path.display()
            )));
        }
    };
    if content
        .trim_matches(|c: char| c.is_whitespace() || c == '\0')
        .is_empty()
    {
        return Ok(T::default());
    }
    serde_json::from_str(&content).map_err(|e| {
        CtlError::io(format!(
            "Failed to parse {noun} file {}: {e}; fix or remove it",
            path.display()
        ))
    })
}

/// Saves `store` at `path` as pretty JSON; `noun` names it in the errors.
fn write_store<T: Serialize>(path: &Path, store: &T, noun: &str) -> Result<(), CtlError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| CtlError::io(format!("Failed to create config dir: {e}")))?;
    }

    let content = serde_json::to_string_pretty(store)
        .map_err(|e| CtlError::io(format!("Failed to serialize {noun}: {e}")))?;

    replace_contents(path, content.as_bytes())
        .map_err(|e| CtlError::io(format!("Failed to write {noun} file: {e}")))?;
    Ok(())
}

/// Saves `bytes` at `path`, where the in-place write of 0.11.0 (`fs::write`)
/// saved them, and refuses what it refused. A symbolic link at `path` keeps
/// naming the file it points to, which is the one saved (or created, for a
/// dangling link).
///
/// - **Atomic**, so a crash leaves the old file or the new one, never a torn
///   one: core's `replace_contents`, a new file renamed over the old. The new
///   file keeps the old one's mode but is this user's, in this user's group
///   (or its directory's, which is then the same), and on Windows takes its
///   directory's inherited permissions. That is a new file, and an existing
///   one this user may write that has a single name and, on unix, is this
///   user's and in that group, in a directory that takes a new file and lets
///   it replace the old.
/// - **In place**, as 0.11.0 wrote, so a crash can cut the file short: a file
///   with more than one name (a hard link); on unix another user's that this
///   user may write, or one in another group (Windows replaces another
///   user's file, which becomes this user's); a directory that takes no new
///   file; and a file the rename may not replace (refused, a mount point, or
///   on another device).
/// - **Refused**: an existing file this user may not open for writing, such
///   as a read-only one or another user's.
///
/// Making the rename itself durable is best effort: by then the new contents
/// are in place, and a file system that cannot sync a directory should not
/// turn every save into a failure.
fn replace_contents(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = linked_file(path)?;
    // A bare file name is in the current directory, whose path is empty and
    // so names no directory to sync.
    let target = match target.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new(".").join(target),
        _ => target,
    };
    // Opened for writing first, so a file `fs::write` refuses is refused
    // here too rather than replaced by one this user owns.
    match fs::OpenOptions::new().write(true).open(&target) {
        Ok(file) if stays_in_place(&file, &target)? => {
            drop(file);
            tracing::debug!(
                "Saving {} in place, a replacement would not be the same file",
                path.display()
            );
            return fs::write(&target, bytes);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match durable_file::replace_contents(&target, bytes, NewFileMode::KeepExisting) {
        Err(failure) if failure.step == ReplaceStep::SyncParent => {
            tracing::debug!(
                "Saved {} without syncing its directory: {}",
                path.display(),
                failure.io()
            );
            Ok(())
        }
        Err(failure) if writes_in_place(&failure) => {
            tracing::debug!(
                "Saving {} in place, it cannot be replaced: {}",
                path.display(),
                failure.io()
            );
            fs::write(&target, bytes)
        }
        result => result.map_err(io::Error::from),
    }
}

/// Whether a failed atomic save is one an in-place write gets past: the
/// directory takes no new file, or the old file may not be replaced, being
/// protected, a mount point or on another device.
fn writes_in_place(failure: &durable_file::ReplaceError) -> bool {
    use io::ErrorKind::{CrossesDevices, PermissionDenied, ResourceBusy};
    match failure.step {
        ReplaceStep::Create => failure.io().kind() == PermissionDenied,
        ReplaceStep::Replace => matches!(
            failure.io().kind(),
            PermissionDenied | ResourceBusy | CrossesDevices
        ),
        ReplaceStep::SetPermissions | ReplaceStep::Write | ReplaceStep::SyncParent => false,
    }
}

/// Whether only an in-place write keeps `file`, at `path`, the same file: it
/// has a name besides the one it was opened by (a hard link), another user
/// owns it, whose file a replacement, being this user's, would take away, or
/// it is in a group a replacement would not take. A new file takes this
/// user's group, or on BSD and macOS, and in a set-group-ID directory, its
/// directory's.
#[cfg(unix)]
fn stays_in_place(file: &fs::File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    unsafe extern "C" {
        /// The effective user, which owns a file this process creates.
        safe fn geteuid() -> u32;
        /// The effective group, which a file this process creates takes
        /// unless its directory gives it its own.
        safe fn getegid() -> u32;
    }
    /// Whether a new file takes its directory's group whatever the
    /// directory's mode.
    const TAKES_DIRECTORY_GROUP: bool = cfg!(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ));
    /// The set-group-ID bit of a directory's mode.
    const SET_GROUP_ID: u32 = 0o2000;
    let metadata = file.metadata()?;
    let directory = path.parent().map(fs::metadata).transpose()?;
    let replacement_group = match directory {
        Some(directory) if TAKES_DIRECTORY_GROUP || directory.mode() & SET_GROUP_ID != 0 => {
            directory.gid()
        }
        _ => getegid(),
    };
    Ok(metadata.nlink() > 1 || metadata.uid() != geteuid() || metadata.gid() != replacement_group)
}

/// Whether only an in-place write keeps `file` the same file: on Windows,
/// one with a name besides the one it was opened by (a hard link), as a
/// dotfiles tree makes them. Its owner and permissions a replacement does not
/// keep: stable Rust reads neither, and in the app's own directory a file
/// that is not this user's comes from a run as administrator, which a
/// replacement gives back to the user.
#[cfg(windows)]
fn stays_in_place(file: &fs::File, _: &Path) -> io::Result<bool> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    /// `BY_HANDLE_FILE_INFORMATION`: the attributes, three `FILETIME`s, the
    /// volume serial number and two halves of the size, all `DWORD`s, then the
    /// link count and two halves of the file index.
    #[repr(C)]
    struct FileInformation {
        _before: [u32; 10],
        links: u32,
        _index: [u32; 2],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(file: *mut c_void, information: *mut FileInformation) -> i32;
    }
    let mut information = std::mem::MaybeUninit::<FileInformation>::uninit();
    // SAFETY: `file` owns the handle for the call, and the call writes one
    // `BY_HANDLE_FILE_INFORMATION`, which `information` has room for.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        // A file system that does not say has no links to keep: replaced,
        // as before this was asked.
        tracing::debug!("Cannot read the link count: {}", io::Error::last_os_error());
        return Ok(false);
    }
    // SAFETY: the call succeeded, so it wrote every field.
    let information = unsafe { information.assume_init() };
    Ok(information.links > 1)
}

/// Whether only an in-place write keeps `file` the same file: nowhere else.
#[cfg(not(any(unix, windows)))]
fn stays_in_place(_: &fs::File, _: &Path) -> io::Result<bool> {
    Ok(false)
}

/// The file a write to `path` reaches: `path` itself, or the end of the
/// chain of symbolic links at it, a relative one taken from the link's own
/// directory. The end need not exist.
fn linked_file(path: &Path) -> io::Result<PathBuf> {
    /// Links followed before giving up, as Linux does, so a loop ends.
    const MAX_LINKS: usize = 40;
    let mut target = path.to_path_buf();
    for _ in 0..MAX_LINKS {
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let link = fs::read_link(&target)?;
                target = match target.parent() {
                    Some(directory) => directory.join(link),
                    None => link,
                };
            }
            _ => return Ok(target),
        }
    }
    Err(io::Error::other(format!(
        "{} leads through too many symbolic links",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempRoot, read_only};

    /// A save replaces the file whole and leaves nothing beside it.
    #[test]
    fn a_save_replaces_the_file_and_leaves_no_temporary() {
        let root = TempRoot::new("replace");
        let path = root.path().join("services.json");
        replace_contents(&path, b"first").unwrap();
        #[cfg(unix)]
        let first = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&path).unwrap());
        replace_contents(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        // This user's file, in its group, with one name, is a new file now.
        #[cfg(unix)]
        assert_ne!(
            std::os::unix::fs::MetadataExt::ino(&fs::metadata(&path).unwrap()),
            first
        );
    }

    /// A save that cannot replace the file fails and leaves nothing beside it.
    #[test]
    fn a_failed_save_leaves_no_temporary() {
        let root = TempRoot::new("replace-fails");
        let path = root.path().join("services.json");
        fs::create_dir_all(path.join("kept")).unwrap();
        replace_contents(&path, b"new").unwrap_err();
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(path.join("kept").is_dir());
    }

    /// A save through a symbolic link writes the file it points to and keeps
    /// the link; a dangling link has its file created.
    #[test]
    fn a_save_through_a_link_writes_the_linked_file() {
        let root = TempRoot::new("replace-link");
        fs::create_dir(root.path().join("dotfiles")).unwrap();
        let link = root.path().join("services.json");
        let linked = Path::new("dotfiles").join("services.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&linked, &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&linked, &link).is_err() {
            // Windows makes links only in Developer Mode or elevated.
            return;
        }
        let target = root.path().join(linked);
        replace_contents(&link, b"created").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"created");
        replace_contents(&link, b"replaced").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"replaced");
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    }

    /// A directory that takes no new file still has its file saved, in
    /// place.
    #[test]
    fn a_save_in_a_directory_that_takes_no_new_file_writes_in_place() {
        let root = TempRoot::new("replace-locked");
        let directory = root.path().join("locked");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("services.json");
        fs::write(&path, b"old").unwrap();
        lock(&directory, true);
        let saved = replace_contents(&path, b"new");
        lock(&directory, false);
        saved.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }

    /// A file with a second name, a hard link, is written in place, so both
    /// names read what was saved: on Windows too, where a dotfiles tree
    /// takes hard links, which need no privilege as symbolic links do.
    #[test]
    fn a_save_reaches_every_hard_link_to_the_file() {
        let root = TempRoot::new("replace-hard-link");
        let path = root.path().join("services.json");
        let other = root.path().join("dotfiles-services.json");
        fs::write(&path, b"old").unwrap();
        fs::hard_link(&path, &other).unwrap();
        replace_contents(&path, b"first").unwrap();
        replace_contents(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read(&other).unwrap(), b"second");
    }

    /// Another user's file this user may write is written in place, so it
    /// stays that user's, as 0.11.0's write left it, rather than becoming
    /// this user's. Only root can give a file away; elsewhere this checks
    /// nothing.
    #[cfg(unix)]
    #[test]
    fn another_users_file_stays_theirs() {
        use std::os::unix::fs::MetadataExt;
        /// `nobody` on Linux.
        const OTHER: u32 = 65534;
        let root = TempRoot::new("replace-owner");
        let path = root.path().join("config.json");
        fs::write(&path, b"old").unwrap();
        if std::os::unix::fs::chown(&path, Some(OTHER), None).is_err() {
            return;
        }
        replace_contents(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::metadata(&path).unwrap().uid(), OTHER);
    }

    /// A file in another group than a replacement would take, as one shared
    /// through a group is, is written in place, so it keeps its group. Only
    /// a member of a second group, or root, can give a file one; elsewhere
    /// this checks nothing.
    #[cfg(unix)]
    #[test]
    fn a_file_in_another_group_keeps_it() {
        use std::os::unix::fs::MetadataExt;
        let root = TempRoot::new("replace-group");
        let path = root.path().join("config.json");
        fs::write(&path, b"old").unwrap();
        let own = fs::metadata(&path).unwrap().gid();
        // `nogroup`, `users` and `adm` on Linux; `staff` and `admin` on macOS.
        let Some(other) = [65534, 100, 4, 20, 80]
            .into_iter()
            .find(|&gid| gid != own && std::os::unix::fs::chown(&path, None, Some(gid)).is_ok())
        else {
            return;
        };
        replace_contents(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::metadata(&path).unwrap().gid(), other);

        // In a directory that gives its files that group, a replacement
        // keeps it too, so the file is replaced whole.
        use std::os::unix::fs::PermissionsExt;
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        if std::os::unix::fs::chown(&shared, None, Some(other)).is_err()
            || fs::set_permissions(&shared, fs::Permissions::from_mode(0o2700)).is_err()
        {
            return;
        }
        let path = shared.join("config.json");
        fs::write(&path, b"old").unwrap();
        let before = fs::metadata(&path).unwrap();
        if before.gid() != other {
            // A file system that ignores the bit.
            return;
        }
        replace_contents(&path, b"new").unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(after.gid(), other);
        assert_ne!(after.ino(), before.ino());
    }

    /// A file this user may not write is refused, as an in-place write
    /// refuses it, rather than replaced by one the user owns.
    #[test]
    fn a_file_this_user_may_not_write_is_left_alone() {
        let root = TempRoot::new("replace-read-only");
        let path = root.path().join("services.json");
        fs::write(&path, b"old").unwrap();
        read_only(&path, true);
        if fs::OpenOptions::new().write(true).open(&path).is_ok() {
            // Root writes it anyway, in place as atomically.
            read_only(&path, false);
            return;
        }
        let saved = replace_contents(&path, b"new");
        let contents = fs::read(&path).unwrap();
        read_only(&path, false);
        assert_eq!(saved.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(contents, b"old");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    /// A file its directory will not let be replaced, though it may be
    /// written, is saved in place.
    #[cfg(windows)]
    #[test]
    fn a_file_that_may_not_be_replaced_is_written_in_place() {
        let root = TempRoot::new("replace-kept");
        let directory = root.path().join("kept");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("services.json");
        fs::write(&path, b"old").unwrap();
        // Neither the file's own delete right nor its directory's.
        deny(&path, Some("DE"));
        deny(&directory, Some("DC"));
        let saved = replace_contents(&path, b"new");
        deny(&directory, None);
        deny(&path, None);
        saved.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }

    /// Makes `directory` take no new file, or take them again; its files stay
    /// writable.
    #[cfg(unix)]
    fn lock(directory: &Path, locked: bool) {
        use std::os::unix::fs::PermissionsExt;
        let mode = if locked { 0o555 } else { 0o755 };
        fs::set_permissions(directory, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Makes `directory` take no new file, or take them again; its files stay
    /// writable.
    #[cfg(windows)]
    fn lock(directory: &Path, locked: bool) {
        deny(directory, locked.then_some("WD,AD"));
    }

    /// Denies this user `rights` on `path` alone, in an entry nothing inside
    /// it inherits; or, given none, removes what was denied.
    #[cfg(windows)]
    fn deny(path: &Path, rights: Option<&str>) {
        let user = std::env::var("USERNAME").unwrap();
        let mut icacls = std::process::Command::new("icacls");
        icacls.arg(path);
        match rights {
            Some(rights) => icacls.args(["/deny", &format!("{user}:({rights})")]),
            None => icacls.args(["/remove:d", &user]),
        };
        let output = icacls.output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}
