//! Atomic replace and parent-directory durability.
//!
//! These are general file primitives, not credential ones — they lived in the
//! auth persistence layer only because that is where the first caller was.
//! [`replace_contents`] is the whole sequence for a state file; [`replace_file`]
//! and [`sync_parent_directory`] are its last two steps, for callers that write
//! their new file some other way. Those two report `io::Result` and leave it to
//! the caller to map into its own error type.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use rand::RngExt;
use snafu::Snafu;

/// The permissions [`replace_contents`] gives the new file on unix. Ignored
/// elsewhere, where the new file takes the platform's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewFileMode {
    /// What creating a file gives it: `0o666` less the umask.
    Default,
    /// Exactly this mode. The file is created with it, so it is never more
    /// open than this, and set to it before its first byte is written, so
    /// the umask takes nothing away either.
    Exactly(u32),
    /// The replaced file's permissions, or [`Self::Default`]'s when there is
    /// no file to replace.
    KeepExisting,
}

/// The step of [`replace_contents`] that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceStep {
    /// Creating the temporary file beside the target.
    Create,
    /// Giving the temporary file its [`NewFileMode`].
    SetPermissions,
    /// Writing the temporary file and syncing it.
    Write,
    /// Renaming the temporary file over the target.
    Replace,
    /// Syncing the directory that holds the target.
    SyncParent,
}

impl ReplaceStep {
    fn verb(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::SetPermissions => "set the permissions of",
            Self::Write => "write",
            Self::Replace => "replace",
            Self::SyncParent => "sync the directory of",
        }
    }
}

/// A [`replace_contents`] failure: the step, the file it touched and the cause.
#[derive(Debug, Snafu)]
#[snafu(display("failed to {} `{}`", step.verb(), path.display()))]
pub struct ReplaceError {
    pub step: ReplaceStep,
    /// The temporary file for [`ReplaceStep::Create`],
    /// [`ReplaceStep::SetPermissions`] and [`ReplaceStep::Write`]; the target
    /// for [`ReplaceStep::Replace`] and [`ReplaceStep::SyncParent`].
    pub path: PathBuf,
    source: std::io::Error,
}

impl ReplaceError {
    /// The underlying failure, for callers that word their own message.
    pub fn io(&self) -> &std::io::Error {
        &self.source
    }
}

impl From<ReplaceError> for std::io::Error {
    /// The underlying failure, kind and text unchanged, for `io::Result` callers.
    fn from(error: ReplaceError) -> Self {
        error.source
    }
}

/// Replaces `path` with `bytes` all at once, so a crash leaves the old file or
/// the new one, never a torn one.
///
/// The bytes go to a new file beside `path`, named
/// `.{file name}.tmp-{pid}-{16 random hex digits}`, opened with `create_new`
/// and given `mode` before its first byte. That file is synced, renamed over
/// `path` with [`replace_file`], and the directory synced.
/// On failure the temporary file is removed and `path` is left as it was —
/// unless only the final directory sync failed, by which time it is replaced.
///
/// The directory must exist: each caller creates it with its own wording for
/// that failure. The directory synced is the one [`sync_parent_directory`]
/// syncs, so a bare file name, whose parent is the empty path, fails that last
/// step; join it to `.` to mean the current directory.
pub fn replace_contents(path: &Path, bytes: &[u8], mode: NewFileMode) -> Result<(), ReplaceError> {
    let Some(file_name) = path.file_name() else {
        return Err(ReplaceError {
            step: ReplaceStep::Create,
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"),
        });
    };
    let suffix: u64 = rand::rng().random();
    let temporary = path.with_file_name(format!(
        ".{}.tmp-{}-{suffix:016x}",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    create_with_mode(&mut options, mode);
    let file = options.open(&temporary).map_err(|source| ReplaceError {
        step: ReplaceStep::Create,
        path: temporary.clone(),
        source,
    })?;
    let result = write_and_replace(file, &temporary, path, bytes, mode);
    if result.is_err() {
        // Best effort: after a rename this names nothing, and either way no
        // later call would clean it up.
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn write_and_replace(
    mut file: File,
    temporary: &Path,
    path: &Path,
    bytes: &[u8],
    mode: NewFileMode,
) -> Result<(), ReplaceError> {
    let failed = |step, at: &Path| {
        let path = at.to_path_buf();
        move |source| ReplaceError { step, path, source }
    };
    set_mode(&file, path, mode).map_err(failed(ReplaceStep::SetPermissions, temporary))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(failed(ReplaceStep::Write, temporary))?;
    drop(file);
    replace_file(temporary, path).map_err(failed(ReplaceStep::Replace, path))?;
    sync_parent_directory(path).map_err(failed(ReplaceStep::SyncParent, path))
}

#[cfg(unix)]
fn create_with_mode(options: &mut OpenOptions, mode: NewFileMode) {
    use std::os::unix::fs::OpenOptionsExt;
    if let NewFileMode::Exactly(mode) = mode {
        options.mode(mode);
    }
}

#[cfg(not(unix))]
fn create_with_mode(_: &mut OpenOptions, _: NewFileMode) {}

#[cfg(unix)]
fn set_mode(file: &File, target: &Path, mode: NewFileMode) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = match mode {
        NewFileMode::Default => return Ok(()),
        NewFileMode::Exactly(mode) => std::fs::Permissions::from_mode(mode),
        NewFileMode::KeepExisting => match std::fs::metadata(target) {
            Ok(metadata) => metadata.permissions(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        },
    };
    file.set_permissions(permissions)
}

#[cfg(not(unix))]
fn set_mode(_: &File, _: &Path, _: NewFileMode) -> std::io::Result<()> {
    Ok(())
}

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
/// A path with no parent is a no-op rather than an error. A bare file name's
/// parent is the empty path, which fails to open, so its sync fails; join it
/// to `.` to sync the current directory.
pub fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    sync_directory(parent)
}

fn sync_directory(directory: &Path) -> std::io::Result<()> {
    open_directory_for_sync(directory).and_then(|directory| directory.sync_all())
}

fn open_directory_for_sync(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempPath;

    /// A directory of its own for one test, created.
    fn scratch(label: &str) -> TempPath {
        let directory = TempPath::new(label);
        std::fs::create_dir(&directory).unwrap();
        directory
    }

    /// The names directly inside `directory`, sorted.
    fn entries(directory: &Path) -> Vec<std::ffi::OsString> {
        let mut names = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn replace_contents_creates_then_replaces_and_leaves_only_the_target() {
        let scratch = scratch("replace");
        let path = scratch.join("state.json");

        replace_contents(&path, b"first", NewFileMode::Default).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        replace_contents(&path, b"second", NewFileMode::KeepExisting).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(entries(&scratch), ["state.json"]);
    }

    /// A failed replace names its step, keeps the target and removes the
    /// temporary file it wrote.
    #[test]
    fn a_failed_replace_reports_its_step_and_removes_the_temporary() {
        let scratch = scratch("replace-fails");
        let path = scratch.join("occupied");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("kept"), b"kept").unwrap();

        let error = replace_contents(&path, b"new", NewFileMode::Default).unwrap_err();
        assert_eq!(error.step, ReplaceStep::Replace);
        assert_eq!(error.path, path);
        assert_eq!(entries(&scratch), ["occupied"]);
        assert_eq!(std::fs::read(path.join("kept")).unwrap(), b"kept");
    }

    /// Syncing a directory opens it, which Windows allows only with backup
    /// semantics.
    #[test]
    fn sync_parent_directory_succeeds_for_a_local_file() {
        let scratch = scratch("dirsync");
        let path = scratch.join("probe");
        std::fs::write(&path, b"x").unwrap();
        sync_parent_directory(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replace_contents_sets_the_requested_mode() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = scratch("replace-mode");
        let path = scratch.join("secret");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        replace_contents(&path, b"key", NewFileMode::Exactly(0o600)).unwrap();
        assert_eq!(mode(&path), 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        replace_contents(&path, b"key", NewFileMode::KeepExisting).unwrap();
        assert_eq!(mode(&path), 0o640);
    }
}
