//! Injection on macOS: `CGEvent`s posted to the HID event tap. The pointer
//! goes to the global point of a pixel of the bound display; a press carries
//! its click count and the modifiers held, which posted events otherwise
//! lack; a move with a button down is a drag; keys are virtual key codes and
//! text is a Unicode string per event. No main thread and no keyboard layout
//! are needed. macOS posts nothing without the owner's Accessibility grant,
//! and says nothing either, so the grant is checked first.

use super::{
    clicks::{Clicks, SLOP},
    desktop::Shared,
    keys::{self, Run},
};
use crate::{
    BackendError, Button, DoubleClickGuard, FailureKind, Key, Permission, Result, error,
    platform::Injector,
};
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
    /// The modifier flags of the modifier keys held down.
    flags: u64,
    /// Left, right, middle: down.
    buttons: [bool; 3],
    /// Where this injector last put the pointer, in global points.
    position: Option<(f64, f64)>,
    clicks: Clicks,
    /// The last Accessibility answer, and when it was asked.
    access: Option<(Instant, bool)>,
}

impl MacInput {
    pub(crate) fn new(geometry: Shared) -> Result<Self> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or_else(|| error("macOS refused an input event source"))?;
        Ok(Self {
            geometry,
            source,
            flags: 0,
            buttons: [false; 3],
            position: None,
            clicks: Clicks::default(),
            access: None,
        })
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

    fn mouse(&self, kind: CGEventType, button: CGMouseButton, clicks: Option<i64>) -> Result<()> {
        let (x, y) = self.position();
        let event = CGEvent::new_mouse_event(Some(&self.source), kind, CGPoint { x, y }, button);
        if let Some(event) = &event {
            CGEvent::set_flags(Some(event), CGEventFlags(self.flags));
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

    fn keyboard(&self, code: u16, down: bool, flags: u64, modifier: bool) -> Result<()> {
        let event = CGEvent::new_keyboard_event(Some(&self.source), code, down);
        if let Some(event) = &event {
            if modifier {
                CGEvent::set_type(Some(event), CGEventType::FlagsChanged);
            }
            CGEvent::set_flags(Some(event), CGEventFlags(flags));
        }
        self.post(event)
    }

    /// One keyboard event of each direction carrying `units`.
    fn unicode(&self, units: &[u16]) -> Result<()> {
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(Some(&self.source), 0, down);
            if let Some(event) = &event {
                CGEvent::set_flags(Some(event), CGEventFlags(0));
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
    fn pointer(&mut self, x: i32, y: i32) -> Result<()> {
        self.allowed()?;
        let (x, y) = self.geometry.get().point((x, y));
        self.position = Some((x, y));
        // With a button down, a move is that button's drag.
        let (kind, button) = [Button::Left, Button::Right, Button::Middle]
            .into_iter()
            .find(|button| self.buttons[index(*button)])
            .map_or((CGEventType::MouseMoved, CGMouseButton::Left), |button| {
                let (_, _, dragged, button) = kinds(button);
                (dragged, button)
            });
        self.mouse(kind, button, None)
    }
    fn button(&mut self, button: Button, down: bool) -> Result<()> {
        self.allowed()?;
        let (pressed, released, _, cg) = kinds(button);
        let count = if down {
            let interval = Duration::try_from_secs_f64(NSEvent::doubleClickInterval())
                .unwrap_or(DEFAULT_DOUBLE_CLICK);
            self.clicks
                .press(button, Instant::now(), self.position(), interval)
        } else {
            self.clicks.release(button)
        };
        self.mouse(if down { pressed } else { released }, cg, Some(count))?;
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
            CGEvent::set_flags(Some(event), CGEventFlags(self.flags));
        }
        self.post(event)
    }
    fn key(&mut self, key: Key, down: bool) -> Result<()> {
        self.allowed()?;
        let code = keys::key_code(key);
        match keys::modifier(code) {
            Some(flag) => {
                let flags = if down {
                    self.flags | flag
                } else {
                    self.flags & !flag
                };
                self.keyboard(code, down, flags, true)?;
                self.flags = flags;
                Ok(())
            }
            None => self.keyboard(code, down, self.flags, false),
        }
    }
    /// Text is typed with no modifier, whatever is held: a held Command
    /// must not turn it into shortcuts.
    fn text(&mut self, text: &str) -> Result<()> {
        self.allowed()?;
        for run in keys::runs(text) {
            match run {
                Run::Text(units) => self.unicode(&units)?,
                Run::Key(code) => {
                    self.keyboard(code, true, 0, false)?;
                    self.keyboard(code, false, 0, false)?;
                }
            }
        }
        Ok(())
    }
    fn lock(&mut self) -> Result<()> {
        Err(BackendError::new(FailureKind::Unsupported, "lock the desktop").into())
    }
    /// The owner's double-click interval; the slop in pixels at this
    /// display's scale.
    fn double_click(&mut self) -> Option<DoubleClickGuard> {
        let interval = NSEvent::doubleClickInterval().max(0.0);
        let half = (SLOP * self.geometry.get().scale).round() as u32;
        Some(DoubleClickGuard {
            time_ms: (interval * 1000.0).round() as u32,
            half_box: (half, half),
        })
    }
}
