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
}

#[derive(Default)]
pub(crate) struct Held {
    buttons: HashSet<Button>,
    keys: HashSet<Key>,
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

    /// Release everything held through `release`; whatever fails to release
    /// stays held, for the next release.
    pub(crate) fn release(&mut self, mut release: impl FnMut(Pressed) -> Result<()>) {
        self.buttons
            .retain(|button| release(Pressed::Button(*button)).is_err());
        self.keys.retain(|key| release(Pressed::Key(*key)).is_err());
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
}
