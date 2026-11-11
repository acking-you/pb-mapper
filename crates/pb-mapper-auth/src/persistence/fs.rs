//! Directory lock, atomic replace, and parent-directory durability.
use super::super::*;
use pb_mapper_core::durable_file::{NewFileMode, ReplaceStep, replace_contents};

use super::lock_path;

/// Create the state directory and take `auth.lock` before any credential or
/// snapshot file is read or written.
pub(crate) fn prepare_state_dir_and_lock(state_dir: &Path) -> Result<Arc<File>, AuthFailure> {
    prepare_state_dir(state_dir)?;
    Ok(Arc::new(acquire_state_dir_lock(state_dir)?))
}

pub fn acquire_state_dir_lock(state_dir: &Path) -> Result<File, AuthFailure> {
    let path = lock_path(state_dir);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            AuthFailure::new(
                codes::AUTH_STATE_UNAVAILABLE,
                format!("failed to open `{}`: {error}", path.display()),
                false,
            )
        })?;
    lock_exclusive_nonblock(&file).map_err(|error| {
        AuthFailure::new(
            codes::AUTH_STATE_LOCKED,
            format!(
                "authentication state directory `{}` is already in use: {error}",
                state_dir.display()
            ),
            false,
        )
    })?;
    Ok(file)
}

fn lock_exclusive_nonblock(file: &File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        const LOCK_EX: i32 = 2;
        const LOCK_NB: i32 = 4;
        use std::os::unix::io::AsRawFd;
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
        const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x2;
        #[repr(C)]
        struct Overlapped {
            internal: usize,
            internal_high: usize,
            offset: u32,
            offset_high: u32,
            event: *mut core::ffi::c_void,
        }
        unsafe extern "system" {
            fn LockFileEx(
                file: *mut core::ffi::c_void,
                flags: u32,
                reserved: u32,
                bytes_low: u32,
                bytes_high: u32,
                overlapped: *mut Overlapped,
            ) -> i32;
        }
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: core::ptr::null_mut(),
        };
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_FAIL_IMMEDIATELY | LOCKFILE_EXCLUSIVE_LOCK,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Ok(())
    }
}

/// `core`'s durability primitive, reported as an `AuthFailure`.
pub(crate) fn sync_parent_directory(path: &Path) -> Result<(), AuthFailure> {
    pb_mapper_core::durable_file::sync_parent_directory(path).map_err(|error| {
        AuthFailure::new(
            codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
            format!("failed to sync `{}`: {error}", path.display()),
            false,
        )
    })
}

pub(crate) fn prepare_state_dir(path: &Path) -> Result<(), AuthFailure> {
    std::fs::create_dir_all(path).map_err(|error| {
        AuthFailure::new(
            codes::AUTH_STATE_UNAVAILABLE,
            format!(
                "failed to create auth state directory `{}`: {error}",
                path.display()
            ),
            false,
        )
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        AuthFailure::new(
            codes::AUTH_STATE_UNAVAILABLE,
            format!(
                "failed to secure auth state directory `{}`: {error}",
                path.display()
            ),
            false,
        )
    })?;
    Ok(())
}

/// Write `data` to `path`, claiming the path itself and failing if it is taken.
///
/// Returns `false` — having written nothing — when `path` already exists.
///
/// Unlike [`atomic_write`], which renames a finished temporary file over its
/// destination, this creates the destination exclusively. Rename always wins, so
/// two processes writing the same path both believe they succeeded and one
/// silently loses; `O_EXCL` makes exactly one of them the winner and tells the
/// other. Use it where the file must not be replaced.
///
/// The trade-off is that the contents are no longer written atomically: a crash
/// between the create and the final write leaves the file short. A caller must
/// therefore treat contents it cannot parse as somebody else's file rather than
/// its own.
pub(crate) fn create_new_write(path: &Path, data: &[u8], mode: u32) -> Result<bool, AuthFailure> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AuthFailure::new(
                codes::AUTH_STATE_UNAVAILABLE,
                format!("failed to create `{}`: {error}", parent.display()),
                false,
            )
        })?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    // Created with its mode, not chmod-ed afterwards: this path is the
    // destination, so a window where it is readable by anyone is a window on the
    // real file.
    #[cfg(unix)]
    options.mode(mode);
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => {
            return Err(AuthFailure::new(
                codes::AUTH_STATE_UNAVAILABLE,
                format!("failed to create `{}`: {error}", path.display()),
                false,
            ));
        }
    };
    // The file exists from here on, so every failure removes it: leaving a
    // half-written one behind would block the next attempt with contents that
    // mean nothing.
    let result = (|| {
        file.write_all(data)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                AuthFailure::new(
                    codes::AUTH_STATE_UNAVAILABLE,
                    format!("failed to write `{}`: {error}", path.display()),
                    false,
                )
            })?;
        drop(file);
        sync_parent_directory(path)
    })();
    match result {
        Ok(()) => Ok(true),
        Err(error) => {
            let _ = std::fs::remove_file(path);
            Err(error)
        }
    }
}

/// Replace `path` with `data` all at once, `mode` on unix, creating its
/// directory first.
///
/// The sequence is core's [`replace_contents`]; each of its steps fails with
/// the code and text this crate has always reported for it.
pub(crate) fn atomic_write(path: &Path, data: &[u8], mode: u32) -> Result<(), AuthFailure> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            AuthFailure::new(
                codes::AUTH_STATE_UNAVAILABLE,
                format!("failed to create `{}`: {error}", parent.display()),
                false,
            )
        })?;
    }
    replace_contents(path, data, NewFileMode::Exactly(mode)).map_err(|failure| {
        // The temporary file for the first three steps, `path` for the last two.
        let at = failure.path.display();
        let error = failure.io();
        let unavailable = |message| AuthFailure::new(codes::AUTH_STATE_UNAVAILABLE, message, false);
        match failure.step {
            ReplaceStep::Create => unavailable(format!("failed to open `{at}`: {error}")),
            ReplaceStep::SetPermissions => {
                AuthFailure::internal(format!("failed to set key permissions: {error}"))
            }
            ReplaceStep::Write => unavailable(format!("failed to write `{at}`: {error}")),
            ReplaceStep::Replace => unavailable(format!("failed to replace `{at}`: {error}")),
            // What `sync_parent_directory` reports for the same failure.
            ReplaceStep::SyncParent => AuthFailure::new(
                codes::TEMPORARY_KEY_STORE_UNAVAILABLE,
                format!("failed to sync `{at}`: {error}"),
                false,
            ),
        }
    })
}
