//! A host display's modes: what a viewer may ask the host to switch its
//! display to, and back. The host changes a mode only dynamically, so the
//! operating system never keeps it, and restores the original when the
//! session ends.

use serde::{Deserialize, Serialize};

/// The most modes a list carries.
const MAX_MODES: usize = 64;
/// The largest width or height of a mode.
const MAX_SIDE: u32 = 32_768;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mode_list_is_valid_only_within_its_bounds() {
        let mode = |width, height| Mode { width, height };
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
}
