use super::*;
use std::{
    ffi::OsString,
    fs::File,
    io::Write,
    os::windows::io::{AsRawHandle, FromRawHandle},
    path::Path,
    time::{Duration, Instant},
};
use windows::{
    Win32::{Foundation::*, Storage::FileSystem::*},
    core::PCWSTR,
};
use windows_service::{
    service::{
        Service, ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState,
        ServiceType,
    },
    service_manager::{ServiceManager, ServiceManagerAccess},
};
const FILE_LIMIT: usize = 20_000;
const BYTE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const INSTALL_ACL: &str = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;BU)";

fn manager() -> Result<ServiceManager> {
    ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(error)
}
fn stop(service: &Service) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match service.query_status().map_err(error)?.current_state {
            ServiceState::Stopped => return Ok(()),
            ServiceState::StartPending | ServiceState::StopPending => {}
            _ => {
                service.stop().map_err(error)?;
            }
        }
        if Instant::now() >= deadline {
            return Err(error("helper stop timed out"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn start(service: &Service) -> Result<()> {
    service.start::<OsString>(&[]).map_err(error)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let status = service.query_status().map_err(error)?;
        match status.current_state {
            ServiceState::Running => return Ok(()),
            ServiceState::Stopped => {
                return Err(error(format!(
                    "helper failed to start ({:?})",
                    status.exit_code
                )));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(error("helper start timed out"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn info(config: &Configuration, start_type: ServiceStartType) -> ServiceInfo {
    ServiceInfo {
        name: SERVICE.into(),
        display_name: "pb-mapper authorized desktop helper".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type,
        error_control: ServiceErrorControl::Normal,
        executable_path: config.helper_executable.clone(),
        launch_arguments: vec!["service".into()],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    }
}

// Apply owner and DACL at creation: changing them afterwards leaves an owner
// WRITE_DAC race even when the inherited file permissions are read-only.
fn create_file(path: &Path, descriptor: &os::Descriptor) -> Result<File> {
    let handle = unsafe {
        CreateFileW(
            PCWSTR(os::wide(path).as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            Some(descriptor.attributes()),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(error)?;
    Ok(unsafe { File::from_raw_handle(handle.0) })
}
fn create_dir(path: &Path, descriptor: &os::Descriptor) -> Result<()> {
    unsafe {
        CreateDirectoryW(
            PCWSTR(os::wide(path).as_ptr()),
            Some(descriptor.attributes()),
        )
    }
    .map_err(error)
}
struct Installation {
    root: PathBuf,
    descriptor: os::Descriptor,
    _lock: File,
}
impl Installation {
    fn open() -> Result<Self> {
        let root = os::install_root()?;
        let descriptor = os::Descriptor::new(INSTALL_ACL)?;
        if !root.exists() {
            create_dir(&root, &descriptor)?;
        }
        os::protected_path(&root)?;
        let path = root.join("installation.lock");
        if path.exists() {
            os::protected_file(&path)?;
        }
        let handle = unsafe {
            CreateFileW(
                PCWSTR(os::wide(&path).as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0 | 0x00010000,
                FILE_SHARE_MODE(0),
                Some(descriptor.attributes()),
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_DELETE_ON_CLOSE,
                None,
            )
        }
        .map_err(|e| {
            error(format!(
                "another helper installation is active or its lock is unavailable: {e}"
            ))
        })?;
        let lock = unsafe { File::from_raw_handle(handle.0) };
        Ok(Self {
            root,
            descriptor,
            _lock: lock,
        })
    }
    fn atomic_write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let destination = self.root.join(name);
        if destination.exists() {
            os::protected_file(&destination)?;
        }
        let temporary = self
            .root
            .join(format!("install-{}.tmp", crate::security::secret()?));
        let result = (|| {
            let mut file = create_file(&temporary, &self.descriptor)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            unsafe {
                MoveFileExW(
                    PCWSTR(os::wide(&temporary).as_ptr()),
                    PCWSTR(os::wide(&destination).as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(error)
        })();
        if result.is_err() && temporary.exists() {
            os::protected_file(&temporary)?;
            std::fs::remove_file(&temporary)?;
        }
        result
    }
    fn configuration(&self, config: &Configuration) -> Result<()> {
        self.atomic_write(
            "installation.json",
            &serde_json::to_vec(config).map_err(error)?,
        )
    }
    fn log(&self, result: &Result<()>) {
        let text = result.as_ref().map_or_else(|e| format!("Helper operation failed: {e}"),
            |()| "Helper operation completed. Files still in use are scheduled for removal at the next normal Windows restart; no restart was requested.".into());
        let text: String = text.chars().take(1000).collect();
        let _ = self.atomic_write("installation.log", text.as_bytes());
    }
}
fn read_source(path: &Path, directory: bool) -> Result<File> {
    let handle = unsafe {
        CreateFileW(
            PCWSTR(os::wide(path).as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT
                | if directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    FILE_ATTRIBUTE_NORMAL
                },
            None,
        )
    }
    .map_err(error)?;
    let file = unsafe { File::from_raw_handle(handle.0) };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .map_err(error)?;
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || (information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0) != directory
        || (!directory && information.nNumberOfLinks > 1)
    {
        return Err(error(
            "bundle contains a reparse point, hard-linked file or unexpected object",
        ));
    }
    Ok(file)
}
fn pin_source_ancestors(source: &Path) -> Result<Vec<File>> {
    if !source.is_absolute()
        || source
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(error(
            "bundle path must be absolute without parent traversal",
        ));
    }
    let mut handles = Vec::new();
    let ancestors: Vec<_> = source.ancestors().filter(|p| p.is_absolute()).collect();
    for path in ancestors.into_iter().rev() {
        let handle = unsafe {
            CreateFileW(
                PCWSTR(os::wide(path).as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(error)?;
        let file = unsafe { File::from_raw_handle(handle.0) };
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
            .map_err(error)?;
        if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(error(
                "bundle ancestors must not contain junctions or symbolic links",
            ));
        }
        handles.push(file);
    }
    Ok(handles)
}
fn copy_tree(
    source: &Path,
    target: &Path,
    descriptor: &os::Descriptor,
    depth: usize,
    count: &mut usize,
    bytes: &mut u64,
) -> Result<()> {
    if depth > 16 {
        return Err(error("bundle directory nesting exceeds the limit"));
    }
    // Holding every parent without write/delete sharing prevents a junction
    // replacement from redirecting the privileged read while this copy runs.
    let _source = read_source(source, true)?;
    create_dir(target, descriptor)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        *count += 1;
        if *count > FILE_LIMIT {
            return Err(error("bundle contains too many files"));
        }
        let destination = target.join(entry.file_name());
        if entry.path().symlink_metadata()?.is_dir() {
            copy_tree(
                &entry.path(),
                &destination,
                descriptor,
                depth + 1,
                count,
                bytes,
            )?;
        } else {
            let mut input = read_source(&entry.path(), false)?;
            *bytes += input.metadata()?.len();
            if *bytes > BYTE_LIMIT {
                return Err(error("bundle exceeds the installation size limit"));
            }
            let mut output = create_file(&destination, descriptor)?;
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
        }
    }
    Ok(())
}
// Cleanup is restricted to verified descendants. Unique release paths prevent
// a delayed delete from touching a reinstall performed before the next reboot.
fn remove_tree(
    root: &Path,
    path: &Path,
    defer_all: bool,
    depth: usize,
    count: &mut usize,
) -> Result<()> {
    if depth > 16 || !path.is_absolute() || !path.starts_with(root) || path == root {
        return Err(error("refusing cleanup outside a release directory"));
    }
    os::protected_path(path)?;
    *count += 1;
    if *count > FILE_LIMIT {
        return Err(error("cleanup file limit exceeded"));
    }
    let directory = path.is_dir();
    if directory {
        for entry in std::fs::read_dir(path)? {
            remove_tree(root, &entry?.path(), defer_all, depth + 1, count)?;
        }
    }
    if !defer_all {
        let result = if directory {
            std::fs::remove_dir(path)
        } else {
            std::fs::remove_file(path)
        };
        match result {
            Ok(()) => return Ok(()),
            Err(e) if matches!(e.raw_os_error(), Some(5 | 32 | 33 | 145)) => {}
            Err(e) => return Err(e),
        }
    }
    unsafe {
        MoveFileExW(
            PCWSTR(os::wide(path).as_ptr()),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    }
    .map_err(error)
}
fn release_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| {
            name.strip_prefix("release-").is_some_and(|id| {
                id.len() == 43
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            })
        })
}
fn cleanup_releases(
    installation: &Installation,
    keep: Option<&Path>,
    defer_all: bool,
) -> Result<()> {
    let mut count = 0;
    for entry in std::fs::read_dir(&installation.root)? {
        let path = entry?.path();
        if release_directory(&path) && keep != Some(path.as_path()) {
            let marker = path.join("cleanup.pending");
            if defer_all {
                os::protected_path(&path)?;
                if marker.exists() {
                    os::protected_file(&marker)?;
                    if std::fs::metadata(&marker)?.len() == 6
                        && std::fs::read(&marker)? == b"queued"
                    {
                        continue;
                    }
                    std::fs::remove_file(&marker)?;
                }
                // The marker is scheduled before its parent directory. Only a
                // completed traversal marks it queued, so partial failures retry.
                drop(create_file(&marker, &installation.descriptor)?);
            }
            remove_tree(&installation.root, &path, defer_all, 0, &mut count)?;
            if defer_all {
                std::fs::write(marker, b"queued")?;
            }
        }
    }
    Ok(())
}
fn validate_service(service: &Service, config: &Configuration) -> Result<ServiceStartType> {
    let actual = service.query_config().map_err(error)?;
    let mut count = 0;
    let arguments = unsafe {
        windows::Win32::UI::Shell::CommandLineToArgvW(
            PCWSTR(os::wide(&actual.executable_path).as_ptr()),
            &mut count,
        )
    };
    if arguments.is_null() {
        return Err(error("invalid helper service command line"));
    }
    let command_matches = count == 2
        && unsafe {
            (*arguments)
                .to_string()
                .is_ok_and(|path| os::same_path(Path::new(&path), &config.helper_executable))
                && (*arguments.add(1))
                    .to_string()
                    .is_ok_and(|arg| arg == "service")
        };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(arguments.cast())));
    }
    if !command_matches
        || actual.service_type != ServiceType::OWN_PROCESS
        || actual
            .account_name
            .as_ref()
            .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("LocalSystem"))
    {
        return Err(error(
            "existing service does not match the protected helper installation",
        ));
    }
    Ok(actual.start_type)
}
pub fn install(source: &Path, sid: &str) -> Result<()> {
    let manager = manager()?;
    let executable = std::env::current_exe()?;
    if !sid.starts_with("S-1-")
        || sid.len() > 184
        || !sid
            .bytes()
            .all(|c| c.is_ascii_digit() || c == b'S' || c == b'-')
    {
        return Err(error("invalid authorized user SID"));
    }
    let _sid_check = os::Descriptor::new(&format!("D:P(A;;GR;;;{sid})"))?;
    if !os::same_path(&source.join(EXECUTABLE), &executable)
        || !source.join("pb_mapper_ui.exe").is_file()
        || !source.join("pb_mapper_ffi.dll").is_file()
        || !source.join("data/app.so").is_file()
    {
        return Err(error(
            "install from a complete pb-mapper Windows release bundle",
        ));
    }
    let installation = Installation::open()?;
    let result = install_bundle(&installation, &manager, source, sid);
    installation.log(&result);
    result
}
fn install_bundle(
    installation: &Installation,
    manager: &ServiceManager,
    source: &Path,
    sid: &str,
) -> Result<()> {
    let _source_ancestors = pin_source_ancestors(source)?;
    let previous = if installation.root.join("installation.json").exists() {
        Some(configuration()?)
    } else {
        None
    };
    let release = installation
        .root
        .join(format!("release-{}", crate::security::secret()?));
    let mut preserve_release = false;
    let result = (|| {
        copy_tree(
            source,
            &release,
            &installation.descriptor,
            0,
            &mut 0,
            &mut 0,
        )?;
        let config = Configuration {
            schema: 1,
            owner_sid: sid.into(),
            ui_executable: release.join("pb_mapper_ui.exe"),
            helper_executable: release.join(EXECUTABLE),
        };
        os::protected_file(&config.ui_executable)?;
        os::protected_file(&config.helper_executable)?;
        let access = ServiceAccess::QUERY_STATUS
            | ServiceAccess::QUERY_CONFIG
            | ServiceAccess::STOP
            | ServiceAccess::START
            | ServiceAccess::CHANGE_CONFIG
            | ServiceAccess::DELETE;
        let (service, old_state) = match manager.open_service(SERVICE, access) {
            Ok(service) => {
                let old = previous
                    .as_ref()
                    .ok_or_else(|| error("existing service has no valid protected installation"))?;
                let start_type = validate_service(&service, old)?;
                let running =
                    service.query_status().map_err(error)?.current_state != ServiceState::Stopped;
                stop(&service)?;
                (service, Some((start_type, running)))
            }
            Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(1060) => (
                manager
                    .create_service(&info(&config, ServiceStartType::AutoStart), access)
                    .map_err(error)?,
                None,
            ),
            Err(e) => return Err(error(e)),
        };
        let update = (|| {
            service
                .change_config(&info(&config, ServiceStartType::AutoStart))
                .map_err(error)?;
            installation.configuration(&config)?;
            service.set_description("Explicitly installed, local authenticated capture/input bridge. No remote listener or unattended consent.").map_err(error)?;
            start(&service)
        })();
        if let Err(failure) = update {
            let rollback = (|| {
                stop(&service)?;
                if let Some(old) = &previous {
                    installation.configuration(old)?;
                } else if installation.root.join("installation.json").exists() {
                    os::protected_file(&installation.root.join("installation.json"))?;
                    std::fs::remove_file(installation.root.join("installation.json"))?;
                }
                if let Some((start_type, running)) = old_state {
                    let old = previous
                        .as_ref()
                        .ok_or_else(|| error("missing rollback configuration"))?;
                    service
                        .change_config(&info(old, start_type))
                        .map_err(error)?;
                    if running {
                        start(&service)?;
                    }
                } else {
                    service.delete().map_err(error)?;
                }
                Ok::<_, std::io::Error>(())
            })();
            if let Err(rollback) = rollback {
                preserve_release = true;
                return Err(error(format!(
                    "{failure}; rollback failed: {rollback}. Installation files were retained for repair."
                )));
            }
            return Err(error(format!(
                "{failure}; the previous helper installation was restored"
            )));
        }
        preserve_release = true;
        // An older installed UI may still be open. Preserve its files until a
        // normal reboot; its permission to use the helper is already revoked.
        cleanup_releases(installation, Some(&release), true)
    })();
    if !preserve_release
        && release.exists()
        && let Err(cleanup) = remove_tree(&installation.root, &release, false, 0, &mut 0)
    {
        return Err(error(format!(
            "{}; cleanup failed: {cleanup}",
            result.err().unwrap_or_else(|| error("installation failed"))
        )));
    }
    result
}
pub fn disable() -> Result<()> {
    let manager = manager()?;
    let installation = Installation::open()?;
    let result = (|| {
        let config = configuration()?;
        let service = manager
            .open_service(
                SERVICE,
                ServiceAccess::STOP
                    | ServiceAccess::QUERY_STATUS
                    | ServiceAccess::QUERY_CONFIG
                    | ServiceAccess::CHANGE_CONFIG,
            )
            .map_err(error)?;
        validate_service(&service, &config)?;
        stop(&service)?;
        service
            .change_config(&info(&config, ServiceStartType::Disabled))
            .map_err(error)
    })();
    installation.log(&result);
    result
}
pub fn uninstall() -> Result<()> {
    let manager = manager()?;
    let installation = Installation::open()?;
    let result = (|| {
        match manager.open_service(
            SERVICE,
            ServiceAccess::STOP
                | ServiceAccess::QUERY_STATUS
                | ServiceAccess::QUERY_CONFIG
                | ServiceAccess::DELETE,
        ) {
            Ok(service) => {
                validate_service(&service, &configuration()?)?;
                stop(&service)?;
                service.delete().map_err(error)?;
            }
            Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(1060) => {}
            Err(e) => return Err(error(e)),
        }
        let config = installation.root.join("installation.json");
        if config.exists() {
            os::protected_file(&config)?;
            std::fs::remove_file(config)?;
        }
        cleanup_releases(&installation, None, false)
    })();
    // A bounded protected result log remains. No service or grant survives an
    // otherwise successful uninstall because a loaded DLL cannot be deleted.
    installation.log(&result);
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn installation_source_rejects_hard_linked_files() {
        let directory = std::env::temp_dir().join(format!(
            "pb-mapper-source-{}",
            crate::security::secret().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        let source = directory.join("source");
        let alias = directory.join("alias");
        std::fs::write(&source, b"bundle asset").unwrap();
        assert!(read_source(&source, false).is_ok());
        assert!(read_source(&directory, true).is_ok());
        std::fs::hard_link(&source, &alias).unwrap();
        assert!(read_source(&source, false).is_err());
        assert!(read_source(&alias, false).is_err());
        std::fs::remove_file(&alias).unwrap();
        assert!(read_source(&source, false).is_ok());
        std::fs::remove_file(&source).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }
}
