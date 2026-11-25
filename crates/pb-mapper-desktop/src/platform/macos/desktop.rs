//! The displays of this Mac and the one a session is bound to: geometry in
//! pixels from Core Graphics, each display named by its UUID, which outlives
//! a mode change or a reconnection that gives it another id; and the
//! session check: a locked screen, or a login switched away from the
//! console, is the secure desktop, which an ordinary process cannot reach.
//! Capture and the pointer reuse a check that passed within 16 ms
//! ([`Reuse`]), until the session's own display mode change or restore;
//! input never does.

use super::{cf, geometry::Geometry, reuse::Reuse};
use crate::{
    BackendError, DesktopKind, Display, DisplaySelection, FailureKind, Result, platform::Desktop,
};
use objc2_core_foundation::{CFBoolean, CFNumber, CFRetained, CFString, CFUUID};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode,
    CGGetActiveDisplayList, CGMainDisplayID, CGSessionCopyCurrentDictionary,
};
use std::{cell::Cell, ptr::NonNull, rc::Rc, time::Instant};

/// The bound display's geometry, as the desktop last read it: the injector
/// and the pointer convert with it. On thread C only.
pub(crate) type Shared = Rc<Cell<Geometry>>;

/// The most displays enumerated.
const MAX_DISPLAYS: usize = 32;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    /// ColorSync's, which the ApplicationServices umbrella re-exports on
    /// every macOS this app runs on. Follows the Create rule.
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> Option<NonNull<CFUUID>>;
}

pub(crate) fn display_lost(message: &str) -> std::io::Error {
    BackendError::new(FailureKind::DisplayLost, message).into()
}

/// The active displays' ids: those that draw the desktop.
pub(crate) fn active() -> Vec<CGDirectDisplayID> {
    let mut ids = [0; MAX_DISPLAYS];
    let mut count = 0;
    // SAFETY: both pointers are to local storage of the size given.
    let failed =
        unsafe { CGGetActiveDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
    if failed.0 != 0 {
        return Vec::new();
    }
    ids.get(..count as usize)
        .map_or_else(Vec::new, <[u32]>::to_vec)
}

pub(crate) fn any_display() -> bool {
    !active().is_empty()
}

/// A display's UUID, the name that finds it again.
pub(crate) fn uuid(id: CGDirectDisplayID) -> Option<String> {
    // SAFETY: any id is accepted; an unknown one gives NULL.
    let uuid = unsafe { CGDisplayCreateUUIDFromDisplayID(id) }?;
    // SAFETY: a Create-rule result, released when this drops.
    let uuid = unsafe { CFRetained::from_raw(uuid) };
    CFUUID::new_string(None, Some(&uuid)).map(|name| name.to_string())
}

/// The display of `name` among the active ones.
pub(crate) fn by_name(name: &str) -> Option<CGDirectDisplayID> {
    active()
        .into_iter()
        .find(|id| uuid(*id).as_deref() == Some(name))
}

/// The geometry of display `id` now; `None` when it has no mode, as one
/// unplugged has not.
pub(crate) fn geometry(id: CGDirectDisplayID) -> Option<Geometry> {
    let bounds = CGDisplayBounds(id);
    if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
        return None;
    }
    let mode = CGDisplayCopyDisplayMode(id)?;
    Some(Geometry::new(
        (
            bounds.origin.x,
            bounds.origin.y,
            bounds.size.width,
            bounds.size.height,
        ),
        CGDisplayMode::pixel_width(Some(&mode)),
        CGDisplayMode::width(Some(&mode)),
    ))
}

fn describe(id: CGDirectDisplayID) -> Option<Display> {
    Some(geometry(id)?.display(id, uuid(id)?))
}

pub(crate) fn displays() -> Result<Vec<Display>> {
    Ok(active().into_iter().filter_map(describe).collect())
}

/// The display `selection` names now. An id is taken as given, even when it
/// is stale, as hosting always took it: its sessions then report the display
/// lost.
pub(crate) fn resolve(selection: &DisplaySelection) -> Result<Display> {
    match selection {
        DisplaySelection::Id(id) => Ok(describe(*id).unwrap_or_else(|| Display {
            id: *id,
            name: String::new(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        })),
        DisplaySelection::Primary => {
            describe(CGMainDisplayID()).ok_or_else(|| display_lost("no display is active"))
        }
        DisplaySelection::Device(name) => by_name(name)
            .and_then(describe)
            .ok_or_else(|| display_lost("selected display is no longer available")),
    }
}

/// Whether this process's login owns the console, and whether the screen is
/// locked (`DesktopKind::Secure`) or unlocked (`DesktopKind::Ordinary`).
/// Fast User Switching to another login or running without a WindowServer
/// session is outside this process's console (`FailureKind::SecureDesktop`).
pub(crate) fn on_console() -> Result<DesktopKind> {
    let Some(session) = CGSessionCopyCurrentDictionary() else {
        return Err(secure("this process has no window server session"));
    };
    // A boolean, or on some systems a number.
    let flag = |key: &'static str| {
        let key = CFString::from_static_str(key);
        cf::value::<CFBoolean>(&session, &key)
            .map(|flag| flag.as_bool())
            .or_else(|| {
                cf::value::<CFNumber>(&session, &key)?
                    .as_i64()
                    .map(|n| n != 0)
            })
    };
    if flag("kCGSSessionOnConsoleKey") == Some(false) {
        return Err(secure("another login is using the screen"));
    }
    if flag("CGSSessionScreenIsLocked") == Some(true) {
        return Ok(DesktopKind::Secure);
    }
    Ok(DesktopKind::Ordinary)
}

/// Whether this Mac's active console session currently shows the lock screen.
pub(crate) fn is_locked() -> bool {
    matches!(on_console(), Ok(DesktopKind::Secure))
}

fn secure(why: &str) -> std::io::Error {
    BackendError::new(
        FailureKind::SecureDesktop,
        format!("{why}; the picture resumes when the owner unlocks the Mac"),
    )
    .into()
}

/// The display a session captures and injects on.
pub(crate) struct MacDesktop {
    id: CGDirectDisplayID,
    /// Its UUID, which finds it again when its id goes stale.
    name: String,
    geometry: Shared,
    kind: DesktopKind,
    generation: u64,
    /// The last passing session check, and the last geometry read with the
    /// id it was read under, which capture and the pointer reuse.
    reuse: Reuse<(CGDirectDisplayID, Geometry), DesktopKind>,
    reads: Reads,
}

/// How many times a desktop read WindowServer, of the session and of the
/// geometry: counted in tests, and nothing in other builds.
#[derive(Default)]
struct Reads {
    #[cfg(test)]
    session: Cell<u32>,
    #[cfg(test)]
    geometry: Cell<u32>,
}

impl Reads {
    fn count_session(&self) {
        #[cfg(test)]
        self.session.set(self.session.get() + 1);
    }
    fn count_geometry(&self) {
        #[cfg(test)]
        self.geometry.set(self.geometry.get() + 1);
    }
}

impl MacDesktop {
    /// `display`, by its id, or by its name when that id is gone. When the
    /// display is asleep, still link-training or hidden behind a locked Mac,
    /// its identity is kept so thread C can report `SecureDesktop` or wake
    /// and bind it once it draws instead of failing the session at start.
    pub(crate) fn open(display: &Display) -> Result<Self> {
        let resolved = match geometry(display.id) {
            Some(read)
                if display.name.is_empty()
                    || uuid(display.id).as_deref() == Some(&display.name) =>
            {
                Some((display.id, read))
            }
            _ if !display.name.is_empty() => {
                by_name(&display.name).and_then(|id| Some((id, geometry(id)?)))
            }
            _ => None,
        };
        let (id, name, initial) = match resolved {
            Some((id, read)) => (id, uuid(id).unwrap_or_else(|| display.name.clone()), read),
            None => (
                if display.name.is_empty() {
                    display.id
                } else {
                    0
                },
                display.name.clone(),
                Geometry::new(
                    (
                        f64::from(display.x),
                        f64::from(display.y),
                        f64::from(display.width.max(1)),
                        f64::from(display.height.max(1)),
                    ),
                    display.width as usize,
                    display.width as usize,
                ),
            ),
        };
        let kind = on_console().unwrap_or(DesktopKind::Ordinary);
        Ok(Self {
            id,
            name,
            geometry: Rc::new(Cell::new(initial)),
            kind,
            generation: 0,
            reuse: Reuse::default(),
            reads: Reads::default(),
        })
    }

    pub(crate) fn shared(&self) -> Shared {
        self.geometry.clone()
    }

    /// The WindowServer reads this desktop made so far: of the session, and
    /// of the geometry.
    #[cfg(test)]
    pub(super) fn reads(&self) -> (u32, u32) {
        (self.reads.session.get(), self.reads.geometry.get())
    }

    /// The session check, reused within 16 ms of a pass when `recent`.
    /// Flipping between unlocked (`Ordinary`) and locked (`Secure`) advances
    /// `generation` and runs `on_switch` so held input is released and a
    /// fresh `ScreenCaptureKit` stream is opened across the boundary.
    fn session(&mut self, recent: bool, on_switch: impl FnOnce()) -> Result<()> {
        let reads = &self.reads;
        let next = match self.reuse.session(Instant::now(), recent, || {
            reads.count_session();
            on_console()
        }) {
            Ok(kind) => kind,
            Err(err) => {
                on_switch();
                return Err(err);
            }
        };
        if next != self.kind {
            on_switch();
            self.kind = next;
            self.generation = self.generation.wrapping_add(1);
            self.reuse.forget_geometry();
        }
        Ok(())
    }

    /// Whether the display is still `expected`; with `recent`, a geometry
    /// read within 16 ms that matched answers.
    fn matches(&mut self, expected: &Display, recent: bool) -> Result<bool> {
        let Self {
            id,
            name,
            geometry,
            reuse,
            reads,
            ..
        } = self;
        reuse.matches(
            Instant::now(),
            recent,
            || {
                reads.count_geometry();
                read_geometry(id, name, geometry)
            },
            |(read_as, read)| read.matches(*read_as, name, expected),
        )
    }
}

/// The geometry of display `id` now, which `shared` then holds. A
/// reconnection may give the display another id, which `id` takes; its
/// UUID, `name`, stays.
fn read_geometry(
    id: &mut CGDirectDisplayID,
    name: &str,
    shared: &Shared,
) -> Result<(CGDirectDisplayID, Geometry)> {
    let read = match (*id != 0).then(|| geometry(*id)).flatten() {
        Some(read) => read,
        None => {
            let found = (!name.is_empty())
                .then(|| by_name(name))
                .flatten()
                .ok_or_else(|| display_lost("selected display is no longer available"))?;
            *id = found;
            geometry(found).ok_or_else(|| display_lost("display is unavailable"))?
        }
    };
    shared.set(read);
    Ok((*id, read))
}

impl Desktop for MacDesktop {
    type Scope = ();
    fn scope(&self) {}
    fn prepare(&mut self, on_switch: impl FnOnce()) -> Result<()> {
        self.session(false, on_switch)
    }
    fn prepare_recent(&mut self, on_switch: impl FnOnce()) -> Result<()> {
        self.session(true, on_switch)
    }
    fn display(&mut self) -> Result<Display> {
        let Self {
            id,
            name,
            geometry,
            reuse,
            reads,
            ..
        } = self;
        let (resolved_id, read) = reuse
            .geometry(Instant::now(), || {
                reads.count_geometry();
                read_geometry(id, name, geometry)
            })
            .inspect_err(|_| {
                // `PlatformBackend::display` ran `prepare` just before: this
                // login owns the console. Keep lighting a display that takes
                // longer than `WAKE_WAIT` to link-train.
                super::power::wake_any();
            })?;
        if name.is_empty()
            && let Some(resolved_name) = uuid(resolved_id)
        {
            *name = resolved_name;
        }
        Ok(read.display(resolved_id, name.clone()))
    }
    fn display_matches(&mut self, expected: &Display) -> Result<bool> {
        self.matches(expected, false)
    }
    fn display_matches_recent(&mut self, expected: &Display) -> Result<bool> {
        self.matches(expected, true)
    }
    fn forget_recent(&mut self) {
        self.reuse.forget();
    }
    fn generation(&self) -> u64 {
        self.generation
    }
    fn kind(&self) -> DesktopKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure_kind;

    /// Opening a display that is currently asleep or unlisted succeeds so the
    /// session can stay alive, report `DisplayLost` (or `SecureDesktop` from
    /// `prepare`), and rebind by UUID once the monitor wakes.
    #[test]
    fn an_unlisted_display_opens_lazily_and_rebinds_by_uuid_when_active() {
        let missing = Display {
            id: u32::MAX - 1,
            name: "00000000-0000-0000-0000-000000000000".into(),
            x: 0,
            y: 0,
            width: 3840,
            height: 2160,
        };
        let mut desktop = MacDesktop::open(&missing).unwrap();
        let err = read_geometry(&mut desktop.id, &desktop.name, &desktop.geometry).unwrap_err();
        assert_eq!(failure_kind(&err), Some(FailureKind::DisplayLost));

        if let Ok(primary) = resolve(&DisplaySelection::Primary)
            && !primary.name.is_empty()
        {
            let sleeping = Display {
                id: u32::MAX - 2,
                name: primary.name.clone(),
                x: 0,
                y: 0,
                width: primary.width,
                height: primary.height,
            };
            let mut rebound = MacDesktop::open(&sleeping).unwrap();
            let (id, read) =
                read_geometry(&mut rebound.id, &rebound.name, &rebound.geometry).unwrap();
            assert_eq!(id, primary.id);
            assert!(read.matches(id, &rebound.name, &primary));
        }
    }
}
