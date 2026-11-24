//! How a captured picture becomes the encoded one, independent of the
//! platform that captures it: the quarter turn that stands it upright, and
//! whether its encoded size is a crop of it or a scaling. The Windows host
//! sets its video processor up from these; they are pure, and tested on
//! every platform.

/// The quarter turn, clockwise, that stands a captured picture upright: a
/// rotated display scans out its desktop turned, and its capture delivers
/// the picture as scanned out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rotation {
    /// Captured upright.
    Identity,
    /// Turned 90° clockwise: the captured top row becomes the right column.
    Cw90,
    Cw180,
    /// Turned 270° clockwise: the captured top row becomes the left column.
    Cw270,
}

impl Rotation {
    /// The upright size of a captured `(width, height)` picture.
    pub(crate) fn upright(self, (width, height): (u32, u32)) -> (u32, u32) {
        match self {
            Self::Identity | Self::Cw180 => (width, height),
            Self::Cw90 | Self::Cw270 => (height, width),
        }
    }

    /// The size a display's pictures are captured at, as scanned out, from
    /// the size its display `mode` is given at and its upright `desktop`'s.
    /// A quarter-turned display's mode may be given either way round: it is
    /// taken as scanned out only where that reading alone matches the
    /// desktop, and as upright otherwise, as Windows' Desktop Duplication is
    /// read elsewhere (WebRTC's capturer requires the mode to equal the
    /// output's desktop rectangle, and its pictures to be that size turned
    /// back). A desktop that matches neither reading, or none read, leaves
    /// the mode upright.
    pub(crate) fn scanned_out(self, mode: (u32, u32), desktop: (u32, u32)) -> (u32, u32) {
        // A quarter turn's swap is its own inverse: the upright size of an
        // upright mode is the size it is scanned out at.
        let turned = self.upright(mode);
        if mode != desktop && turned == desktop {
            mode
        } else {
            turned
        }
    }

    /// Where captured pixel `(x, y)` of a `size` picture lands upright: the
    /// turn itself, written out apart from `source_rect`, which the tests
    /// check against it, and the Windows video processor too.
    #[cfg(test)]
    pub(crate) fn upright_point(
        self,
        (x, y): (u32, u32),
        (width, height): (u32, u32),
    ) -> (u32, u32) {
        match self {
            Self::Identity => (x, y),
            Self::Cw90 => (height - 1 - y, x),
            Self::Cw180 => (width - 1 - x, height - 1 - y),
            Self::Cw270 => (y, width - 1 - x),
        }
    }

    /// The region of a captured `input` picture that becomes the top-left
    /// `kept` (width, height) of the upright picture, as `[left, top, right,
    /// bottom]` in the captured picture: what is left out is the upright
    /// picture's right columns and bottom rows, whichever edges of the
    /// captured picture they come from. `kept` is at most the upright size;
    /// the whole upright size keeps the whole picture.
    pub(crate) fn source_rect(self, input: (u32, u32), kept: (u32, u32)) -> [u32; 4] {
        let (width, height) = input;
        let (upright_width, upright_height) = self.upright(input);
        let (kept_width, kept_height) = (kept.0.min(upright_width), kept.1.min(upright_height));
        match self {
            Self::Identity => [0, 0, kept_width, kept_height],
            // Upright (x, y) is captured (y, height - 1 - x).
            Self::Cw90 => [0, height - kept_width, kept_height, height],
            // Upright (x, y) is captured (width - 1 - x, height - 1 - y).
            Self::Cw180 => [width - kept_width, height - kept_height, width, height],
            // Upright (x, y) is captured (width - 1 - y, x).
            Self::Cw270 => [width - kept_height, 0, width, kept_width],
        }
    }
}

/// Whether an `input` picture becomes `output` by cropping alone: each of
/// its dimensions equal to the output's or one pixel larger, as rounding an
/// odd size down to the even one NV12 needs leaves it. Scaling it instead,
/// by a factor a hair under 1, would resample every pixel at a drifting
/// phase and blur text across the whole picture. Both sizes are upright.
pub(crate) fn crop_only(input: (u32, u32), output: (u32, u32)) -> bool {
    (output.0..=output.0.saturating_add(1)).contains(&input.0)
        && (output.1..=output.1.saturating_add(1)).contains(&input.1)
}

/// Whether making `output` of `input` shrinks the picture: either dimension
/// smaller, beyond the edge pixel a crop drops (`crop_only`). Both sizes are
/// upright.
pub(crate) fn downscales(input: (u32, u32), output: (u32, u32)) -> bool {
    !crop_only(input, output) && (output.0 < input.0 || output.1 < input.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROTATIONS: [Rotation; 4] = [
        Rotation::Identity,
        Rotation::Cw90,
        Rotation::Cw180,
        Rotation::Cw270,
    ];

    /// A quarter-turned display's pictures are captured as scanned out,
    /// its mode's size swapped: the mode is read upright, unless only the
    /// other reading matches the desktop. Half a turn and none keep the
    /// mode's size whatever the desktop says.
    #[test]
    fn a_turned_display_is_captured_at_its_mode_turned_back() {
        let portrait = (1080, 1920);
        let landscape = (1920, 1080);
        for rotation in [Rotation::Cw90, Rotation::Cw270] {
            // The mode upright, the desktop's own size, as WebRTC reads it.
            assert_eq!(rotation.scanned_out(portrait, portrait), landscape);
            // The mode as scanned out, which only the desktop tells apart.
            assert_eq!(rotation.scanned_out(landscape, portrait), landscape);
            // No desktop to tell by: the mode is taken upright.
            assert_eq!(rotation.scanned_out(portrait, (0, 0)), landscape);
            assert_eq!(rotation.scanned_out(portrait, (2160, 3840)), landscape);
            // A square mode reads the same either way.
            assert_eq!(
                rotation.scanned_out((1200, 1200), (1200, 1200)),
                (1200, 1200)
            );
            assert_eq!(
                rotation.upright(rotation.scanned_out(portrait, portrait)),
                portrait
            );
        }
        for rotation in [Rotation::Identity, Rotation::Cw180] {
            assert_eq!(rotation.scanned_out(landscape, landscape), landscape);
            assert_eq!(rotation.scanned_out(landscape, portrait), landscape);
            assert_eq!(rotation.scanned_out(landscape, (0, 0)), landscape);
        }
    }

    /// An odd edge that needs no scaling is cropped, never resampled: one
    /// pixel off either dimension or both, and nothing off.
    #[test]
    fn an_odd_edge_is_cropped_and_anything_more_is_scaled() {
        // 1365×767 under 1080p bounds encodes at 1364×766.
        assert!(crop_only((1365, 767), (1364, 766)));
        assert!(crop_only((1365, 768), (1364, 768)));
        assert!(crop_only((1920, 1080), (1920, 1080)));
        // Two pixels, a real downscale, and an upscale all scale.
        assert!(!crop_only((1366, 768), (1364, 768)));
        assert!(!crop_only((2560, 1440), (1920, 1080)));
        assert!(!crop_only((1280, 720), (1920, 1080)));
        assert!(!crop_only((1921, 1080), (1920, 1082)));
        assert!(!crop_only((u32::MAX, 2), (u32::MAX - 2, 2)));
        assert!(crop_only((u32::MAX, 2), (u32::MAX, 2)));

        assert!(downscales((2560, 1440), (1920, 1080)));
        assert!(downscales((3440, 1440), (1920, 802)));
        assert!(!downscales((1365, 767), (1364, 766)));
        assert!(!downscales((1920, 1080), (1920, 1080)));
        assert!(!downscales((1280, 720), (1920, 1080)));
    }

    /// The source region is exactly the captured pixels that land in the
    /// kept top-left of the upright picture, so a crop drops its right
    /// column and bottom row whatever the turn, and keeps upright pixel
    /// (x, y) at the display's (x, y). The whole upright size is the whole
    /// picture.
    #[test]
    fn the_source_region_is_what_lands_in_the_kept_upright_corner() {
        for rotation in ROTATIONS {
            for input in [(5, 3), (4, 7), (6, 6)] {
                let (upright_width, upright_height) = rotation.upright(input);
                for kept in [
                    (upright_width, upright_height),
                    (upright_width - 1, upright_height),
                    (upright_width, upright_height - 1),
                    (upright_width - 1, upright_height - 1),
                ] {
                    let [left, top, right, bottom] = rotation.source_rect(input, kept);
                    assert!(left <= right && right <= input.0, "{rotation:?} {input:?}");
                    assert!(top <= bottom && bottom <= input.1, "{rotation:?} {input:?}");
                    for y in 0..input.1 {
                        for x in 0..input.0 {
                            let (ux, uy) = rotation.upright_point((x, y), input);
                            assert!(ux < upright_width && uy < upright_height);
                            let inside = (left..right).contains(&x) && (top..bottom).contains(&y);
                            let kept_pixel = ux < kept.0 && uy < kept.1;
                            assert_eq!(
                                inside, kept_pixel,
                                "{rotation:?} {input:?} keeping {kept:?}: captured ({x}, {y}) lands at ({ux}, {uy})"
                            );
                        }
                    }
                }
                assert_eq!(
                    rotation.source_rect(input, rotation.upright(input)),
                    [0, 0, input.0, input.1]
                );
            }
        }
    }
}
