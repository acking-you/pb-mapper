//! The service's bounded diagnostics files in the protected installation.
use super::{Descriptor, install_root, protected_file, protected_path, wide};
use crate::{Result, error};
use std::os::windows::io::FromRawHandle;
use windows::{
    Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::*},
    core::PCWSTR,
};

pub fn record_service_failure(failure: &std::io::Error) {
    record_service_event("service_failed", &failure.to_string());
}

/// A diagnostics file in the protected installation root, opened to append:
/// SYSTEM and administrators own it, users may read it. Its length.
pub fn open_service_log(name: &str) -> Result<(std::fs::File, u64)> {
    use std::io::{Seek, SeekFrom};
    let root = install_root()?;
    protected_path(&root)?;
    let path = root.join(name);
    if path.exists() {
        protected_file(&path)?;
    }
    let descriptor = Descriptor::new("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GR;;;BU)")?;
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide(&path).as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            Some(descriptor.attributes()),
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(error)?;
    let mut file = unsafe { std::fs::File::from_raw_handle(handle.0) };
    let len = file.seek(SeekFrom::End(0))?;
    Ok((file, len))
}

/// Keep one older copy of a diagnostics file: `name` becomes `name.1`,
/// replacing the last one.
pub fn rotate_service_log(name: &str) -> Result<()> {
    let root = install_root()?;
    protected_path(&root)?;
    std::fs::rename(root.join(name), root.join(format!("{name}.1")))
}

/// Bounded control-path diagnostics; never pass grants, keys or input events.
pub fn record_service_event(event: &str, detail: &str) {
    let write = || -> Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        let (mut file, len) = open_service_log("service.log")?;
        if len > 64 * 1024 {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
        }
        let text: String = detail
            .chars()
            .take(2000)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        writeln!(file, "{now} {event}: {text}")?;
        Ok(())
    };
    let _ = write();
}
