//! A host display's modes: what a viewer may ask the host to switch its
//! display to, and back. The host changes a mode only dynamically, so the
//! operating system never keeps it, and restores the original when the
//! session ends.
//!
//! Every change is journalled before it is made ([`Journal`]), so a host
//! that crashed is undone at its next start, within the login and on the
//! monitor it was made on: a dynamic mode ends with either. One process
//! keeps one claim per display it changed, with the first original and the
//! latest mode set: two hosts on one display share it, and the last to
//! restore puts it back ([`Guard`]). A display whose mode its owner changed
//! meanwhile is never changed back.

use crate::{FailureKind, Result, error, failure_kind, platform::DisplayModes};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The most modes a list carries.
const MAX_MODES: usize = 64;
/// The largest width or height of a mode.
const MAX_SIDE: u32 = 32_768;
/// The most displays one journal names.
const MAX_ENTRIES: usize = 16;
/// The longest scope an entry keeps.
const MAX_SCOPE: usize = 512;
/// The in-process journal's file in its state directory.
pub(crate) const JOURNAL: &str = "display-modes.json";
/// How long after a change a capture failure is the display still changing,
/// not a failure: a mode change costs the capture its device.
pub(crate) const SETTLE: Duration = Duration::from_secs(3);

/// A display mode's size, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
}
impl Mode {
    fn valid(self) -> bool {
        (1..=MAX_SIDE).contains(&self.width) && (1..=MAX_SIDE).contains(&self.height)
    }
}

/// A display's modes as the host offers them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeList {
    pub current: Mode,
    /// The mode before this host changed it; equal to `current` when unchanged.
    pub original: Mode,
    /// Physical pixels, deduplicated by size, largest first, at most 64.
    pub supported: Vec<Mode>,
}
impl ModeList {
    /// The supported mode that best fits a viewer's display of `local`
    /// size: that size itself, else the largest mode inside it whose aspect
    /// ratio is within 2 % of its own, else none.
    pub fn fit_local(&self, local: Mode) -> Option<Mode> {
        if !local.valid() {
            return None;
        }
        if self.supported.contains(&local) {
            return Some(local);
        }
        let area = |mode: &Mode| u64::from(mode.width) * u64::from(mode.height);
        // |w/h - lw/lh| <= 2 % of lw/lh, without division.
        let near = |mode: &Mode| {
            let (w, h) = (u64::from(mode.width), u64::from(mode.height));
            let (lw, lh) = (u64::from(local.width), u64::from(local.height));
            50 * (w * lh).abs_diff(h * lw) <= h * lw
        };
        self.supported
            .iter()
            .filter(|mode| mode.width <= local.width && mode.height <= local.height && near(mode))
            .max_by_key(|mode| (area(mode), mode.width))
            .copied()
    }
    /// The host changed the display, and has not put it back.
    pub fn changed(&self) -> bool {
        self.current != self.original
    }
    /// At most 64 modes, every size in 1..=32768, `current` and `original`
    /// included.
    pub fn valid(&self) -> bool {
        self.supported.len() <= MAX_MODES
            && self.current.valid()
            && self.original.valid()
            && self.supported.iter().all(|mode| mode.valid())
    }
}

/// What a viewer asks the host's display to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeTarget {
    /// The mode the display had before this host changed it.
    Original,
    /// The supported mode that best fits the viewer's own display.
    FitLocal(Mode),
    /// This supported mode.
    Size(Mode),
}

/// Why a mode change was not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DisplayModeError {
    /// The host offers no display modes.
    NotOffered,
    /// The viewer does not control the host.
    NotControlling,
    /// No supported mode fits the viewer's display.
    NoFit,
    /// The host does not list that mode.
    NotListed,
    /// The session has ended.
    Closed,
}

/// A display mode command for thread C, in order with input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeCommand {
    /// The controlling viewer's choice.
    Set(Mode),
    /// The controlling viewer's way back to the original.
    Restore,
    /// The host's owner's way back, which needs no control.
    HostRestore,
}

/// A display mode as the journal keeps it: its size, and the refresh rate
/// and depth the platform set it at (0 where it has none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Setting {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub bpp: u32,
}
impl Setting {
    pub(crate) fn size(self) -> Mode {
        Mode {
            width: self.width,
            height: self.height,
        }
    }
    fn valid(self) -> bool {
        self.size().valid() && self.hz <= 1000 && self.bpp <= 64
    }
}

/// One display a host changed: what to put back, what it set, and the
/// [`DisplayModes::scope`] the change lives in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub device: String,
    pub scope: String,
    pub original: Setting,
    pub applied: Setting,
    pub pid: u32,
    pub at_unix_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    entries: Vec<Entry>,
}
impl Stored {
    /// 2 added the scope; a version 1 file is discarded, never replayed.
    const VERSION: u32 = 2;
    fn valid(&self) -> bool {
        self.version == Self::VERSION
            && self.entries.len() <= MAX_ENTRIES
            && self.entries.iter().all(|entry| {
                valid_device(&entry.device)
                    && entry.scope.len() <= MAX_SCOPE
                    && entry.original.valid()
                    && entry.applied.valid()
            })
    }
}

/// A Windows display device, `\\.\DISPLAY` and one to three digits: the
/// only displays any platform changes this round. A port widens it.
fn valid_device(device: &str) -> bool {
    device.strip_prefix(r"\\.\DISPLAY").is_some_and(|number| {
        (1..=3).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Where a host records a display mode before it changes it, written
/// durably: a file in the host's state directory, or, for the privileged
/// worker, in the helper's protected one. Without one, a backend changes no
/// mode.
/// The default journal is none: no mode is ever changed.
#[derive(Clone, Debug, Default)]
pub(crate) struct Journal {
    path: Option<PathBuf>,
}
impl Journal {
    /// The journal at `path`: the privileged worker's, in the helper's
    /// protected directory, one per console session.
    #[cfg(all(windows, feature = "windows-helper"))]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }
    /// The journal in a host's state directory.
    pub(crate) fn in_dir(dir: Option<PathBuf>) -> Self {
        Self {
            path: dir.map(|dir| dir.join(JOURNAL)),
        }
    }

    /// Whether it still names a display to put back.
    pub(crate) fn pending(&self) -> bool {
        !self.read().is_empty()
    }

    /// Its entries. A file that fails validation is discarded, never acted
    /// on.
    fn read(&self) -> Vec<Entry> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(failure) => {
                tracing::warn!(
                    target: "pb_mapper_desktop::session",
                    event = "display_mode_journal_unreadable",
                    detail = %failure,
                );
                return Vec::new();
            }
        };
        match serde_json::from_slice::<Stored>(&bytes) {
            Ok(stored) if stored.valid() => stored.entries,
            _ => {
                tracing::warn!(
                    target: "pb_mapper_desktop::session",
                    event = "display_mode_journal_discarded",
                    path = %path.display(),
                );
                let _ = std::fs::remove_file(path);
                Vec::new()
            }
        }
    }

    /// Replace its entries, durably; none removes the file.
    fn write(&self, entries: &[Entry]) -> Result<()> {
        let Some(path) = &self.path else {
            return Err(no_journal());
        };
        if entries.is_empty() {
            return match std::fs::remove_file(path) {
                Ok(()) => pb_mapper_core::durable_file::sync_parent_directory(path),
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(failure) => Err(failure),
            };
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let stored = Stored {
            version: Stored::VERSION,
            entries: entries.to_vec(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(error)?;
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        pb_mapper_core::durable_file::replace_file(&temporary, path)?;
        pb_mapper_core::durable_file::sync_parent_directory(path)
    }

    /// Its entries with `device`'s replaced by `entry`, or removed.
    fn put(&self, device: &str, entry: Option<Entry>) -> Result<()> {
        let mut entries = self.read();
        entries.retain(|kept| kept.device != device);
        entries.extend(entry);
        if entries.len() > MAX_ENTRIES {
            return Err(error("too many displays are changed"));
        }
        self.write(&entries)
    }
}

fn no_journal() -> std::io::Error {
    crate::BackendError::new(
        crate::FailureKind::Unsupported,
        "no state directory to journal a display mode change in",
    )
    .into()
}

/// One display this process changed: the first original, the latest mode
/// any holder set, which the journal names too, and how many backends hold
/// it changed.
struct Claim {
    device: String,
    original: Setting,
    applied: Setting,
    holders: usize,
}

/// This process's claims. Held across every mode change, restore and
/// replay: the process-wide lock that serialises them per display.
static CLAIMS: Mutex<Vec<Claim>> = Mutex::new(Vec::new());

fn claims() -> MutexGuard<'static, Vec<Claim>> {
    CLAIMS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unclaim(claims: &mut Vec<Claim>, device: &str) {
    if let Some(at) = claims.iter().position(|claim| claim.device == device) {
        claims[at].holders -= 1;
        if claims[at].holders == 0 {
            claims.swap_remove(at);
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// What one backend changed of its display's mode, and the way back.
pub(crate) struct Guard {
    journal: Journal,
    applied: Option<Applied>,
}
/// The display a backend holds changed, and the original it put there.
struct Applied {
    device: String,
    original: Setting,
}

impl Guard {
    pub(crate) fn new(journal: Journal) -> Self {
        Self {
            journal,
            applied: None,
        }
    }

    /// `device`'s modes, `original` being what it showed before this
    /// process changed it; `None` where the platform cannot change them now.
    pub(crate) fn list<M: DisplayModes>(
        &self,
        modes: &mut M,
        device: &str,
    ) -> Result<Option<ModeList>> {
        if !modes.available(device) {
            return Ok(None);
        }
        let supported = modes.list(device)?;
        let current = modes.current(device)?.size();
        let original = match &self.applied {
            Some(applied) if applied.device == device => Some(applied.original.size()),
            _ => claims()
                .iter()
                .find(|claim| claim.device == device)
                .map(|claim| claim.original.size()),
        };
        Ok(Some(ModeList {
            current,
            original: original.unwrap_or(current),
            supported,
        }))
    }

    /// Change `device` to `mode`, dynamically. The journal names it first;
    /// a change that fails leaves the journal as it was.
    pub(crate) fn set<M: DisplayModes>(
        &mut self,
        modes: &mut M,
        device: &str,
        mode: Mode,
    ) -> Result<()> {
        if self.journal.path.is_none() {
            return Err(no_journal());
        }
        if self
            .applied
            .as_ref()
            .is_some_and(|applied| applied.device != device)
        {
            return Err(error("another display's mode is changed"));
        }
        let mut claims = claims();
        let target = modes.target(device, mode)?;
        let scope = modes.scope(device)?;
        let claimed = claims.iter().position(|claim| claim.device == device);
        // The first original wins: this backend's own, another host's in
        // this process, else what the display shows now.
        let original = match (&self.applied, claimed) {
            (Some(applied), _) => applied.original,
            (None, Some(at)) => claims[at].original,
            (None, None) => modes.current(device)?,
        };
        let before = self
            .journal
            .read()
            .into_iter()
            .find(|entry| entry.device == device);
        self.journal.put(
            device,
            Some(Entry {
                device: device.to_owned(),
                scope,
                original,
                applied: target,
                pid: std::process::id(),
                at_unix_ms: now_unix_ms(),
            }),
        )?;
        if let Err(failure) = modes.apply(device, &target) {
            // Nothing changed: the journal says again what holds.
            let _ = self.journal.put(device, before);
            return Err(failure);
        }
        match claimed {
            Some(at) => {
                claims[at].applied = target;
                if self.applied.is_none() {
                    claims[at].holders += 1;
                }
            }
            None => claims.push(Claim {
                device: device.to_owned(),
                original,
                applied: target,
                holders: 1,
            }),
        }
        self.applied = Some(Applied {
            device: device.to_owned(),
            original,
        });
        Ok(())
    }

    /// Put back what this backend changed, `before` running first.
    /// `Ok(false)`: nothing was changed, another host of this process still
    /// holds the display changed and puts it back when it ends, or the
    /// owner changed the mode meanwhile, who is never fought. The last
    /// holder compares the display with the latest mode any holder set. A
    /// failure keeps both the change and its journal entry, to be tried
    /// again.
    pub(crate) fn restore<M: DisplayModes>(
        &mut self,
        modes: &mut M,
        before: impl FnOnce(),
    ) -> Result<bool> {
        let Some(device) = self.applied.as_ref().map(|applied| applied.device.clone()) else {
            return Ok(false);
        };
        let mut claims = claims();
        let Some(claim) = claims.iter().find(|claim| claim.device == device) else {
            self.applied = None;
            return Ok(false);
        };
        let (original, latest) = (claim.original, claim.applied);
        if claim.holders > 1 {
            unclaim(&mut claims, &device);
            self.applied = None;
            return Ok(false);
        }
        let current = modes.current(&device)?;
        let ours = current.size() == latest.size();
        if ours {
            before();
            modes.reset(&device, &original)?;
        }
        // Put back, or changed by its owner: either way not ours now.
        if let Err(failure) = self.journal.put(&device, None) {
            tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "display_mode_journal_unwritten",
                detail = %failure,
            );
        }
        unclaim(&mut claims, &device);
        self.applied = None;
        Ok(ours)
    }

    /// Give up the claim, leaving the journal: what a restore that failed
    /// leaves to the replay.
    pub(crate) fn release(&mut self) {
        if let Some(applied) = self.applied.take() {
            unclaim(&mut claims(), &applied.device);
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.release();
    }
}

/// Put back what `journal` says a host changed and did not restore: an
/// entry whose display still shows what was set, in the scope it was set
/// in, is reset to its original. One is dropped whose display shows
/// anything else (its owner changed it since), is gone, or now has another
/// login or monitor: its dynamic mode went with them, and replaying it
/// would force a mode on a monitor that never had it. One that cannot be
/// read or reset now (the secure desktop shows, say) is kept for the next
/// try, warned of once. A display a live backend of this process holds is
/// left to it. How many were restored.
pub(crate) fn recover<M: DisplayModes>(modes: &mut M, journal: &Journal) -> Result<usize> {
    let claims = claims();
    let entries = journal.read();
    if entries.is_empty() {
        return Ok(0);
    }
    let mut kept = Vec::new();
    let mut restored = 0;
    for entry in &entries {
        if claims.iter().any(|claim| claim.device == entry.device) {
            kept.push(entry.clone());
            continue;
        }
        let outcome = modes.scope(&entry.device).and_then(|scope| {
            if scope != entry.scope {
                return Ok(false);
            }
            let shows = modes.current(&entry.device)?.size() == entry.applied.size();
            if shows {
                modes.reset(&entry.device, &entry.original)?;
            }
            Ok(shows)
        });
        match outcome {
            Ok(true) => restored += 1,
            Ok(false) => {}
            Err(failure) if failure_kind(&failure) == Some(FailureKind::DisplayLost) => {}
            Err(failure) => {
                not_restored(&entry.device, &failure);
                kept.push(entry.clone());
                continue;
            }
        }
        settled(&entry.device);
    }
    if kept != entries {
        journal.write(&kept)?;
    }
    Ok(restored)
}

/// The failure last warned of per display, so a replay that keeps failing
/// the same way warns once, not at every retry.
static WARNED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn not_restored(device: &str, failure: &std::io::Error) {
    let detail = failure.to_string();
    let mut warned = WARNED.lock().unwrap_or_else(PoisonError::into_inner);
    let new = !warned
        .iter()
        .any(|(seen, said)| seen == device && *said == detail);
    if new {
        warned.retain(|(seen, _)| seen != device);
        warned.push((device.to_owned(), detail));
        tracing::warn!(
            target: "pb_mapper_desktop::session",
            event = "display_mode_not_restored",
            device = %device,
            detail = %failure,
        );
    }
}

fn settled(device: &str) {
    WARNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|(seen, _)| seen != device);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32) -> Mode {
        Mode { width, height }
    }

    fn setting(width: u32, height: u32) -> Setting {
        Setting {
            width,
            height,
            hz: 60,
            bpp: 32,
        }
    }

    #[test]
    fn a_mode_list_is_valid_only_within_its_bounds() {
        let list = ModeList {
            current: mode(2560, 1440),
            original: mode(2560, 1440),
            supported: vec![mode(2560, 1440), mode(1920, 1080)],
        };
        assert!(list.valid());
        let too_many = ModeList {
            supported: vec![mode(800, 600); MAX_MODES + 1],
            ..list.clone()
        };
        let empty_side = ModeList {
            current: mode(0, 1440),
            ..list.clone()
        };
        let too_wide = ModeList {
            supported: vec![mode(MAX_SIDE + 1, 1440)],
            ..list
        };
        assert!(!too_many.valid() && !empty_side.valid() && !too_wide.valid());
    }

    /// Exact where offered, else the largest inside the local display within
    /// 2 % of its shape, else nothing.
    #[test]
    fn fit_local_takes_the_exact_size_then_the_nearest_shape_then_none() {
        let list = ModeList {
            current: mode(2560, 1440),
            original: mode(2560, 1440),
            supported: vec![
                mode(2560, 1440),
                mode(1920, 1200),
                mode(1920, 1080),
                mode(1280, 720),
                mode(1024, 768),
            ],
        };
        assert_eq!(list.fit_local(mode(1920, 1080)), Some(mode(1920, 1080)));
        // 2048x1152 is 16:9: 1920x1200 is too tall a shape, and 1920x1080
        // the largest 16:9 inside it.
        assert_eq!(list.fit_local(mode(2048, 1152)), Some(mode(1920, 1080)));
        assert_eq!(list.fit_local(mode(1000, 1000)), None);
    }

    /// A display the test steers: what it shows, its scope, whether it is
    /// attached, and each reset.
    struct Steered {
        current: Setting,
        scope: &'static str,
        gone: bool,
        resets: Vec<Setting>,
    }
    fn steered() -> Steered {
        Steered {
            current: setting(2560, 1440),
            scope: "login 1, monitor A",
            gone: false,
            resets: Vec::new(),
        }
    }
    impl DisplayModes for Steered {
        const SUPPORTED: bool = true;
        fn list(&mut self, _: &str) -> Result<Vec<Mode>> {
            Ok(vec![mode(2560, 1440), mode(1920, 1080), mode(1280, 720)])
        }
        fn current(&mut self, _: &str) -> Result<Setting> {
            if self.gone {
                return Err(crate::BackendError::new(FailureKind::DisplayLost, "gone").into());
            }
            Ok(self.current)
        }
        fn scope(&mut self, device: &str) -> Result<String> {
            self.current(device)?;
            Ok(self.scope.to_owned())
        }
        fn target(&mut self, _: &str, mode: Mode) -> Result<Setting> {
            Ok(setting(mode.width, mode.height))
        }
        fn apply(&mut self, _: &str, setting: &Setting) -> Result<()> {
            self.current = *setting;
            Ok(())
        }
        fn reset(&mut self, _: &str, original: &Setting) -> Result<()> {
            self.current = *original;
            self.resets.push(*original);
            Ok(())
        }
    }

    fn journal(name: &str) -> Journal {
        let dir =
            std::env::temp_dir().join(format!("pb-desktop-modes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Journal::in_dir(Some(dir))
    }

    /// A crashed host's journal is replayed once: its display is reset and
    /// the journal cleared. A display its owner changed since is left alone,
    /// as is one now under another login or monitor, or gone; and a journal
    /// that fails validation is discarded unread.
    #[test]
    fn a_crashed_hosts_journal_resets_once_and_never_fights_the_owner() {
        let journal = journal("recover");
        let device = r"\\.\DISPLAY911";
        let mut display = steered();
        let mut crashed = Guard::new(journal.clone());
        crashed.set(&mut display, device, mode(1920, 1080)).unwrap();
        assert_eq!(display.current.size(), mode(1920, 1080));
        // A crash: the guard is gone without restoring.
        drop(crashed);
        assert!(journal.pending());
        assert_eq!(recover(&mut display, &journal).unwrap(), 1);
        assert_eq!(display.resets, [setting(2560, 1440)]);
        assert!(!journal.pending());
        assert_eq!(recover(&mut display, &journal).unwrap(), 0);

        // The owner picked another mode meanwhile: dropped, nothing reset.
        let mut crashed = Guard::new(journal.clone());
        crashed.set(&mut display, device, mode(1920, 1080)).unwrap();
        drop(crashed);
        display.current = setting(1280, 720);
        assert_eq!(recover(&mut display, &journal).unwrap(), 0);
        assert_eq!(display.resets.len(), 1);
        assert!(!journal.pending());

        // Another monitor on that device name, or none: dropped, not reset.
        let changes: [fn(&mut Steered); 2] =
            [|d| d.scope = "login 1, monitor B", |d| d.gone = true];
        for change in changes {
            let mut display = steered();
            let mut crashed = Guard::new(journal.clone());
            crashed.set(&mut display, device, mode(1920, 1080)).unwrap();
            drop(crashed);
            change(&mut display);
            assert_eq!(recover(&mut display, &journal).unwrap(), 0);
            assert!(display.resets.is_empty() && !journal.pending());
        }

        // A journal naming no display device is never acted on.
        let path = journal.path.clone().unwrap();
        let forged = format!(
            r#"{{"version":2,"entries":[{{"device":"C:\\Windows","scope":"login 1, monitor A","original":{},"applied":{},"pid":1,"at_unix_ms":1}}]}}"#,
            serde_json::to_string(&setting(2560, 1440)).unwrap(),
            serde_json::to_string(&setting(1280, 720)).unwrap(),
        );
        std::fs::write(&path, forged).unwrap();
        assert_eq!(recover(&mut display, &journal).unwrap(), 0);
        assert!(!path.exists());
        assert_eq!(display.resets.len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Two hosts of one process on one display: the first to end leaves it
    /// to the other, and the last puts the original back whoever set the
    /// mode it shows.
    #[test]
    fn the_last_of_two_hosts_on_a_display_puts_its_original_back() {
        let journal = journal("shared");
        let device = r"\\.\DISPLAY912";
        let mut display = steered();
        let (mut first, mut second) = (Guard::new(journal.clone()), Guard::new(journal.clone()));
        first.set(&mut display, device, mode(1920, 1080)).unwrap();
        second.set(&mut display, device, mode(1280, 720)).unwrap();
        assert!(!second.restore(&mut display, || {}).unwrap());
        assert_eq!(display.current.size(), mode(1280, 720));
        assert!(journal.pending());
        assert!(first.restore(&mut display, || {}).unwrap());
        assert_eq!(display.resets, [setting(2560, 1440)]);
        assert!(!journal.pending());
        let _ = std::fs::remove_dir_all(journal.path.unwrap().parent().unwrap());
    }
}
