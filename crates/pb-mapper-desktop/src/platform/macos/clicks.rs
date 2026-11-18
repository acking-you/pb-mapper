//! Click counts. macOS reads a double click from the click state an event
//! carries, which it never works out for posted events: the injector counts
//! presses of one button, close in time and place, as the system does for
//! a mouse.

use crate::Button;
use std::time::{Duration, Instant};

/// How far, in points, the pointer may move between the clicks of one
/// double click.
pub(crate) const SLOP: f64 = 4.0;

/// The last press, and how many clicks it made.
#[derive(Default)]
pub(crate) struct Clicks {
    last: Option<(Button, Instant, (f64, f64), i64)>,
}

impl Clicks {
    /// The click state of a press of `button` at `point` (global points),
    /// `at`, when the owner's double-click interval is `interval`: one more
    /// than the press before it when that was the same button, within the
    /// interval and the slop; else 1.
    pub(crate) fn press(
        &mut self,
        button: Button,
        at: Instant,
        point: (f64, f64),
        interval: Duration,
    ) -> i64 {
        let count = match self.last {
            Some((previous, when, (x, y), count))
                if previous == button
                    && at.saturating_duration_since(when) <= interval
                    && (point.0 - x).abs() <= SLOP
                    && (point.1 - y).abs() <= SLOP =>
            {
                count + 1
            }
            _ => 1,
        };
        self.last = Some((button, at, point, count));
        count
    }

    /// The click state of the release of `button`: its press's.
    pub(crate) fn release(&self, button: Button) -> i64 {
        match self.last {
            Some((pressed, _, _, count)) if pressed == button => count,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two quick presses in place are a double click and a third a triple;
    /// a slow one, one elsewhere or another button starts again.
    #[test]
    fn quick_presses_in_place_count_up() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let interval = Duration::from_millis(500);
        let mut clicks = Clicks::default();
        assert_eq!(clicks.press(Button::Left, at(0), (10.0, 10.0), interval), 1);
        assert_eq!(clicks.release(Button::Left), 1);
        assert_eq!(
            clicks.press(Button::Left, at(300), (12.0, 9.0), interval),
            2
        );
        assert_eq!(clicks.release(Button::Left), 2);
        assert_eq!(
            clicks.press(Button::Left, at(600), (12.0, 9.0), interval),
            3
        );
        assert_eq!(
            clicks.press(Button::Left, at(1200), (12.0, 9.0), interval),
            1
        );
        assert_eq!(
            clicks.press(Button::Left, at(1300), (30.0, 9.0), interval),
            1
        );
        assert_eq!(
            clicks.press(Button::Right, at(1400), (30.0, 9.0), interval),
            1
        );
        assert_eq!(clicks.release(Button::Left), 1);
    }
}
