//! Pointer metadata, confined to the selected display, the pointer's shape,
//! and the pointer an injector's move leaves until the OS shows it.

use crate::Display;
use serde::{Deserialize, Serialize};
use std::{io, sync::Arc};

/// Well past the time an OS takes to apply a posted move, even under load;
/// a stop short of the injected pixel is reported this late.
#[cfg_attr(
    not(any(windows, target_os = "macos", test)),
    expect(dead_code, reason = "for the hosts that post moves")
)]
pub(crate) const SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

/// The last move an injector posted, until the OS shows it. Both hosts post
/// a move the OS applies after the call returns (`CGEventPost`,
/// `SendInput`), so the pointer read right after it can still show where it
/// was. Thread C reports every position but the one it injected, and a
/// viewer in control follows a report more than a pixel from its own
/// pointer, so that stale position would drag the viewer's pointer back after
/// each move: it would flicker between the two for as long as it moved. The
/// `Backend::cursor` contract is the pointer as it stands after every input
/// applied. Until a read shows the injected pixel, or `SETTLE` passes after
/// the last move (the system stopped it short, someone at the host moved the
/// mouse, or the move never took effect), the injected pixel is the pointer.
#[cfg_attr(
    not(any(windows, target_os = "macos", test)),
    expect(dead_code, reason = "for the hosts that post moves")
)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Pointer {
    /// Absolute pixel, and when it was posted.
    moved: Option<((i32, i32), std::time::Instant)>,
    /// What the last read showed (`Some(None)`: a pointer not on the bound
    /// display); `None` before the first read.
    shown: Option<Option<(i32, i32)>>,
    /// What the last read before the open window's first move showed: where
    /// the pointer was before any of its moves.
    from: Option<Option<(i32, i32)>>,
}

/// What a read of the pointer reports while a move settles.
#[cfg_attr(
    not(any(windows, target_os = "macos", test)),
    expect(dead_code, reason = "for the hosts that post moves")
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settled {
    /// The pointer to report: the injected pixel until a read shows it.
    pub(crate) at: Option<(i32, i32)>,
    /// The injected pixel, when this read ended its settle window with the
    /// pointer more than a pixel from it and where the last read before the
    /// window's first move found it: no move posted in the window took
    /// effect. A pointer that moved meanwhile (a move that landed a pixel off
    /// or was stopped short, someone at the host), one that ends within a
    /// pixel of the target, or a window opened before any read says nothing
    /// of the moves.
    pub(crate) unmoved: Option<(i32, i32)>,
}

#[cfg_attr(
    not(any(windows, target_os = "macos", test)),
    expect(dead_code, reason = "for the hosts that post moves")
)]
impl Pointer {
    /// A move to absolute pixel `to` was posted at `now`; the first one
    /// since the last window closed opens a window from the last read.
    pub(crate) fn moved(&mut self, to: (i32, i32), now: std::time::Instant) {
        if self.moved.is_none() {
            self.from = self.shown;
        }
        self.moved = Some((to, now));
    }

    /// The pointer to report at `now`, given the absolute pixel the OS shows
    /// (`None`: not on the bound display).
    pub(crate) fn settle(&mut self, shown: Option<(i32, i32)>, now: std::time::Instant) -> Settled {
        self.shown = Some(shown);
        let Some((to, at)) = self.moved else {
            return Settled {
                at: shown,
                unmoved: None,
            };
        };
        if shown != Some(to) && now.saturating_duration_since(at) < SETTLE {
            return Settled {
                at: Some(to),
                unmoved: None,
            };
        }
        self.moved = None;
        let from = self.from.take();
        let near = shown.is_some_and(|(x, y)| x.abs_diff(to.0) <= 1 && y.abs_diff(to.1) <= 1);
        Settled {
            at: shown,
            unmoved: (!near && from == Some(shown)).then_some(to),
        }
    }
}

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

    /// The longest side of a pointer image a platform reads at all: four
    /// times `MAX_SIDE`, which covers the largest pointer an OS draws at any
    /// accessibility size and scale. A larger one is not read, so a program
    /// that sets a huge cursor costs the host nothing per tick, and the
    /// viewer keeps the last shape.
    pub(crate) const MAX_READ_SIDE: u32 = Self::MAX_SIDE * 4;

    /// Whether a platform may read a pointer image of that size: before it
    /// allocates or copies a pixel. `InvalidData` for an empty or too large
    /// one, a refusal that reading again does not change.
    #[cfg_attr(
        not(any(windows, target_os = "macos", test)),
        expect(dead_code, reason = "for the platforms that read pointer bitmaps")
    )]
    pub(crate) fn readable(width: u32, height: u32) -> io::Result<()> {
        let side = 1..=Self::MAX_READ_SIDE;
        if side.contains(&width) && side.contains(&height) {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the pointer's image is empty or too large to read",
        ))
    }

    /// A shape of a pointer the platform read, as `readable` bounds it: one
    /// larger than `MAX_SIDE` a side is scaled down to fit, its hotspot with
    /// it, and a hotspot outside the image is moved to its nearest edge.
    #[cfg_attr(
        not(any(windows, target_os = "macos", test)),
        expect(dead_code, reason = "for the platforms that read pointer bitmaps")
    )]
    pub(crate) fn fitted(
        kind: CursorKind,
        width: u32,
        height: u32,
        hotspot: (u32, u32),
        rgba: Vec<u8>,
    ) -> io::Result<Self> {
        Self::readable(width, height)?;
        let longest = width.max(height);
        if longest <= Self::MAX_SIDE || rgba.len() != Self::bytes(width, height) {
            // Within bounds already, or no bitmap of that size, which `new`
            // refuses.
            let hotspot = (hotspot.0.min(width - 1), hotspot.1.min(height - 1));
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
    not(any(windows, target_os = "macos", test)),
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
    use std::time::{Duration, Instant};

    /// Before the OS applies a move, the read shows the old pixel, or none
    /// when that was off the display: the injected one is reported. Once it
    /// shows the move, reads are the OS's again, so a later move of the
    /// host's own mouse is reported; and a move the system stopped short is
    /// reported once `SETTLE` has passed.
    #[test]
    fn a_posted_move_is_the_pointer_until_the_os_shows_it() {
        let start = Instant::now();
        let mut pointer = Pointer::default();
        let at = |pointer: &mut Pointer, shown, now| pointer.settle(shown, now).at;
        assert_eq!(at(&mut pointer, Some((5, 5)), start), Some((5, 5)));

        pointer.moved((100, 40), start);
        assert_eq!(at(&mut pointer, Some((5, 5)), start), Some((100, 40)));
        assert_eq!(at(&mut pointer, None, start), Some((100, 40)));
        assert_eq!(at(&mut pointer, Some((100, 40)), start), Some((100, 40)));
        assert_eq!(at(&mut pointer, Some((7, 9)), start), Some((7, 9)));

        pointer.moved((200, 80), start);
        let late = start + SETTLE;
        assert_eq!(
            at(
                &mut pointer,
                Some((199, 80)),
                late - Duration::from_millis(1)
            ),
            Some((200, 80))
        );
        assert_eq!(at(&mut pointer, Some((199, 80)), late), Some((199, 80)));
    }

    /// A window that ends with the pointer where the last read before its
    /// first move found it, more than a pixel from the target, says no move
    /// in it took effect, however many were posted; one that ends at or
    /// next to the target, with a pointer that moved meanwhile, or that
    /// opened before any read says nothing of the moves.
    #[test]
    fn a_window_that_ends_where_it_started_reports_its_moves_unmoved() {
        let start = Instant::now();
        let late = start + SETTLE;
        // A pointer last read at `shown`, with no window open.
        let read = |shown| {
            let mut pointer = Pointer::default();
            pointer.settle(shown, start);
            pointer
        };
        let unmoved = |pointer: &mut Pointer, shown, now| pointer.settle(shown, now).unmoved;

        // Every move dropped: the pointer stays at (5, 5) throughout, and
        // the window, kept open by each move, ends `SETTLE` after the last.
        let mut pointer = read(Some((5, 5)));
        pointer.moved((100, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), start), None);
        pointer.moved((120, 40), start + Duration::from_millis(100));
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), late), None);
        let end = late + Duration::from_millis(100);
        assert_eq!(
            pointer.settle(Some((5, 5)), end),
            Settled {
                at: Some((5, 5)),
                unmoved: Some((120, 40)),
            }
        );
        // Taken once: the window is over.
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), end), None);

        // No read in the window but the one that ends it.
        let mut pointer = read(Some((5, 5)));
        pointer.moved((100, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), late), Some((100, 40)));

        // Off the display before and after counts as the same place.
        let mut pointer = read(None);
        pointer.moved((100, 40), start);
        assert_eq!(unmoved(&mut pointer, None, start), None);
        assert_eq!(unmoved(&mut pointer, None, late), Some((100, 40)));

        // The move landed a pixel off, read there from the window's first
        // read on.
        let mut pointer = read(Some((5, 5)));
        pointer.moved((100, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((101, 40)), start), None);
        assert_eq!(unmoved(&mut pointer, Some((101, 40)), late), None);

        // Someone at the host moved the mouse during the window, even
        // before its first read, and then left it; a later move in the
        // window keeps where the window started.
        let mut pointer = read(Some((5, 5)));
        pointer.moved((100, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((60, 9)), start), None);
        pointer.moved((120, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((60, 9)), late), None);

        // A stroke that comes back to where it started reached its target,
        // and one that ends a pixel from it is as good as there.
        let mut pointer = read(Some((5, 5)));
        pointer.moved((100, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), start), None);
        pointer.moved((5, 5), start);
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), start), None);
        let mut pointer = read(Some((5, 5)));
        pointer.moved((6, 4), start);
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), late), None);

        // Reached late, after reads of the old pixel.
        let mut pointer = read(Some((5, 5)));
        pointer.moved((100, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), start), None);
        assert_eq!(unmoved(&mut pointer, Some((100, 40)), late), None);

        // A window opened before any read.
        let mut pointer = Pointer::default();
        pointer.moved((100, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), start), None);
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), late), None);

        // A read that shows the move closes the window, and the next one
        // starts where that read found the pointer.
        let mut pointer = read(Some((5, 5)));
        pointer.moved((100, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((100, 40)), start), None);
        pointer.moved((200, 40), start);
        assert_eq!(
            unmoved(&mut pointer, Some((100, 40)), late),
            Some((200, 40))
        );
        pointer.moved((300, 40), start);
        assert_eq!(unmoved(&mut pointer, Some((5, 5)), late), None);
    }

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
    /// hotspot with it; a bitmap of another size is refused, and so is a
    /// pointer too large to read, before a platform reads a pixel of it.
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
        let max = CursorShape::MAX_READ_SIDE;
        assert!(CursorShape::readable(max, max).is_ok());
        for refused in [
            CursorShape::readable(max + 1, 1),
            CursorShape::readable(0, 1),
            CursorShape::fitted(CursorKind::Custom, 1, max + 1, (0, 0), vec![]).map(drop),
        ] {
            assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }
}
