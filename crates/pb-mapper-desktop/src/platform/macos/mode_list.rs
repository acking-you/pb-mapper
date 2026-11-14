//! Which of a Mac display's modes a viewer is offered. macOS lists one
//! pixel size several times: a HiDPI mode (two pixels a point) and its
//! low-resolution twin (one), each at several refresh rates. A viewer picks
//! by pixel size, so each size is offered once, at the scale the owner uses
//! now where the display has it, else at the lower scale (more desktop for
//! the same picture); the scale is kept, so the owner's own mode is found
//! again exactly.

use crate::modes::{Mode, Setting};

/// No mode is offered below this.
const MIN_SIZE: (u32, u32) = (800, 600);
/// The most modes offered.
const MAX_MODES: usize = 64;

/// One mode as `CGDisplayCopyAllDisplayModes` lists it, turned like the
/// display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Raw {
    pub pixels: (u32, u32),
    pub points: (u32, u32),
    /// Rounded; 0 for a panel that reports none.
    pub hz: u32,
    /// For the desktop, not only a game or a mirror.
    pub usable: bool,
}

impl Raw {
    /// Pixels per point: 1 or 2.
    pub(crate) fn scale(&self) -> u32 {
        match self.points.0 {
            0 => 1,
            points => (self.pixels.0 / points).max(1),
        }
    }
    pub(crate) fn size(&self) -> Mode {
        Mode {
            width: self.pixels.0,
            height: self.pixels.1,
        }
    }
    pub(crate) fn setting(&self) -> Setting {
        Setting {
            width: self.pixels.0,
            height: self.pixels.1,
            hz: self.hz,
            bpp: 0,
            scale: self.scale(),
        }
    }
    /// The mode `setting` was made from.
    pub(crate) fn is(&self, setting: &Setting) -> bool {
        self.pixels == (setting.width, setting.height)
            && self.hz == setting.hz
            && self.scale() == setting.scale
    }
    /// Whether this mode, of the same pixel size, suits a display showing
    /// `current` better than `other`: its scale, else the lower scale; its
    /// refresh rate, else the faster.
    fn better(&self, other: &Self, current: &Self) -> bool {
        let rank = |mode: &Self| {
            (
                mode.scale() == current.scale(),
                std::cmp::Reverse(mode.scale()),
                mode.hz == current.hz,
                speed(mode.hz),
            )
        };
        rank(self) > rank(other)
    }
}

/// How fast a rate is, to choose between them: 0, which a display lists
/// beside fixed rates for its variable mode (ProMotion, up to 120 Hz), is
/// the fastest.
fn speed(hz: u32) -> u32 {
    if hz == 0 { u32::MAX } else { hz }
}

/// What a viewer is offered of `all`, a display showing `current`: usable
/// modes of at least 800x600 pixels, one per pixel size, largest first, at
/// most 64.
pub(crate) fn offered(all: &[Raw], current: &Raw) -> Vec<Raw> {
    let mut kept: Vec<Raw> = Vec::new();
    let usable = all
        .iter()
        .filter(|mode| mode.usable && mode.pixels.0 >= MIN_SIZE.0 && mode.pixels.1 >= MIN_SIZE.1);
    for mode in usable {
        match kept.iter_mut().find(|kept| kept.pixels == mode.pixels) {
            Some(kept) if mode.better(kept, current) => *kept = *mode,
            Some(_) => {}
            None => kept.push(*mode),
        }
    }
    let area = |mode: &Raw| u64::from(mode.pixels.0) * u64::from(mode.pixels.1);
    kept.sort_by(|a, b| area(b).cmp(&area(a)).then(b.pixels.0.cmp(&a.pixels.0)));
    kept.truncate(MAX_MODES);
    kept
}

/// The usable listed mode `setting` names, at its rate where the display
/// still has it, else at the fastest. A mode not for the desktop (stretched,
/// interlaced, for mirroring) is never set, even where it is listed first
/// at the same size, scale and rate.
pub(crate) fn find(all: &[Raw], setting: &Setting) -> Option<Raw> {
    let usable = || all.iter().filter(|mode| mode.usable);
    usable()
        .find(|mode| mode.is(setting))
        .or_else(|| {
            usable()
                .filter(|mode| {
                    mode.pixels == (setting.width, setting.height) && mode.scale() == setting.scale
                })
                .max_by_key(|mode| speed(mode.hz))
        })
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pixels: (u32, u32), scale: u32, hz: u32) -> Raw {
        Raw {
            pixels,
            points: (pixels.0 / scale, pixels.1 / scale),
            hz,
            usable: true,
        }
    }

    /// A Retina panel at "looks like 1512x982" (2x): its HiDPI sizes stay
    /// HiDPI, a size only its 1x twin has is that twin, the current rate is
    /// kept where offered, small and unusable modes are left out, and the
    /// owner's own mode is found again exactly.
    #[test]
    fn each_pixel_size_is_offered_once_at_the_owners_scale_first() {
        let current = raw((3024, 1964), 2, 120);
        let all = [
            raw((3024, 1964), 2, 120),
            raw((3024, 1964), 2, 60),
            raw((3024, 1964), 1, 120),
            raw((2048, 1330), 2, 120),
            raw((1920, 1200), 1, 60),
            raw((1920, 1200), 1, 120),
            raw((640, 480), 1, 60),
            Raw {
                usable: false,
                ..raw((4096, 2160), 1, 60)
            },
        ];
        let offered = offered(&all, &current);
        assert_eq!(
            offered,
            [
                raw((3024, 1964), 2, 120),
                raw((2048, 1330), 2, 120),
                raw((1920, 1200), 1, 120),
            ]
        );
        let own = current.setting();
        assert_eq!((own.scale, own.bpp), (2, 0));
        assert_eq!(find(&all, &own), Some(current));
        // The owner's rate is gone: the same size and scale at another.
        let gone = Setting { hz: 75, ..own };
        assert_eq!(find(&all, &gone), Some(current));
        assert_eq!(find(&all, &Setting { scale: 3, ..own }), None);
        // An unusable twin listed first is passed over; a display's variable
        // rate (0) is its fastest.
        let unusable = Raw {
            usable: false,
            ..current
        };
        assert_eq!(find(&[unusable, current], &own), Some(current));
        let variable = raw((3024, 1964), 2, 0);
        assert_eq!(find(&[current, variable], &gone), Some(variable));
        let many: Vec<Raw> = (0..100).map(|i| raw((800 + 2 * i, 600), 1, 60)).collect();
        assert_eq!(super::offered(&many, &current).len(), MAX_MODES);
    }
}
