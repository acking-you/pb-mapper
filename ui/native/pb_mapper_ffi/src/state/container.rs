//! The Mac app's data, once out of its sandbox container.
//!
//! Up to 0.11.0 the app was sandboxed, and macOS points a sandboxed app's
//! home at its container (`~/Library/Containers/<bundle>/Data`), so
//! everything it kept is there. Since 0.12.0 it is not: an app that posts
//! input to other apps cannot be sandboxed. The same paths now resolve
//! under the real home, where nothing is yet. So once, before anything reads
//! them, what the container holds is copied out, never moved, so that a
//! 0.11.0 the user goes back to still finds its own:
//! - the configuration directory (tunnels, the relay credential, settings,
//!   the remote desktop's state) and the Flutter app's support directory
//!   (the update record, thumbnails), each where the home has none yet;
//! - the preferences, through Core Foundation's preferences, which macOS
//!   caches and would not see a file copied behind its back: each key the
//!   app does not have yet, at the same time as the configuration
//!   directory, and so also once.
//!
//! Nothing here runs in a sandboxed process, whose home is its container
//! already. The Keychain needs nothing: its items are the login Keychain's,
//! not the container's, and their access lists name the app by its code
//! signature, which the sandbox does not change.

use std::io;
use std::path::{Path, PathBuf};

/// The app's bundle identifier, as `ui/macos/Runner/Configs/AppInfo.xcconfig`
/// sets it.
pub(crate) const BUNDLE: &str = "com.pb-mapper.ui";

/// What the app keeps under its home: the configuration directory first,
/// then the Flutter app's support directory.
const KEPT: [&str; 2] = [
    "Library/Application Support/pb-mapper-ui",
    "Library/Application Support/com.pb-mapper.ui",
];

/// The home a sandboxed app had: its container's.
fn container(home: &Path) -> PathBuf {
    home.join("Library/Containers").join(BUNDLE).join("Data")
}

/// What to copy out of the container under `home`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Each directory the container has and the home does not, `(from, to)`.
    pub directories: Vec<(PathBuf, PathBuf)>,
    /// The preferences too: the configuration directory is being copied,
    /// which happens once.
    pub preferences: bool,
}

/// What a process copies: nothing when it is sandboxed itself, else each
/// kept directory the container has and the home lacks.
pub(crate) fn plan(home: &Path, sandboxed: bool, exists: impl Fn(&Path) -> bool) -> Plan {
    if sandboxed {
        return Plan {
            directories: Vec::new(),
            preferences: false,
        };
    }
    let container = container(home);
    let directories: Vec<_> = KEPT
        .iter()
        .map(|kept| (container.join(kept), home.join(kept)))
        .filter(|(from, to)| exists(from) && !exists(to))
        .collect();
    let preferences = directories
        .first()
        .is_some_and(|(_, to)| *to == home.join(KEPT[0]));
    Plan {
        directories,
        preferences,
    }
}

/// Copy the directory `from` to `to`, which does not exist: into a sibling
/// first, renamed into place once every file is written and synced, so a
/// copy cut short is never taken for a finished one. Symbolic links and
/// other special files are left out.
pub(crate) fn copy_directory(from: &Path, to: &Path) -> io::Result<()> {
    let name = to
        .file_name()
        .ok_or_else(|| io::Error::other("a directory to copy to has no name"))?;
    let partial = to.with_file_name(format!(".{}.copying", name.to_string_lossy()));
    if partial.exists() {
        std::fs::remove_dir_all(&partial)?;
    }
    let copied = copy_tree(from, &partial).and_then(|()| std::fs::rename(&partial, to));
    if copied.is_err() {
        let _ = std::fs::remove_dir_all(&partial);
    }
    copied?;
    pb_mapper_core::durable_file::sync_parent_directory(to)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
            std::fs::OpenOptions::new()
                .write(true)
                .open(&target)?
                .sync_all()?;
            pb_mapper_core::durable_file::sync_parent_directory(&target)?;
        }
    }
    Ok(())
}

/// Copy the app's data out of its container, at most once per process, and
/// at its first launch outside the sandbox only. A failure is logged; the
/// container keeps everything for the next try or a downgrade.
#[cfg(target_os = "macos")]
pub(crate) fn adopt() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let sandboxed = std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some();
        let plan = plan(&home, sandboxed, Path::exists);
        for (from, to) in &plan.directories {
            match copy_directory(from, to) {
                Ok(()) => tracing::info!(
                    event = "app_data_copied_from_container",
                    from = %from.display(),
                    to = %to.display(),
                ),
                Err(failure) => tracing::warn!(
                    event = "app_data_not_copied_from_container",
                    from = %from.display(),
                    detail = %failure,
                ),
            }
        }
        if plan.preferences {
            preferences::adopt(&container(&home));
        }
    });
}

#[cfg(target_os = "macos")]
mod preferences {
    use super::BUNDLE;
    use objc2_core_foundation::{
        CFData, CFDictionary, CFPreferencesAppSynchronize, CFPreferencesCopyValue,
        CFPreferencesSetValue, CFPropertyListCreateWithData, CFRetained, CFString, CFType,
        kCFPreferencesAnyHost, kCFPreferencesCurrentUser,
    };
    use std::path::Path;

    /// Set each preference the container's file holds and the app does not,
    /// through Core Foundation, which keeps the preferences cache coherent.
    pub(super) fn adopt(container: &Path) {
        let file = container
            .join("Library/Preferences")
            .join(format!("{BUNDLE}.plist"));
        let Ok(bytes) = std::fs::read(&file) else {
            return;
        };
        let data = CFData::from_bytes(&bytes);
        // SAFETY: immutable options, and no format or error wanted (both
        // may be null).
        let read = unsafe {
            CFPropertyListCreateWithData(
                None,
                Some(&data),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        let Some(Ok(dictionary)) = read.map(CFRetained::downcast::<CFDictionary>) else {
            return;
        };
        // SAFETY: a preferences file's top level maps strings to property
        // list values.
        let dictionary =
            unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(dictionary) };
        let app = CFString::from_str(BUNDLE);
        // SAFETY: Core Foundation's constants, alive for the process.
        let (user, host) = unsafe { (kCFPreferencesCurrentUser, kCFPreferencesAnyHost) };
        let (keys, values) = dictionary.to_vecs();
        let mut adopted = 0;
        for (key, value) in keys.iter().zip(&values) {
            if CFPreferencesCopyValue(key, &app, user, host).is_some() {
                continue;
            }
            // SAFETY: a property list value read from a preferences file.
            unsafe { CFPreferencesSetValue(key, Some(value), &app, user, host) };
            adopted += 1;
        }
        if adopted > 0 && !CFPreferencesAppSynchronize(&app) {
            tracing::warn!(event = "app_preferences_not_saved");
        }
        tracing::info!(event = "app_preferences_copied_from_container", adopted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempRoot;

    /// Outside the sandbox, at the first launch, both directories and the
    /// preferences are copied; a directory the home has already is not, and
    /// without the configuration directory nor are the preferences; a
    /// sandboxed process copies nothing.
    #[test]
    fn the_container_is_copied_once_and_only_outside_the_sandbox() {
        let home = Path::new("/Users/someone");
        let container = container(home);
        let (config, support) = (home.join(KEPT[0]), home.join(KEPT[1]));
        let in_container = |path: &Path| path.starts_with(&container);
        let first = plan(home, false, in_container);
        assert_eq!(
            first.directories,
            [
                (container.join(KEPT[0]), config.clone()),
                (container.join(KEPT[1]), support.clone()),
            ]
        );
        assert!(first.preferences);
        let later = plan(home, false, |path| in_container(path) || *path == config);
        assert_eq!(later.directories, [(container.join(KEPT[1]), support)]);
        assert!(!later.preferences);
        assert_eq!(plan(home, true, in_container).directories, []);
        let bundle = format!("PRODUCT_BUNDLE_IDENTIFIER = {BUNDLE}");
        assert!(
            include_str!("../../../../macos/Runner/Configs/AppInfo.xcconfig")
                .lines()
                .any(|line| line.trim() == bundle)
        );
    }

    /// A tree is copied whole, a leftover of a copy cut short is replaced,
    /// and nothing of the copying is left behind.
    #[test]
    fn a_directory_is_copied_whole_or_not_at_all() {
        let root = TempRoot::new("container-copy");
        let (from, to) = (root.path().join("from"), root.path().join("to"));
        std::fs::create_dir_all(from.join("desktop")).unwrap();
        std::fs::write(from.join("config.json"), b"{\"tunnels\":1}").unwrap();
        std::fs::write(from.join("desktop/vault"), b"sealed").unwrap();
        let partial = root.path().join(".to.copying");
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(partial.join("stale"), b"x").unwrap();
        copy_directory(&from, &to).unwrap();
        assert_eq!(
            std::fs::read(to.join("config.json")).unwrap(),
            b"{\"tunnels\":1}"
        );
        assert_eq!(std::fs::read(to.join("desktop/vault")).unwrap(), b"sealed");
        assert!(!to.join("stale").exists() && !partial.exists());
        assert!(from.join("config.json").exists());
    }
}
