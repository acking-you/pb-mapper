//! A pointer image as Core Graphics renders it (premultiplied RGBA, in
//! points) turned into the shape the host sends (straight RGBA, in pixels).

/// `rgba`, premultiplied, made straight in place: each colour divided by
/// its pixel's alpha, rounded; a clear pixel stays all zero.
pub(crate) fn unpremultiply(rgba: &mut [u8]) {
    for [red, green, blue, alpha] in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(*alpha);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in [red, green, blue] {
            let straight = (u32::from(*channel) * 255 + alpha / 2) / alpha;
            *channel = u8::try_from(straight).unwrap_or(u8::MAX);
        }
    }
}

/// A pointer's pixel size and hotspot: `size` and `hotspot` in points,
/// from the top left, at `scale` pixels a point. The size is at least one
/// pixel; the hotspot lands inside it.
pub(crate) fn in_pixels(
    size: (f64, f64),
    hotspot: (f64, f64),
    scale: f64,
) -> ((u32, u32), (u32, u32)) {
    let side = |points: f64| (points * scale).round().clamp(1.0, f64::from(u16::MAX)) as u32;
    let (width, height) = (side(size.0), side(size.1));
    let at = |points: f64, side: u32| ((points * scale).floor().max(0.0) as u32).min(side - 1);
    (
        (width, height),
        (at(hotspot.0, width), at(hotspot.1, height)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opaque and clear pixels keep their bytes; a half-transparent one is
    /// divided by its alpha.
    #[test]
    fn premultiplied_colours_come_back_straight() {
        let mut rgba = [10, 20, 30, 255, 0, 0, 0, 0, 64, 32, 0, 128];
        unpremultiply(&mut rgba);
        assert_eq!(rgba, [10, 20, 30, 255, 0, 0, 0, 0, 128, 64, 0, 128]);
    }

    /// A 2x pointer is twice its points, its hotspot too, and a hotspot on
    /// the far edge stays inside.
    #[test]
    fn a_retina_pointer_is_measured_in_pixels() {
        assert_eq!(in_pixels((17.0, 23.0), (4.5, 4.5), 2.0), ((34, 46), (9, 9)));
        assert_eq!(
            in_pixels((16.0, 16.0), (16.0, 16.0), 1.0),
            ((16, 16), (15, 15))
        );
        assert_eq!(in_pixels((0.0, 0.0), (0.0, 0.0), 2.0), ((1, 1), (0, 0)));
    }
}
