//! The pointer on macOS: where the system has it, in pixels of the bound
//! display, and its shape. Drawn apart from the picture (ScreenCaptureKit
//! captures without it). macOS says nothing reliable about another app
//! hiding it, so on the display it shows.
//!
//! The shape is `NSCursor.currentSystemCursor`, the pointer every app shows,
//! read again only when the window server's cursor seed moved (a private
//! counter, looked up at run time; without it, at most twice a second).
//! Its image is rendered at the display's scale into straight RGBA, and
//! named by the standard cursor it equals pixel for pixel, else `Custom`.
//! AppKit is called from thread C: `NSCursor`'s class methods and `NSImage`
//! are documented thread-safe for reading, and each read runs in its own
//! autorelease pool. Without a window-server connection AppKit hands out no
//! cursor at all: every cursor and image is read as possibly nil (objc2's
//! accessors assert one), and then there is no shape, or it is `Custom`.

use super::{cf, desktop::Shared, pixels};
use crate::{CursorKind, CursorMode, CursorShape, Result, error, platform::CursorSource};
use objc2::{
    ClassType, msg_send,
    rc::{Retained, autoreleasepool},
};
use objc2_app_kit::{NSCursor, NSImage};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGEvent, CGImageAlphaInfo,
    CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::time::{Duration, Instant};

/// How often the shape is read again where the system has no cursor seed.
const UNSEEDED: Duration = Duration::from_millis(500);

/// `CGSCurrentCursorSeed`: moves whenever the pointer's image changes.
type Seed = unsafe extern "C" fn() -> i32;

/// What says the pointer may have a new shape.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Identity {
    Seed(i32),
    /// No seed on this system: a read every `UNSEEDED`.
    Read(Instant),
}

pub(crate) struct MacCursor {
    geometry: Shared,
    seed: Option<Seed>,
    /// The identity last shaped, or tried: one that failed is not retried.
    seen: Option<Identity>,
    /// The standard cursors rendered at a scale, to name a shape; none
    /// rendered (AppKit had none to give) are asked for again.
    standards: Option<(u64, Vec<(CursorKind, CursorShape)>)>,
}

impl MacCursor {
    pub(crate) fn new(geometry: Shared) -> Self {
        let seed = cf::function(c"CGSCurrentCursorSeed").map(|address| {
            // SAFETY: the window server's `int CGSCurrentCursorSeed(void)`.
            unsafe { std::mem::transmute::<*mut std::ffi::c_void, Seed>(address.as_ptr()) }
        });
        Self {
            geometry,
            seed,
            seen: None,
            standards: None,
        }
    }

    /// Whether the pointer may have changed since the last shape.
    fn changed(&mut self) -> bool {
        let identity = match self.seed {
            // SAFETY: takes nothing, reads a counter.
            Some(seed) => Identity::Seed(unsafe { seed() }),
            None => match self.seen {
                Some(Identity::Read(at)) if at.elapsed() < UNSEEDED => return false,
                _ => Identity::Read(Instant::now()),
            },
        };
        if self.seen == Some(identity) {
            return false;
        }
        self.seen = Some(identity);
        true
    }

    /// The kind of `shape`: the standard cursor it equals at `scale`.
    fn kind(&mut self, shape: &CursorShape, scale: f64) -> CursorKind {
        let key = scale.to_bits();
        if self
            .standards
            .as_ref()
            .is_none_or(|(at, standards)| *at != key || standards.is_empty())
        {
            self.standards = Some((key, standards(scale)));
        }
        self.standards
            .as_ref()
            .and_then(|(_, standards)| {
                standards.iter().find(|(_, standard)| {
                    standard.hotspot() == shape.hotspot()
                        && (standard.width(), standard.height()) == (shape.width(), shape.height())
                        && standard.rgba() == shape.rgba()
                })
            })
            .map_or(CursorKind::Custom, |(kind, _)| *kind)
    }
}

/// The standard cursors that have a kind and that AppKit gives, rendered
/// at `scale`.
fn standards(scale: f64) -> Vec<(CursorKind, CursorShape)> {
    /// The class's cursor of that name, or `None`.
    macro_rules! standard {
        ($name:ident) => {{
            // SAFETY: an `NSCursor` class method that takes nothing and
            // returns an autoreleased cursor, or nil.
            let cursor: Option<Retained<NSCursor>> = unsafe { msg_send![NSCursor::class(), $name] };
            cursor
        }};
    }
    let cursors = [
        (CursorKind::Arrow, standard!(arrowCursor)),
        (CursorKind::Ibeam, standard!(IBeamCursor)),
        (CursorKind::Hand, standard!(pointingHandCursor)),
        (CursorKind::Crosshair, standard!(crosshairCursor)),
        (CursorKind::NotAllowed, standard!(operationNotAllowedCursor)),
        // Deprecated, and still the ones apps show.
        (CursorKind::ResizeEw, standard!(resizeLeftRightCursor)),
        (CursorKind::ResizeNs, standard!(resizeUpDownCursor)),
    ];
    cursors
        .into_iter()
        .filter_map(|(kind, cursor)| {
            let cursor = cursor?;
            render(&cursor, scale, kind)
                .ok()
                .flatten()
                .map(|shape| (kind, shape))
        })
        .collect()
}

/// `cursor`'s image and hotspot at `scale` pixels a point, straight RGBA,
/// scaled down to the largest shape; `None` for no image or an empty one.
fn render(cursor: &NSCursor, scale: f64, kind: CursorKind) -> Result<Option<CursorShape>> {
    // SAFETY: takes nothing, returns the cursor's image or nil.
    let image: Option<Retained<NSImage>> = unsafe { msg_send![cursor, image] };
    let Some(image) = image else {
        return Ok(None);
    };
    let size: NSSize = image.size();
    if !(size.width > 0.0 && size.height > 0.0) {
        return Ok(None);
    }
    let hotspot: NSPoint = cursor.hotSpot();
    let ((width, height), hot) =
        pixels::in_pixels((size.width, size.height), (hotspot.x, hotspot.y), scale);
    // Never more than four times the largest shape before scaling down.
    let limit = CursorShape::MAX_SIDE * 4;
    if width > limit || height > limit {
        return Err(error("the pointer's image is too large"));
    }
    let pixel_size = CGSize {
        width: f64::from(width),
        height: f64::from(height),
    };
    let mut proposed = NSRect::new(NSPoint::new(0.0, 0.0), pixel_size);
    // SAFETY: a local rect, no context, no hints: the representation that
    // fits that many pixels best.
    let Some(picture) =
        (unsafe { image.CGImageForProposedRect_context_hints(&mut proposed, None, None) })
    else {
        return Ok(None);
    };
    let mut rgba = vec![0u8; CursorShape::bytes(width, height)];
    // SAFETY: a static name.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or_else(|| error("no sRGB colour space"))?;
    let info = CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0;
    {
        // SAFETY: `rgba` holds `height` rows of `width * 4` bytes and
        // outlives the context, which drops at the end of this block.
        let context = unsafe {
            CGBitmapContextCreate(
                rgba.as_mut_ptr().cast(),
                width as usize,
                height as usize,
                8,
                width as usize * 4,
                Some(&space),
                info,
            )
        }
        .ok_or_else(|| error("no bitmap for the pointer"))?;
        let all = CGRect::new(CGPoint::new(0.0, 0.0), pixel_size);
        CGContext::draw_image(Some(&context), all, Some(&picture));
    }
    pixels::unpremultiply(&mut rgba);
    CursorShape::fitted(kind, width, height, hot, rgba).map(Some)
}

impl CursorSource for MacCursor {
    const MODE: CursorMode = CursorMode::Separate;
    const SHAPES: bool = true;
    /// On another display, it is hidden from this one's viewer.
    fn read(&mut self) -> Result<Option<(i32, i32, bool)>> {
        let here = CGEvent::new(None);
        let point = CGEvent::location(here.as_deref());
        Ok(Some(match self.geometry.get().pixel((point.x, point.y)) {
            Some((x, y)) => (x, y, true),
            None => (0, 0, false),
        }))
    }
    #[expect(
        deprecated,
        reason = "currentSystemCursor is the only public way to the pointer other apps show; nil on a system without it"
    )]
    fn shape(&mut self) -> Result<Option<CursorShape>> {
        if !self.changed() {
            return Ok(None);
        }
        let scale = self.geometry.get().scale;
        autoreleasepool(|_| {
            let Some(cursor) = NSCursor::currentSystemCursor() else {
                return Ok(None);
            };
            let Some(shape) = render(&cursor, scale, CursorKind::Custom)? else {
                return Ok(None);
            };
            let kind = self.kind(&shape, scale);
            if kind == CursorKind::Custom {
                return Ok(Some(shape));
            }
            CursorShape::new(
                kind,
                shape.width(),
                shape.height(),
                shape.hotspot(),
                shape.rgba().to_vec(),
            )
            .map(Some)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::macos::geometry::Geometry;
    use objc2_app_kit::NSApplication;
    use objc2_core_graphics::CGSessionCopyCurrentDictionary;
    use std::{cell::Cell, io::Write, rc::Rc};

    /// In a window-server session, with AppKit's application loaded as the
    /// app has it, the pointer the system shows and the standard pointers
    /// render at 2x into straight RGBA within a shape's bounds, off the main
    /// thread as thread C reads them; each standard one is named by its own
    /// image, and one with another hotspot is custom. Without a session there
    /// is no shape, and no panic.
    #[test]
    fn standard_pointers_render_and_are_named_by_their_image() {
        NSApplication::load();
        let geometry = Geometry::new((0.0, 0.0, 100.0, 100.0), 200, 100);
        let mut cursor = MacCursor::new(Rc::new(Cell::new(geometry)));
        let rendered = autoreleasepool(|_| standards(2.0));
        if rendered.is_empty() && CGSessionCopyCurrentDictionary().is_none() {
            assert!(cursor.shape().unwrap().is_none());
            // Past the harness's capture, so that the log says so.
            let _ = writeln!(
                std::io::stderr(),
                "skipped {}: no window-server session, so AppKit has no cursor",
                module_path!()
            );
            return;
        }
        let shown = cursor.shape().unwrap().expect("the system shows a pointer");
        assert!(shown.rgba().chunks(4).any(|pixel| pixel[3] == 255));
        assert!(rendered.len() >= 5, "{} rendered", rendered.len());
        for (kind, shape) in &rendered {
            assert!(shape.width().max(shape.height()) <= CursorShape::MAX_SIDE);
            assert!(
                shape.rgba().chunks(4).any(|pixel| pixel[3] == 255),
                "{kind:?} has no opaque pixel"
            );
        }
        let (kind, arrow) = &rendered[0];
        assert_eq!(autoreleasepool(|_| cursor.kind(arrow, 2.0)), *kind);
        let corner = (arrow.width() - 1, arrow.height() - 1);
        let moved = CursorShape::new(
            CursorKind::Custom,
            arrow.width(),
            arrow.height(),
            corner,
            arrow.rgba().to_vec(),
        )
        .unwrap();
        assert_eq!(cursor.kind(&moved, 2.0), CursorKind::Custom);
    }
}
