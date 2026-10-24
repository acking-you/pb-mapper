//! Atomic replace and parent-directory durability.
//!
//! These are general file primitives, not credential ones — they lived in the
//! auth persistence layer only because that is where the first caller was. Both
//! report `io::Result` and leave it to the caller to map into its own error
//! type.

use std::fs::File;
use std::path::Path;

/// Replaces `to` with `from`, atomically where the platform allows it.
///
/// This is `std::fs::rename` everywhere. On Windows, std tries `MoveFileExW` and,
/// when that is refused with `ERROR_ACCESS_DENIED` — as it is while any handle
/// is open on `to` — retries through `FileRenameInfoEx` with POSIX semantics,
/// which replace `to` regardless (Windows 10 1607 and later, on a file system
/// that supports it). A bare `MoveFileExW` stops at the refusal.
///
/// The rename is not durable on its own on any platform: callers follow it
/// with [`sync_parent_directory`]. They rename from the same directory, so the
/// `MOVEFILE_WRITE_THROUGH` flag std does not pass would add nothing — it only
/// flushes a move that crosses volumes.
pub fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
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
