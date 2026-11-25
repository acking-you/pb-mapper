//! Injection on macOS: `CGEvent`s posted to the HID event tap. The pointer
//! goes to the global point of a pixel of the bound display; a press carries
//! its click count and the modifiers held, which posted events otherwise
//! lack; a move with a button down is a drag; keys are virtual key codes: a
//! named letter or digit the key that types it in the Mac's layout, read on
//! the main thread (`layout.rs`), and goes up where it went down; every
//! other key, and a viewer's physical keys, by place, so the Mac's own
//! layout and input source decide what they type; text is a Unicode string
//! per event, or virtual key codes on the lock screen. macOS posts nothing
//! without the owner's Accessibility grant, and says nothing either, so the
//! grant is checked first.

use super::{
    clicks::{Clicks, SLOP},
    desktop::{self, Shared},
    keys::{self, Layout, Modifiers},
    layout, permissions, pointer, power,
    unlock::{self, PasswordCheck},
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
    CGDisplayIsAsleep, CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID,
    CGEventTapLocation, CGEventType, CGMainDisplayID, CGMouseButton, CGScrollEventUnit,
};
use std::time::{Duration, Instant};

/// How long a granted or refused Accessibility answer is trusted.
const ACCESS_CHECK: Duration = Duration::from_secs(1);
/// macOS's own default, for an interval the system reports out of range.
const DEFAULT_DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// Wait bounds when waking a sleeping display and submitting a password on `loginwindow`.
const UNLOCK_WAKE_WAIT: Duration = Duration::from_millis(800);
const UNLOCK_WAKE_POLL: Duration = Duration::from_millis(40);
const UNLOCK_FOCUS_SETTLE: Duration = Duration::from_millis(120);
const UNLOCK_KEY_PACE: Duration = Duration::from_millis(8);
const UNLOCK_VERIFY_WAIT: Duration = Duration::from_millis(1800);
const UNLOCK_VERIFY_POLL: Duration = Duration::from_millis(50);

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

    fn refresh_layout(&mut self) {
        let generation = layout::generation();
        if generation != self.layout.0 {
            self.layout = (generation, layout::current());
        }
    }

    fn send_key(&mut self, source: Pressed, down: bool) -> Result<()> {
        self.allowed()?;
        if matches!(source, Pressed::Key(_)) && down {
            self.refresh_layout();
        }
        let layout = self.layout.1;
        let routed = self.keys.route(source, down, || match source {
            Pressed::Physical(usage) => keys::usage_code(usage),
            Pressed::Key(key) => Some(layout.code(key)),
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
                let granted = permissions::input_granted();
                self.access = Some((now, granted));
                granted
            }
        };
        if granted {
            Ok(())
        } else {
            Err(BackendError::permission_missing(
                Permission::Input,
                "macOS has not granted pb-mapper Accessibility, which remote control and lock-screen unlock need: allow it in System Settings › Privacy & Security › Accessibility",
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

    /// Type one character onto `loginwindow`'s secure password field: printable
    /// ASCII uses real hardware virtual key codes (`kVK_*` + `SHIFT`), because
    /// `NSSecureTextField` ignores `UNICODE_KEY`'s Unicode override; non-ASCII
    /// characters fall back to Unicode insertion.
    fn type_lock_char(&self, ch: char) -> Result<()> {
        if let Some((code, shifted)) = self.layout.1.ascii_stroke(ch) {
            let shift = keys::key_code(Key::Shift);
            let flags = if shifted { keys::SHIFT | 0x02 } else { 0 };
            if shifted {
                self.keyboard(shift, true, flags, true)?;
            }
            let typed = self
                .keyboard(code, true, flags, false)
                .and_then(|()| self.keyboard(code, false, flags, false));
            if shifted {
                let _ = self.keyboard(shift, false, 0, true);
            }
            typed
        } else {
            let mut units = [0u16; 2];
            self.unicode(ch.encode_utf16(&mut units))
        }
    }

    /// Clear any stale characters in `loginwindow`'s password box (`Cmd+A`,
    /// then `Backspace`) while still on the lock screen.
    fn clear_lock_field(&self) -> Result<()> {
        if !desktop::is_locked() {
            return Ok(());
        }
        let cmd = keys::key_code(Key::Meta);
        let cmd_flags = keys::COMMAND | 0x08;
        let select_all = self.layout.1.code(Key::A);
        self.keyboard(cmd, true, cmd_flags, true)?;
        let selected = self
            .keyboard(select_all, true, cmd_flags, false)
            .and_then(|()| self.keyboard(select_all, false, cmd_flags, false));
        let _ = self.keyboard(cmd, false, 0, true);
        selected?;
        let backspace = keys::key_code(Key::Backspace);
        self.keyboard(backspace, true, 0, false)?;
        self.keyboard(backspace, false, 0, false)
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
    const UNLOCK: bool = true;
    const PHYSICAL_KEYS: bool = true;
    const HIRES_WHEEL: bool = true;
    /// The cursor reports `(x, y)` until the window server has applied the
    /// move (`pointer.rs`).
    fn pointer(&mut self, x: i32, y: i32) -> Result<()> {
        self.allowed()?;
        let pixel = (x, y);
        let (x, y) = self.geometry.get().point(pixel);
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
        self.mouse(kind, button, Some(moved), None)?;
        let mut pointer = self.pointer.get();
        pointer.moved(pixel, Instant::now());
        self.pointer.set(pointer);
        Ok(())
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
    /// must not turn it into shortcuts. It holds no line break or tab. On the
    /// lock screen, printable ASCII goes as virtual key codes so `loginwindow`
    /// accepts it into its password field.
    fn text(&mut self, text: &str) -> Result<()> {
        self.allowed()?;
        if desktop::is_locked() {
            self.refresh_layout();
            for ch in text.chars() {
                self.type_lock_char(ch)?;
            }
            return Ok(());
        }
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
    /// Wake a sleeping Mac display and unlock `loginwindow` with `password`.
    /// Verifies `password` via OpenDirectory first when the user is a local
    /// account, and checks `desktop::is_locked()` before every keystroke so
    /// nothing is ever typed into an unlocked desktop.
    fn unlock(&mut self, password: &str) -> Result<()> {
        if password.is_empty() || password.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0')) {
            return Err(error("invalid unlock password"));
        }
        desktop::on_console()?;
        // Re-check Accessibility fresh in case the owner just granted it.
        self.access = None;
        self.allowed()?;
        if !desktop::is_locked() {
            return Ok(());
        }
        if unlock::verify_local_password(password) == PasswordCheck::Invalid {
            return Err(error("incorrect Mac login password"));
        }
        power::force_light();
        let wake_until = Instant::now() + UNLOCK_WAKE_WAIT;
        while CGDisplayIsAsleep(CGMainDisplayID()) && Instant::now() < wake_until {
            std::thread::sleep(UNLOCK_WAKE_POLL);
        }
        std::thread::sleep(UNLOCK_FOCUS_SETTLE);
        if !desktop::is_locked() {
            return Ok(());
        }
        self.modifiers = Modifiers::default();
        self.refresh_layout();
        // Nudge `loginwindow` so the password field owns keyboard focus, then
        // replace any stale dots before typing the password.
        let _ = self.mouse(
            CGEventType::MouseMoved,
            CGMouseButton::Left,
            Some((0, 0)),
            None,
        );
        self.clear_lock_field()?;
        std::thread::sleep(Duration::from_millis(20));
        for ch in password.chars() {
            if !desktop::is_locked() {
                return Ok(());
            }
            self.type_lock_char(ch)?;
            std::thread::sleep(UNLOCK_KEY_PACE);
        }
        if !desktop::is_locked() {
            return Ok(());
        }
        self.keyboard(keys::RETURN, true, 0, false)?;
        self.keyboard(keys::RETURN, false, 0, false)?;
        let verify_until = Instant::now() + UNLOCK_VERIFY_WAIT;
        while Instant::now() < verify_until {
            if !desktop::is_locked() {
                return Ok(());
            }
            std::thread::sleep(UNLOCK_VERIFY_POLL);
        }
        if !desktop::is_locked() {
            Ok(())
        } else {
            Err(error("the Mac stayed locked after entering the password"))
        }
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
