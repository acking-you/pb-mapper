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
    layout, pointer,
};
use crate::{
    BackendError, Button, DoubleClickGuard, FailureKind, Key, Permission, Result, error,
    input::{
        SCROLL_UNITS_PER_NOTCH, ScrollCarry,
        held::{NativeKeys, Pressed},
    },
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
    /// Each move posted, for the cursor to report until the window server
    /// shows it.
    pointer: pointer::Shared,
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
    keys: NativeKeys,
    /// What scrolls left short of a whole point or line.
    scrolls: ScrollSteps,
}

/// `Input::Scroll` units per point of continuous travel: a notch, 120
/// units, is 100 points, as the viewer measures a trackpad's travel.
const UNITS_PER_POINT: (i64, i64) = (6, 5);

/// What a Mac posts of `Input::Scroll`s: whole points of continuous travel,
/// 1.2 units a point, or whole lines of a wheel's, a notch a line as
/// `Input::Wheel` posts them; what falls short of one is kept for the next.
#[derive(Default)]
struct ScrollSteps {
    points: ScrollCarry,
    lines: ScrollCarry,
}
impl ScrollSteps {
    /// The unit and the wheel deltas to post for a scroll of these units,
    /// in Core Graphics' directions (wheel 1 positive up, wheel 2 positive
    /// left), or `None` while it completes no whole step.
    fn take(
        &mut self,
        horizontal: i32,
        vertical: i32,
        continuous: bool,
    ) -> Option<(CGScrollEventUnit, i32, i32)> {
        let units = (horizontal, vertical);
        let (unit, (x, y)) = if continuous {
            let (per, worth) = UNITS_PER_POINT;
            (
                CGScrollEventUnit::Pixel,
                self.points.take(units, worth, per),
            )
        } else {
            let per = SCROLL_UNITS_PER_NOTCH.into();
            (CGScrollEventUnit::Line, self.lines.take(units, 1, per))
        };
        ((x, y) != (0, 0)).then(|| (unit, y.saturating_neg(), x.saturating_neg()))
    }
}

impl MacInput {
    pub(crate) fn new(geometry: Shared, pointer: pointer::Shared) -> Result<Self> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or_else(|| error("macOS refused an input event source"))?;
        layout::request(layout::FIRST_READ);
        Ok(Self {
            geometry,
            pointer,
            source,
            modifiers: Modifiers::default(),
            buttons: [false; 3],
            position: None,
            clicks: Clicks::default(),
            access: None,
            layout: (layout::generation(), layout::current()),
            keys: NativeKeys::default(),
            scrolls: ScrollSteps::default(),
        })
    }

    fn send_key(&mut self, source: Pressed, down: bool) -> Result<()> {
        self.allowed()?;
        let routed = self.keys.route(source, down, || match source {
            Pressed::Physical(usage) => keys::usage_code(usage),
            Pressed::Key(key) => {
                let generation = layout::generation();
                if generation != self.layout.0 {
                    self.layout = (generation, layout::current());
                }
                Some(self.layout.1.code(key))
            }
            Pressed::Button(_) => None,
        });
        if let Some((_, code)) = routed {
            self.press(code, down)?;
            if !down {
                self.keys.released(source);
            }
        }
        Ok(())
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

    /// What a move is: with a button down, that button's drag.
    fn motion(&self) -> (CGEventType, CGMouseButton) {
        [Button::Left, Button::Right, Button::Middle]
            .into_iter()
            .find(|button| self.buttons[index(*button)])
            .map_or((CGEventType::MouseMoved, CGMouseButton::Left), |button| {
                let (_, _, dragged, button) = kinds(button);
                (dragged, button)
            })
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
            self.flag(event, self.modifiers.flags(), 0);
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
            self.flag(event, flags, keys::kept_key_flags(code, down));
        }
        self.post(event)
    }

    /// Give `event` the modifier flags `held`. `keep` is what of Fn, the
    /// numeric pad and Help this key's direction owns; the rest is dropped,
    /// so a latched Fn does not ride onto the next key.
    fn flag(&self, event: &CGEvent, held: u64, keep: u64) {
        let made = CGEvent::flags(Some(event)).0;
        CGEvent::set_flags(
            Some(event),
            CGEventFlags(keys::with_modifiers(made, held, keep)),
        );
    }

    /// One keyboard event of each direction carrying `units`, on the Space
    /// key ([`keys::UNICODE_KEY`]) and with no modifier and no Fn, so no
    /// shortcut fires: the A key under a latched Fn was Fn-A, which focuses
    /// the Dock on Finder instead of typing.
    fn unicode(&self, units: &[u16]) -> Result<()> {
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(Some(&self.source), keys::UNICODE_KEY, down);
            if let Some(event) = &event {
                // The flags Core Graphics gives every keyboard event stay,
                // as on every other key posted here.
                self.flag(event, 0, 0);
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
    const HIRES_WHEEL: bool = true;
    const RELATIVE_POINTER: bool = true;
    /// The cursor reports `(x, y)` until the window server has applied the
    /// move (`pointer.rs`).
    fn pointer(&mut self, x: i32, y: i32) -> Result<()> {
        self.allowed()?;
        let pixel = (x, y);
        let (x, y) = self.geometry.get().point(pixel);
        let (from_x, from_y) = self.position();
        let moved = ((x - from_x).round() as i64, (y - from_y).round() as i64);
        self.position = Some((x, y));
        let (kind, button) = self.motion();
        self.mouse(kind, button, Some(moved), None)?;
        let mut pointer = self.pointer.get();
        pointer.moved(pixel, Instant::now());
        self.pointer.set(pointer);
        Ok(())
    }
    /// From where the system has the pointer now, not where this injector
    /// last put it (an app may have put it back, as a game recentres it):
    /// the counts as the move's deltas, which an app that reads the mouse
    /// takes as they are, and the pointer that many pixels on, a drag with a
    /// button down. The window server keeps it on a screen, so later moves
    /// and clicks start from where it is again.
    fn pointer_by(&mut self, dx: i32, dy: i32) -> Result<()> {
        self.allowed()?;
        self.position = None;
        let (x, y) = self.position();
        let scale = self.geometry.get().scale;
        self.position = Some((x + f64::from(dx) / scale, y + f64::from(dy) / scale));
        let (kind, button) = self.motion();
        let posted = self.mouse(kind, button, Some((dx.into(), dy.into())), None);
        self.position = None;
        let mut pointer = self.pointer.get();
        pointer.moved_by();
        self.pointer.set(pointer);
        posted
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
            self.flag(event, self.modifiers.flags(), 0);
        }
        self.post(event)
    }
    /// Continuous travel as a pixel scroll in points, marked continuous, as
    /// a trackpad's: apps scroll by exactly that much. A wheel's notches as
    /// lines, as `wheel` posts them, parts of one kept until it is whole.
    fn scroll(&mut self, horizontal: i32, vertical: i32, continuous: bool) -> Result<()> {
        self.allowed()?;
        let Some((unit, wheel1, wheel2)) = self.scrolls.take(horizontal, vertical, continuous)
        else {
            return Ok(());
        };
        let event =
            CGEvent::new_scroll_wheel_event2(Some(&self.source), unit, 2, wheel1, wheel2, 0);
        if let Some(event) = &event {
            self.flag(event, self.modifiers.flags(), 0);
            CGEvent::set_integer_value_field(
                Some(event),
                CGEventField::ScrollWheelEventIsContinuous,
                i64::from(continuous),
            );
        }
        self.post(event)
    }
    fn forget_scroll(&mut self) {
        self.scrolls = ScrollSteps::default();
    }
    /// `Meta` is Command and `Control` Control: the viewer's Apple swap
    /// relies on it.
    fn key(&mut self, key: Key, down: bool) -> Result<()> {
        self.send_key(Pressed::Key(key), down)
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
        self.send_key(Pressed::Physical(usage), down)
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

#[cfg(test)]
mod scroll_tests {
    use super::{CGScrollEventUnit, ScrollSteps};

    /// A trackpad's travel, sent in uneven parts, posts a point for each
    /// 1.2 units, wheel 1 up for a scroll down and wheel 2 left for one
    /// right; a wheel's notch posts a line, half notches a line every
    /// second; what one kind left over waits for that kind alone.
    #[test]
    fn a_mac_posts_points_of_travel_and_lines_of_notches() {
        let mut steps = ScrollSteps::default();
        let (mut up, mut left) = (0, 0);
        // 360 units down and 180 right: 300 points and 150.
        for units in [2, 4, 6, 8, 10, 6].repeat(10) {
            if let Some((unit, wheel1, wheel2)) = steps.take(units / 2, units, true) {
                assert_eq!(unit, CGScrollEventUnit::Pixel);
                up += wheel1;
                left += wheel2;
            }
        }
        assert_eq!((up, left), (-300, -150));
        let line = |wheel1, wheel2| Some((CGScrollEventUnit::Line, wheel1, wheel2));
        assert_eq!(steps.take(0, 120, false), line(-1, 0));
        assert_eq!(steps.take(0, -60, false), None);
        assert_eq!(steps.take(0, -60, false), line(1, 0));
        assert_eq!(steps.take(-240, 0, false), line(0, 2));
        assert_eq!(steps.take(0, 1, true), None);
        assert_eq!(steps.take(0, 60, false), None);
        assert_eq!(
            steps.take(0, 1, true),
            Some((CGScrollEventUnit::Pixel, -1, 0))
        );
    }
}
