//! Atomic replace and parent-directory durability.
//!
//! These are general file primitives, not credential ones — they lived in the
//! auth persistence layer only because that is where the first caller was. Both
//! report `io::Result` and leave it to the caller to map into its own error
//! type.

use std::fs::File;
use std::path::Path;

/// Replaces `to` with `from`, atomically where the platform allows it.
pub fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        unsafe extern "system" {
            fn MoveFileExW(
                lp_existing_file_name: *const u16,
                lp_new_file_name: *const u16,
                dw_flags: u32,
            ) -> i32;
        }
        fn wide(path: &Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }
        let from_w = wide(from);
        let to_w = wide(to);
        let ok = unsafe {
            MoveFileExW(
                from_w.as_ptr(),
                to_w.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(from, to)
}

/// Fsyncs the directory holding `path`, so a rename into it survives a crash.
///
/// A path with no parent is a no-op rather than an error.
pub fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    open_directory_for_sync(parent).and_then(|directory| directory.sync_all())
}

fn open_directory_for_sync(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        OpenOptions::new()
            .access_mode(GENERIC_READ | GENERIC_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }
    #[cfg(not(windows))]
    File::open(path)
}
