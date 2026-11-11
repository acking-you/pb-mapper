//! Points and pixels. macOS places displays in one global space of points,
//! top left at the main display's origin, each display at its own backing
//! scale; the host speaks physical pixels. A display's pixel origin is its
//! point origin at its own scale, so coordinates are only ever converted on
//! the display a session is bound to: never across two displays, whose
//! scales may differ.

use crate::Display;

/// One display's place in points and its size in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    /// Top left, in global points.
    pub origin: (f64, f64),
    /// Size in points.
    pub size: (f64, f64),
    /// Pixels per point: the current mode's pixel width over its width.
    pub scale: f64,
}

impl Geometry {
    /// A display at `bounds` (x, y, width, height in global points) whose
    /// mode is `pixel_width` pixels and `point_width` points wide. A mode
    /// that reports no width counts as scale 1.
    pub(crate) fn new(
        bounds: (f64, f64, f64, f64),
        pixel_width: usize,
        point_width: usize,
    ) -> Self {
        let scale = if point_width == 0 || pixel_width == 0 {
            1.0
        } else {
            pixel_width as f64 / point_width as f64
        };
        Self {
            origin: (bounds.0, bounds.1),
            size: (bounds.2, bounds.3),
            scale,
        }
    }

    /// The pixel rectangle: origin and size, rounded.
    pub(crate) fn pixels(&self) -> (i32, i32, u32, u32) {
        let at = |points: f64| (points * self.scale).round() as i32;
        let side = |points: f64| (points * self.scale).round().max(0.0) as u32;
        (
            at(self.origin.0),
            at(self.origin.1),
            side(self.size.0),
            side(self.size.1),
        )
    }

    /// `name` and `id` with this geometry, in pixels.
    pub(crate) fn display(&self, id: u32, name: String) -> Display {
        let (x, y, width, height) = self.pixels();
        Display {
            id,
            name,
            x,
            y,
            width,
            height,
        }
    }

    /// The global point at the centre of pixel `(x, y)`, absolute physical
    /// pixels as the injector gets them: this display's pixel origin plus
    /// an offset inside it.
    pub(crate) fn point(&self, (x, y): (i32, i32)) -> (f64, f64) {
        let (left, top, _, _) = self.pixels();
        let offset =
            |pixel: i32, origin: i32| (f64::from(pixel) - f64::from(origin) + 0.5) / self.scale;
        (
            self.origin.0 + offset(x, left),
            self.origin.1 + offset(y, top),
        )
    }

    /// The absolute pixel under global point `(x, y)`, or `None` when the
    /// point is on another display.
    pub(crate) fn pixel(&self, (x, y): (f64, f64)) -> Option<(i32, i32)> {
        let (dx, dy) = (x - self.origin.0, y - self.origin.1);
        if !(0.0..self.size.0).contains(&dx) || !(0.0..self.size.1).contains(&dy) {
            return None;
        }
        let (left, top, width, height) = self.pixels();
        // At the far edge, a rounded size can be a pixel short of the point.
        let clamp = |offset: f64, side: u32| {
            (offset * self.scale)
                .floor()
                .min(f64::from(side.max(1) - 1)) as i32
        };
        Some((left + clamp(dx, width), top + clamp(dy, height)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Retina display at the origin, 1512x982 points at 2x: 3024x1964
    /// pixels, and a pixel's centre maps back to that pixel.
    #[test]
    fn a_retina_display_is_twice_its_points_and_round_trips() {
        let retina = Geometry::new((0.0, 0.0, 1512.0, 982.0), 3024, 1512);
        assert_eq!(retina.pixels(), (0, 0, 3024, 1964));
        for pixel in [(0, 0), (1, 1), (3023, 1963), (1511, 700)] {
            assert_eq!(retina.pixel(retina.point(pixel)), Some(pixel));
        }
        assert_eq!(retina.point((1, 0)), (0.75, 0.25));
        assert_eq!(retina.pixel((1512.0, 10.0)), None);
    }

    /// A 1x display left of the main one has a negative pixel origin at its
    /// own scale; a point on another display is not on it.
    #[test]
    fn a_secondary_display_keeps_its_own_scale_and_origin() {
        let left = Geometry::new((-1920.0, -200.0, 1920.0, 1080.0), 1920, 1920);
        let display = left.display(7, "LEFT".into());
        assert_eq!(
            (display.x, display.y, display.width, display.height),
            (-1920, -200, 1920, 1080)
        );
        assert_eq!(left.point((-1920, -200)), (-1919.5, -199.5));
        assert_eq!(left.pixel((-1.0, 879.0)), Some((-1, 879)));
        assert_eq!(left.pixel((0.0, 0.0)), None);
        // A display with no mode width counts as 1x.
        assert_eq!(Geometry::new((0.0, 0.0, 10.0, 10.0), 0, 0).scale, 1.0);
    }
}
