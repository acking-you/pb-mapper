//! Pointer metadata, confined to the selected display, and the pointer's
//! shape.

use crate::Display;
use serde::{Deserialize, Serialize};
use std::{io, sync::Arc};

/// Generic pointer hotspot in physical pixels relative to the selected display.
/// This is position/visibility metadata; the image is a [`CursorShape`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPosition {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}

/// What the host's pointer looks like, as a standard shape a viewer's own
/// operating system has too, or `Custom`, which only the bitmap shows. A
/// closed set: a value a peer does not know needs a capability of its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CursorKind {
    #[default]
    Arrow,
    /// Text selection.
    Ibeam,
    /// A link.
    Hand,
    /// Busy: nothing can be clicked.
    Wait,
    /// Busy in the background: the arrow still clicks.
    Progress,
    Crosshair,
    Help,
    NotAllowed,
    /// Move in every direction.
    Move,
    /// Resize up and down.
    ResizeNs,
    /// Resize left and right.
    ResizeEw,
    /// Resize along the top-left to bottom-right diagonal.
    ResizeNwse,
    /// Resize along the top-right to bottom-left diagonal.
    ResizeNesw,
    UpArrow,
    /// None of the above: only the bitmap shows it.
    Custom,
}

/// The pointer's shape: its kind, its bitmap and the hotspot in it. The
/// bitmap is straight (not premultiplied) RGBA8, row by row from the top,
/// in the host's physical pixels: 1 to [`CursorShape::MAX_SIDE`] a side,
/// with the hotspot inside. Every shape carries its bitmap, a standard one
/// too: a viewer that draws the pointer itself paints it. Cloning shares the
/// bitmap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorShape {
    kind: CursorKind,
    width: u16,
    height: u16,
    hot_x: u16,
    hot_y: u16,
    rgba: Arc<[u8]>,
}
impl CursorShape {
    /// The longest side of a shape. A host scales a larger pointer down.
    pub const MAX_SIDE: u32 = 256;

    /// A shape of `width` by `height` pixels whose hotspot is `hotspot`,
    /// `rgba` holding `width * height * 4` bytes. Fails as `InvalidData`
    /// outside those bounds.
    pub fn new(
        kind: CursorKind,
        width: u32,
        height: u32,
        hotspot: (u32, u32),
        rgba: impl Into<Arc<[u8]>>,
    ) -> io::Result<Self> {
        let rgba = rgba.into();
        let side = 1..=Self::MAX_SIDE;
        if !side.contains(&width)
            || !side.contains(&height)
            || hotspot.0 >= width
            || hotspot.1 >= height
            || rgba.len() != Self::bytes(width, height)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "cursor shape out of bounds",
            ));
        }
        let side = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
        Ok(Self {
            kind,
            width: side(width),
            height: side(height),
            hot_x: side(hotspot.0),
            hot_y: side(hotspot.1),
            rgba,
        })
    }

    /// The bitmap's length for a shape of that size.
    pub(crate) fn bytes(width: u32, height: u32) -> usize {
        width as usize * height as usize * 4
    }

    /// A shape of a pointer the platform read, any size: one larger than
    /// `MAX_SIDE` a side is scaled down to fit, its hotspot with it, and a
    /// hotspot outside the image is moved to its nearest edge.
    #[cfg_attr(
        not(any(windows, test)),
        expect(dead_code, reason = "for the platforms that read pointer bitmaps")
    )]
    pub(crate) fn fitted(
        kind: CursorKind,
        width: u32,
        height: u32,
        hotspot: (u32, u32),
        rgba: Vec<u8>,
    ) -> io::Result<Self> {
        let longest = width.max(height);
        if longest <= Self::MAX_SIDE
            || width == 0
            || height == 0
            || rgba.len() != Self::bytes(width, height)
        {
            // Within bounds already, or no bitmap of that size, which `new`
            // refuses.
            let hotspot = (
                hotspot.0.min(width.saturating_sub(1)),
                hotspot.1.min(height.saturating_sub(1)),
            );
            return Self::new(kind, width, height, hotspot, rgba);
        }
        let scale = |value: u32| {
            u32::try_from(u64::from(value) * u64::from(Self::MAX_SIDE) / u64::from(longest))
                .unwrap_or(u32::MAX)
        };
        let (fit_width, fit_height) = (scale(width).max(1), scale(height).max(1));
        let hotspot = (
            scale(hotspot.0).min(fit_width - 1),
            scale(hotspot.1).min(fit_height - 1),
        );
        let rgba = shrink(&rgba, (width, height), (fit_width, fit_height));
        Self::new(kind, fit_width, fit_height, hotspot, rgba)
    }

    pub fn kind(&self) -> CursorKind {
        self.kind
    }
    pub fn width(&self) -> u32 {
        self.width.into()
    }
    pub fn height(&self) -> u32 {
        self.height.into()
    }
    /// Where the pointer points, in the bitmap's pixels.
    pub fn hotspot(&self) -> (u32, u32) {
        (self.hot_x.into(), self.hot_y.into())
    }
    /// Straight RGBA8, `width * height * 4` bytes, top row first.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
    /// The same bitmap, not merely an equal one: a cheap identity check
    /// before comparing content.
    pub(crate) fn shares_bitmap(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.rgba, &other.rgba)
    }
}

/// `rgba`, `from` pixels, scaled down to `to` by averaging each target
/// pixel's box, colours weighted by their alpha so that transparent pixels
/// add no dark fringe.
#[cfg_attr(
    not(any(windows, test)),
    expect(dead_code, reason = "for the platforms that read pointer bitmaps")
)]
fn shrink(rgba: &[u8], from: (u32, u32), to: (u32, u32)) -> Vec<u8> {
    let span = |target: u32, of: u32, total: u32| {
        let start = u64::from(target) * u64::from(total) / u64::from(of);
        let end = (u64::from(target) + 1) * u64::from(total) / u64::from(of);
        start as usize..(end as usize).max(start as usize + 1)
    };
    let mut out = Vec::with_capacity(CursorShape::bytes(to.0, to.1));
    for ty in 0..to.1 {
        let rows = span(ty, to.1, from.1);
        for tx in 0..to.0 {
            let columns = span(tx, to.0, from.0);
            let (mut sum, mut alpha, mut count) = ([0_u64; 3], 0_u64, 0_u64);
            for y in rows.clone() {
                for x in columns.clone() {
                    let at = (y * from.0 as usize + x) * 4;
                    if let Some(&[r, g, b, a]) = rgba.get(at..at + 4) {
                        let a64 = u64::from(a);
                        sum[0] += u64::from(r) * a64;
                        sum[1] += u64::from(g) * a64;
                        sum[2] += u64::from(b) * a64;
                        alpha += a64;
                        count += 1;
                    }
                }
            }
            let channel =
                |sum: u64| u8::try_from(sum.checked_div(alpha).unwrap_or(0)).unwrap_or(u8::MAX);
            let a = u8::try_from(alpha.checked_div(count).unwrap_or(0)).unwrap_or(u8::MAX);
            out.extend_from_slice(&[channel(sum[0]), channel(sum[1]), channel(sum[2]), a]);
        }
    }
    out
}

/// The pointer at `(x, y)`, absolute physical virtual-desktop pixels, relative
/// to `display`. A hidden pointer, or one on another display, reports nothing:
/// its position elsewhere is not disclosed.
pub(crate) fn confine(display: &Display, (x, y): (i32, i32), showing: bool) -> CursorPosition {
    let x = i64::from(x) - i64::from(display.x);
    let y = i64::from(y) - i64::from(display.y);
    if !showing || x < 0 || y < 0 || x >= i64::from(display.width) || y >= i64::from(display.height)
    {
        // Do not disclose the cursor location on a different monitor.
        return CursorPosition::default();
    }
    let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else {
        return CursorPosition::default();
    };
    CursorPosition {
        x,
        y,
        visible: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointer_coordinates_are_physical_and_confined_to_the_selected_display() {
        let display = Display {
            id: 1,
            name: "left monitor".into(),
            x: -1920,
            y: -200,
            width: 1920,
            height: 1080,
        };
        let point = (-100, 300);
        assert_eq!(
            confine(&display, point, true),
            CursorPosition {
                x: 1820,
                y: 500,
                visible: true
            }
        );
        // Both an off-display cursor and an OS-hidden cursor withhold position.
        assert_eq!(confine(&display, (0, 300), true), CursorPosition::default());
        assert_eq!(confine(&display, point, false), CursorPosition::default());
    }

    /// A pointer larger than a shape may be is scaled down to fit, its
    /// hotspot with it; a bitmap of another size is refused.
    #[test]
    fn a_large_pointer_is_scaled_down_to_the_largest_shape() {
        // Opaque white on the left half, clear on the right.
        let (width, height) = (512, 300);
        let rgba: Vec<u8> = (0..height)
            .flat_map(|_| (0..width).flat_map(|x| if x < 256 { [255; 4] } else { [0; 4] }))
            .collect();
        let shape =
            CursorShape::fitted(CursorKind::Custom, width, height, (511, 10), rgba).unwrap();
        assert_eq!((shape.width(), shape.height()), (256, 150));
        assert_eq!(shape.hotspot(), (255, 5));
        let pixel = |x: u32| {
            let at = x as usize * 4;
            shape.rgba()[at..at + 4].to_vec()
        };
        assert_eq!((pixel(0), pixel(255)), (vec![255; 4], vec![0; 4]));
        assert!(CursorShape::fitted(CursorKind::Arrow, 2, 2, (0, 0), vec![0; 4]).is_err());
    }
}
