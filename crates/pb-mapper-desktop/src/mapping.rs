//! Mapping the picture to input: a point in the video to a pointer on the
//! host's display, and text to input chunks the host accepts.
use crate::{DesktopView, Input};

/// The most UTF-8 bytes one `Input::Text` carries.
const TEXT_MAX: usize = 1024;

impl DesktopView {
    /// The pointer move to the display pixel under `(nx, ny)`, a point in
    /// the video normalised to 0.0..=1.0 from its top left. The video keeps
    /// the display's aspect ratio, so the same fractions address the
    /// display, whose physical pixels are what input is in; a point outside
    /// the picture is held to its edge. `None` until the host has described
    /// its display, or for a coordinate that is not a number.
    pub fn display_point(&self, nx: f32, ny: f32) -> Option<Input> {
        let axis = |n: f32, size: u32| {
            (n.is_finite() && size > 0)
                .then(|| (f64::from(n) * f64::from(size)).floor() as i64)
                .map(|pixel| pixel.clamp(0, i64::from(size) - 1) as u32)
        };
        Some(Input::Move {
            x: axis(nx, self.display_width)?,
            y: axis(ny, self.display_height)?,
        })
    }
}

impl Input {
    /// `text` as the `Input::Text` events the host accepts: at most 1024
    /// UTF-8 bytes each, split between characters, with NUL left out. Empty
    /// text makes none. Each is one batch for [`crate::Viewer::input`].
    pub fn text_chunks(text: &str) -> impl Iterator<Item = Input> + '_ {
        let mut chars = text.chars().filter(|c| *c != '\0').peekable();
        std::iter::from_fn(move || {
            let mut chunk = String::new();
            while let Some(c) = chars.next_if(|c| chunk.len() + c.len_utf8() <= TEXT_MAX) {
                chunk.push(c);
            }
            (!chunk.is_empty()).then_some(Input::Text { text: chunk })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Display;

    #[test]
    fn a_video_point_lands_on_the_display_pixel_under_it() {
        let view = DesktopView {
            display_width: 2560,
            display_height: 1440,
            video_width: 1920,
            video_height: 1080,
            ..Default::default()
        };
        let mv = |nx, ny| match view.display_point(nx, ny) {
            Some(Input::Move { x, y }) => Some((x, y)),
            _ => None,
        };
        assert_eq!(mv(0.5, 0.25), Some((1280, 360)));
        // The far edge and beyond stay on the display, where the host
        // accepts them.
        assert_eq!(mv(1.0, 2.0), Some((2559, 1439)));
        assert_eq!(mv(-0.1, 0.0), Some((0, 0)));
        assert_eq!(mv(f32::NAN, 0.5), None);
        assert!(DesktopView::default().display_point(0.5, 0.5).is_none());
    }

    #[test]
    fn text_splits_between_characters_within_the_host_limit() {
        let display = Display {
            id: 0,
            name: String::new(),
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        // 3-byte characters do not divide 1024: a split mid-character would
        // be invalid UTF-8, and NUL would be refused.
        let text = format!("{}\0{}", "語".repeat(400), "a".repeat(30));
        let chunks: Vec<String> = Input::text_chunks(&text)
            .map(|input| {
                input.validate(&display).unwrap();
                match input {
                    Input::Text { text } => text,
                    _ => unreachable!(),
                }
            })
            .collect();
        assert_eq!(
            chunks.iter().map(String::len).collect::<Vec<_>>(),
            [1023, 207]
        );
        assert_eq!(chunks.concat(), text.replace('\0', ""));
        assert_eq!(Input::text_chunks("\0").count(), 0);
    }
}
