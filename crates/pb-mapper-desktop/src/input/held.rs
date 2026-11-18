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
impl Pressed {
    pub(crate) fn into_release_input(self) -> crate::Input {
        match self {
            Self::Button(button) => crate::Input::Button {
                button,
                down: false,
                drag_start: false,
            },
            Self::Key(key) => crate::Input::Key { key, down: false },
            Self::Physical(usage) => crate::Input::PhysicalKey { usage, down: false },
        }
    }
}

/// Collapse wire keys that press the same native key. Keep the first injection
/// method and native code until the last alias releases, including across a
/// keyboard layout change or a failed native key-up.
#[cfg(any(windows, target_os = "macos", test))]
#[derive(Default)]
pub(crate) struct NativeKeys(Vec<NativeHold>);

#[cfg(any(windows, target_os = "macos", test))]
#[derive(Clone, Copy)]
struct NativeHold {
    source: Pressed,
    posted: Pressed,
    code: u16,
}

#[cfg(any(windows, target_os = "macos", test))]
impl NativeKeys {
    pub(crate) fn route(
        &mut self,
        source: Pressed,
        down: bool,
        resolve: impl FnOnce() -> Option<u16>,
    ) -> Option<(Pressed, u16)> {
        let held = self.0.iter().find(|held| held.source == source).copied();
        let code = match held {
            Some(held) => held.code,
            None => resolve()?,
        };
        let other = self
            .0
            .iter()
            .find(|held| held.source != source && held.code == code);
        if let Some(other) = other {
            if !down {
                self.released(source);
                return None;
            }
            if held.is_none() {
                let posted = other.posted;
                self.0.push(NativeHold {
                    source,
                    posted,
                    code,
                });
                return None;
            }
        }
        let posted = held.map_or(source, |held| held.posted);
        if down && held.is_none() {
            self.0.push(NativeHold {
                source,
                posted,
                code,
            });
        }
        Some((posted, code))
    }

    pub(crate) fn released(&mut self, source: Pressed) {
        self.0.retain(|held| held.source != source);
    }
}

#[derive(Default)]
pub(crate) struct Held {
    buttons: HashSet<Button>,
    keys: HashSet<Key>,
    physical: HashSet<u16>,
}
impl Held {
    /// Text commits Enter or Tab as a key tap. Repeat a key already held
    /// by a controller without releasing its native hold.
    pub(crate) fn tap(
        &mut self,
        key: Key,
        mut inject: impl FnMut(bool) -> Result<()>,
    ) -> Result<()> {
        if self.keys.contains(&key) {
            return inject(true);
        }
        self.key(key, true, &mut inject)?;
        self.key(key, false, inject)
    }

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

    pub(crate) fn is_empty(&self) -> bool {
        self.buttons.is_empty() && self.keys.is_empty() && self.physical.is_empty()
    }

    pub(crate) fn contains(&self, pressed: Pressed) -> bool {
        match pressed {
            Pressed::Button(button) => self.buttons.contains(&button),
            Pressed::Key(key) => self.keys.contains(&key),
            Pressed::Physical(usage) => self.physical.contains(&usage),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buttons.clear();
        self.keys.clear();
        self.physical.clear();
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
    fn mixed_key_representations_release_the_original_native_key_only_once() {
        let mut keys = NativeKeys::default();
        let logical = Pressed::Key(Key::Control);
        let physical = Pressed::Physical(0xe0);
        let native = || Some(0x3b);
        assert_eq!(keys.route(logical, true, native), Some((logical, 0x3b)));
        assert_eq!(keys.route(physical, true, native), None);
        assert_eq!(keys.route(logical, false, || None), None);
        // The native release fails and is retried after a layout change.
        for _ in 0..2 {
            assert_eq!(
                keys.route(physical, false, || Some(1)),
                Some((logical, 0x3b))
            );
        }
        keys.released(physical);
        assert!(keys.0.is_empty());
    }

    #[test]
    fn typing_a_held_enter_preserves_its_release_obligation() {
        let mut held = Held::default();
        let mut posted = Vec::new();
        let mut inject = |down| {
            posted.push(down);
            Ok(())
        };
        held.key(Key::Enter, true, &mut inject).unwrap();
        held.tap(Key::Enter, &mut inject).unwrap();
        assert!(held.contains(Pressed::Key(Key::Enter)));
        held.key(Key::Enter, false, inject).unwrap();
        assert_eq!(posted, [true, true, false]);
    }

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
