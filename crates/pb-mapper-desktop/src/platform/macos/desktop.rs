//! The displays of this Mac and the one a session is bound to: geometry in
//! pixels from Core Graphics, each display named by its UUID, which outlives
//! a mode change or a reconnection that gives it another id; and the
//! session check: a locked screen, or a login switched away from the
//! console, is the secure desktop, which an ordinary process cannot reach.

use super::{cf, geometry::Geometry};
use crate::{BackendError, Display, DisplaySelection, FailureKind, Result, platform::Desktop};
use objc2_core_foundation::{CFBoolean, CFNumber, CFRetained, CFString, CFUUID};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode,
    CGGetActiveDisplayList, CGMainDisplayID, CGSessionCopyCurrentDictionary,
};
use std::{cell::Cell, ptr::NonNull, rc::Rc};

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

/// Whether this process's login owns the screen, unlocked: else the secure
/// desktop.
fn available() -> Result<()> {
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
        return Err(secure("the screen is locked"));
    }
    Ok(())
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
}

impl MacDesktop {
    /// `display`, by its id, or by its name when that id is gone.
    pub(crate) fn open(display: &Display) -> Result<Self> {
        let id = match geometry(display.id) {
            Some(_)
                if display.name.is_empty()
                    || uuid(display.id).as_deref() == Some(&display.name) =>
            {
                display.id
            }
            _ => by_name(&display.name)
                .ok_or_else(|| display_lost("selected display is no longer available"))?,
        };
        let geometry = geometry(id).ok_or_else(|| display_lost("display is unavailable"))?;
        Ok(Self {
            id,
            name: uuid(id).unwrap_or_else(|| display.name.clone()),
            geometry: Rc::new(Cell::new(geometry)),
        })
    }

    pub(crate) fn shared(&self) -> Shared {
        self.geometry.clone()
    }
}

impl Desktop for MacDesktop {
    type Scope = ();
    fn scope(&self) {}
    fn prepare(&mut self, on_switch: impl FnOnce()) -> Result<()> {
        available().inspect_err(|_| on_switch())
    }
    fn display(&mut self) -> Result<Display> {
        let geometry = match geometry(self.id) {
            Some(geometry) => geometry,
            None => {
                // A reconnection may give the display another id; its UUID
                // stays.
                let id = by_name(&self.name)
                    .filter(|id| *id != self.id)
                    .ok_or_else(|| display_lost("selected display is no longer available"))?;
                self.id = id;
                geometry(id).ok_or_else(|| display_lost("display is unavailable"))?
            }
        };
        self.geometry.set(geometry);
        Ok(geometry.display(self.id, self.name.clone()))
    }
}
