//! The release obligation of injected input: every key and button pressed
//! and not yet seen released is released when the session asks, whatever the
//! platform injects with.

use crate::{Button, Key, Result};
use std::collections::HashSet;

/// A key or button held down by this backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pressed {
    Button(Button),
    Key(Key),
    /// A physical key, by its USB HID usage (`Input::PhysicalKey`).
    Physical(u16),
}

#[derive(Default)]
pub(crate) struct Held {
    buttons: HashSet<Button>,
    keys: HashSet<Key>,
    physical: HashSet<u16>,
}
impl Held {
    /// Press or release `button` through `inject`, which gets `down`.
    pub(crate) fn button(
        &mut self,
        button: Button,
        down: bool,
        inject: impl FnOnce(bool) -> Result<()>,
    ) -> Result<()> {
        apply(&mut self.buttons, button, down, inject)
    }

    /// Press or release `key` through `inject`, which gets `down`.
    pub(crate) fn key(
        &mut self,
        key: Key,
        down: bool,
        inject: impl FnOnce(bool) -> Result<()>,
    ) -> Result<()> {
        apply(&mut self.keys, key, down, inject)
    }

    /// Press or release the physical key `usage` through `inject`, which
    /// gets `down`.
    pub(crate) fn physical(
        &mut self,
        usage: u16,
        down: bool,
        inject: impl FnOnce(bool) -> Result<()>,
    ) -> Result<()> {
        apply(&mut self.physical, usage, down, inject)
    }

    /// Release everything held through `release`; whatever fails to release
    /// stays held, for the next release.
    pub(crate) fn release(&mut self, mut release: impl FnMut(Pressed) -> Result<()>) {
        self.buttons
            .retain(|button| release(Pressed::Button(*button)).is_err());
        self.keys.retain(|key| release(Pressed::Key(*key)).is_err());
        self.physical
            .retain(|usage| release(Pressed::Physical(*usage)).is_err());
    }
}

fn apply<T: Copy + Eq + std::hash::Hash>(
    held: &mut HashSet<T>,
    value: T,
    down: bool,
    inject: impl FnOnce(bool) -> Result<()>,
) -> Result<()> {
    // A failed native call can have partially applied input. Preserve the
    // release obligation until a subsequent key/button-up succeeds.
    if down {
        held.insert(value);
    }
    inject(down)?;
    if !down {
        held.remove(&value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error;

    #[test]
    fn failed_native_input_retains_the_release_obligation() {
        let mut held = Held::default();
        let key = Key::A;
        assert!(
            held.key(key, true, |_| Err(error("partial press")))
                .is_err()
        );
        assert!(held.keys.contains(&key));
        assert!(
            held.key(key, false, |_| Err(error("blocked release")))
                .is_err()
        );
        assert!(held.keys.contains(&key));
        assert!(held.key(key, false, |_| Ok(())).is_ok());
        assert!(held.keys.is_empty());
        // A release that fails keeps the obligation; one that succeeds ends it.
        assert!(held.button(Button::Left, true, |_| Ok(())).is_ok());
        held.release(|_| Err(error("blocked release")));
        assert!(held.buttons.contains(&Button::Left));
        held.release(|pressed| {
            assert_eq!(pressed, Pressed::Button(Button::Left));
            Ok(())
        });
        assert!(held.buttons.is_empty());
    }

    /// A physical key held down is released like any other; one let go of
    /// is not released again.
    #[test]
    fn held_physical_keys_join_the_release_obligation() {
        let mut held = Held::default();
        let (shift, a) = (0xE1, 0x04);
        assert!(held.physical(shift, true, |_| Ok(())).is_ok());
        assert!(held.physical(a, true, |_| Ok(())).is_ok());
        assert!(held.physical(a, false, |_| Ok(())).is_ok());
        let mut released = Vec::new();
        held.release(|pressed| {
            released.push(pressed);
            Ok(())
        });
        assert_eq!(released, [Pressed::Physical(shift)]);
        assert!(held.physical.is_empty());
    }
}
