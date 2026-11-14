//! Injection on macOS: `CGEvent`s posted to the HID event tap. The pointer
//! goes to the global point of a pixel of the bound display; a press carries
//! its click count and the modifiers held, which posted events otherwise
//! lack; a move with a button down is a drag; keys are virtual key codes: a
//! named letter or digit the key that types it in the Mac's layout, read on
//! the main thread (`layout.rs`), and goes up where it went down; every
//! other key, and a viewer's physical keys, by place, so the Mac's own
//! layout and input source decide what they type; text is a Unicode string
//! per event. macOS posts nothing without the owner's Accessibility grant,
//! and says nothing either, so the grant is checked first.

use super::{
    clicks::{Clicks, SLOP},
    desktop::Shared,
    keys::{self, Layout, Modifiers},
    layout,
};
use crate::{
    BackendError, Button, DoubleClickGuard, FailureKind, Key, Permission, Result, error,
    platform::Injector,
};
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSEvent;
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventType, CGMouseButton, CGPreflightPostEventAccess, CGScrollEventUnit,
};
use std::time::{Duration, Instant};

/// How long a granted or refused Accessibility answer is trusted.
const ACCESS_CHECK: Duration = Duration::from_secs(1);
/// macOS's own default, for an interval the system reports out of range.
const DEFAULT_DOUBLE_CLICK: Duration = Duration::from_millis(500);

pub(crate) struct MacInput {
    geometry: Shared,
    source: CFRetained<CGEventSource>,
    /// The modifier keys held down, and Caps Lock.
    modifiers: Modifiers,
    /// Left, right, middle: down.
    buttons: [bool; 3],
    /// Where this injector last put the pointer, in global points.
    position: Option<(f64, f64)>,
    clicks: Clicks,
    /// The last Accessibility answer, and when it was asked.
    access: Option<(Instant, bool)>,
    /// The layout named keys go through, and the read it came from.
    layout: (u64, Layout),
    /// Named keys down, each with the virtual key it went down at.
    named: Vec<(Key, u16)>,
}

impl MacInput {
    pub(crate) fn new(geometry: Shared) -> Result<Self> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or_else(|| error("macOS refused an input event source"))?;
        layout::request(layout::FIRST_READ);
        Ok(Self {
            geometry,
            source,
            modifiers: Modifiers::default(),
            buttons: [false; 3],
            position: None,
            clicks: Clicks::default(),
            access: None,
            layout: (layout::generation(), layout::current()),
            named: Vec::new(),
        })
    }

    /// The virtual key `key` goes `down` or up at: where it went down, if
    /// it is down, whatever the layout did meanwhile; else through the
    /// layout read last.
    fn code(&mut self, key: Key, down: bool) -> u16 {
        if let Some(at) = self.named.iter().position(|(held, _)| *held == key) {
            let code = self.named[at].1;
            if !down {
                self.named.swap_remove(at);
            }
            return code;
        }
        let generation = layout::generation();
        if generation != self.layout.0 {
            self.layout = (generation, layout::current());
        }
        let code = self.layout.1.code(key);
        if down && keys::named(key) {
            self.named.push((key, code));
        }
        code
    }

    /// Fails while the owner has not granted Accessibility.
    fn allowed(&mut self) -> Result<()> {
        let now = Instant::now();
        let granted = match self.access {
            Some((asked, granted)) if now.saturating_duration_since(asked) < ACCESS_CHECK => {
                granted
            }
            _ => {
                let granted = CGPreflightPostEventAccess();
                self.access = Some((now, granted));
                granted
            }
        };
        if granted {
            Ok(())
        } else {
            Err(BackendError::permission_missing(
                Permission::Input,
                "macOS has not granted pb-mapper Accessibility, which remote control needs: allow it in System Settings › Privacy & Security › Accessibility",
            )
            .into())
        }
    }

    fn post(&self, event: Option<CFRetained<CGEvent>>) -> Result<()> {
        let event = event.ok_or_else(|| error("macOS refused to create an input event"))?;
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        Ok(())
    }

    /// Where the pointer is: where this injector put it, else where the
    /// system has it.
    fn position(&self) -> (f64, f64) {
        self.position.unwrap_or_else(|| {
            let here = CGEvent::new(None);
            let point = CGEvent::location(here.as_deref());
            (point.x, point.y)
        })
    }

    /// A mouse event at the pointer: `moved` by how many points, a move
    /// or a drag, which apps and the window server read as the gesture;
    /// `clicks`, a press or release.
    fn mouse(
        &self,
        kind: CGEventType,
        button: CGMouseButton,
        moved: Option<(i64, i64)>,
        clicks: Option<i64>,
    ) -> Result<()> {
        let (x, y) = self.position();
        let event = CGEvent::new_mouse_event(Some(&self.source), kind, CGPoint { x, y }, button);
        if let Some(event) = &event {
            self.flag(event, self.modifiers.flags());
            if let Some((dx, dy)) = moved {
                CGEvent::set_integer_value_field(Some(event), CGEventField::MouseEventDeltaX, dx);
                CGEvent::set_integer_value_field(Some(event), CGEventField::MouseEventDeltaY, dy);
            }
            if let Some(count) = clicks {
                CGEvent::set_integer_value_field(
                    Some(event),
                    CGEventField::MouseEventClickState,
                    count,
                );
            }
            if button == CGMouseButton::Center {
                CGEvent::set_integer_value_field(
                    Some(event),
                    CGEventField::MouseEventButtonNumber,
                    2,
                );
            }
        }
        self.post(event)
    }

    /// Press or release the key `code`. A modifier posts as a flags change
    /// carrying the flags with it; any other key carries the flags held, so
    /// Cmd+C copies.
    fn press(&mut self, code: u16, down: bool) -> Result<()> {
        let mut modifiers = self.modifiers;
        if modifiers.update(code, down) {
            self.keyboard(code, down, modifiers.flags(), true)?;
            self.modifiers = modifiers;
            Ok(())
        } else {
            self.keyboard(code, down, self.modifiers.flags(), false)
        }
    }

    fn keyboard(&self, code: u16, down: bool, flags: u64, modifier: bool) -> Result<()> {
        let event = CGEvent::new_keyboard_event(Some(&self.source), code, down);
        if let Some(event) = &event {
            if modifier {
                CGEvent::set_type(Some(event), CGEventType::FlagsChanged);
            }
            self.flag(event, flags);
        }
        self.post(event)
    }

    /// Give `event` the modifier flags `held`, keeping those it carries of
    /// its own.
    fn flag(&self, event: &CGEvent, held: u64) {
        let made = CGEvent::flags(Some(event)).0;
        CGEvent::set_flags(Some(event), CGEventFlags(keys::with_modifiers(made, held)));
    }

    /// One keyboard event of each direction carrying `units`.
    fn unicode(&self, units: &[u16]) -> Result<()> {
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(Some(&self.source), 0, down);
            if let Some(event) = &event {
                self.flag(event, 0);
                // SAFETY: `units` holds `units.len()` UTF-16 code units.
                unsafe {
                    CGEvent::keyboard_set_unicode_string(
                        Some(event),
                        units.len() as _,
                        units.as_ptr(),
                    );
                }
            }
            self.post(event)?;
        }
        Ok(())
    }
}

/// The event types of a button: down, up, dragged; and its Core Graphics
/// button.
fn kinds(button: Button) -> (CGEventType, CGEventType, CGEventType, CGMouseButton) {
    match button {
        Button::Left => (
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::LeftMouseDragged,
            CGMouseButton::Left,
        ),
        Button::Right => (
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGEventType::RightMouseDragged,
            CGMouseButton::Right,
        ),
        Button::Middle => (
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGEventType::OtherMouseDragged,
            CGMouseButton::Center,
        ),
    }
}

fn index(button: Button) -> usize {
    match button {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
    }
}

impl Injector for MacInput {
    /// macOS has no public call that locks the screen.
    const LOCK: bool = false;
    const PHYSICAL_KEYS: bool = true;
    fn pointer(&mut self, x: i32, y: i32) -> Result<()> {
        self.allowed()?;
        let (x, y) = self.geometry.get().point((x, y));
        let (from_x, from_y) = self.position();
        let moved = ((x - from_x).round() as i64, (y - from_y).round() as i64);
        self.position = Some((x, y));
        // With a button down, a move is that button's drag.
        let (kind, button) = [Button::Left, Button::Right, Button::Middle]
            .into_iter()
            .find(|button| self.buttons[index(*button)])
            .map_or((CGEventType::MouseMoved, CGMouseButton::Left), |button| {
                let (_, _, dragged, button) = kinds(button);
                (dragged, button)
            });
        self.mouse(kind, button, Some(moved), None)
    }
    fn button(&mut self, button: Button, down: bool) -> Result<()> {
        self.allowed()?;
        let (pressed, released, _, cg) = kinds(button);
        let count = if down {
            let interval = autoreleasepool(|_| {
                Duration::try_from_secs_f64(NSEvent::doubleClickInterval())
                    .unwrap_or(DEFAULT_DOUBLE_CLICK)
            });
            self.clicks
                .press(button, Instant::now(), self.position(), interval)
        } else {
            self.clicks.release(button)
        };
        self.mouse(if down { pressed } else { released }, cg, None, Some(count))?;
        self.buttons[index(button)] = down;
        Ok(())
    }
    /// Lines, as a wheel's notches; positive is down and right, the
    /// opposite of Core Graphics.
    fn wheel(&mut self, horizontal: i32, vertical: i32) -> Result<()> {
        self.allowed()?;
        let event = CGEvent::new_scroll_wheel_event2(
            Some(&self.source),
            CGScrollEventUnit::Line,
            2,
            -vertical,
            -horizontal,
            0,
        );
        if let Some(event) = &event {
            self.flag(event, self.modifiers.flags());
        }
        self.post(event)
    }
    /// `Meta` is Command and `Control` Control: the viewer's Apple swap
    /// relies on it.
    fn key(&mut self, key: Key, down: bool) -> Result<()> {
        self.allowed()?;
        let code = self.code(key, down);
        self.press(code, down)
    }
    /// Text is typed with no modifier, whatever is held: a held Command
    /// must not turn it into shortcuts. It holds no line break or tab.
    fn text(&mut self, text: &str) -> Result<()> {
        self.allowed()?;
        for units in keys::runs(text) {
            self.unicode(&units)?;
        }
        Ok(())
    }
    /// The key at that place on a Mac keyboard, by its virtual key code:
    /// the host's layout and input source decide what it types. A usage a
    /// Mac keyboard has no key for presses nothing.
    fn physical_key(&mut self, usage: u16, down: bool) -> Result<()> {
        let Some(code) = keys::usage_code(usage) else {
            return Ok(());
        };
        self.allowed()?;
        self.press(code, down)
    }
    fn lock(&mut self) -> Result<()> {
        Err(BackendError::new(FailureKind::Unsupported, "lock the desktop").into())
    }
    /// The owner's double-click interval; the slop in pixels at this
    /// display's scale.
    fn double_click(&mut self) -> Option<DoubleClickGuard> {
        let interval = autoreleasepool(|_| NSEvent::doubleClickInterval()).max(0.0);
        let half = (SLOP * self.geometry.get().scale).round() as u32;
        Some(DoubleClickGuard {
            time_ms: (interval * 1000.0).round() as u32,
            half_box: (half, half),
        })
    }
}
