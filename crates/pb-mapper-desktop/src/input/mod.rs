//! Neutral input rules every platform's injector follows.
pub(crate) mod cursor;
pub(crate) mod held;

#[cfg(test)]
mod tests {
    use crate::{Button, Display, Input, Key};

    /// The injection boundary refuses a pointer off the display, text too
    /// long or holding NUL, and a scroll too large; nothing else.
    #[test]
    fn input_is_refused_only_outside_its_bounds() {
        let display = Display {
            id: 1,
            name: "bounds".into(),
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let valid = |input: Input| input.validate(&display).is_ok();
        assert!(valid(Input::Move { x: 1919, y: 1079 }));
        assert!(!valid(Input::Move { x: 1920, y: 0 }) && !valid(Input::Move { x: 0, y: 1080 }));
        assert!(valid(Input::Text {
            text: "a".repeat(1024)
        }));
        assert!(
            !valid(Input::Text {
                text: "a".repeat(1025)
            }) && !valid(Input::Text { text: "a\0".into() })
        );
        assert!(valid(Input::Wheel {
            horizontal: -100,
            vertical: 100
        }));
        assert!(
            !valid(Input::Wheel {
                horizontal: 101,
                vertical: 0
            }) && !valid(Input::Wheel {
                horizontal: 0,
                vertical: i32::MIN
            })
        );
        assert!(valid(Input::Button {
            button: Button::Left,
            down: true,
            drag_start: true
        }));
        assert!(valid(Input::Key {
            key: Key::A,
            down: true
        }));
        assert!(valid(Input::Lock {}));
    }
}
