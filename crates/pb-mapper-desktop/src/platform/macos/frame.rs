//! What a Mac host's session codes, and what its viewers see of it.
//! VideoToolbox encodes a picture inside its encode call where the
//! picture's buffer covers a multiple of 16 both ways, by its own size or
//! with the pixels it says it extends by; any other it encodes in the
//! background, at a longer dwell (`video_toolbox_padded` measures the
//! three). A padded frame is coded at the content's size rounded up to 16,
//! the content in its top left and the rest black, so that its pictures
//! cover a multiple of 16 whatever ScreenCaptureKit allocates at the
//! content's own size, and its sequence parameter set crops the rest away,
//! byte for byte as VideoToolbox's own SPS at the content's size does:
//! every decoder shows the content alone. Where ScreenCaptureKit's own
//! pictures of that size already extend to a multiple of 16
//! (`macos_capture_layout`, `sck_padded_capture`), padding gains nothing.

/// The multiple VideoToolbox's fast path wants of both dimensions: H.264's
/// macroblock.
pub(crate) const ALIGN: u32 = 16;

/// A session's coded size and the content in its top left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    /// What the capture fills and the encoder codes.
    pub coded: (u32, u32),
    /// What the viewers see: `VideoFormat`'s size, and the SPS's after its
    /// cropping. Never larger than `coded`.
    pub content: (u32, u32),
}

impl Frame {
    /// Coded at `width` x `height` itself.
    pub(crate) fn exact(width: u32, height: u32) -> Self {
        Self {
            coded: (width, height),
            content: (width, height),
        }
    }

    /// `width` x `height` coded at each rounded up to `ALIGN`.
    pub(crate) fn padded(width: u32, height: u32) -> Self {
        let up = |side: u32| side.div_ceil(ALIGN).saturating_mul(ALIGN);
        Self {
            coded: (up(width), up(height)),
            content: (width, height),
        }
    }

    /// Whether the SPS must crop the coded picture to the content.
    pub(crate) fn cropped(self) -> bool {
        self.coded != self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The commonest Mac hosts at Native, and a scaled size: each side is
    /// padded to the next multiple of 16, and a size already aligned is
    /// coded as it is, with nothing to crop.
    #[test]
    fn a_padded_frame_codes_the_next_multiple_of_16() {
        for ((width, height), coded) in [
            // A 14-inch MacBook Pro.
            ((3024, 1964), (3024, 1968)),
            // A 13-inch MacBook Air.
            ((2940, 1912), (2944, 1920)),
            ((1920, 1080), (1920, 1088)),
            ((3340, 2160), (3344, 2160)),
            ((2, 2), (16, 16)),
        ] {
            let frame = Frame::padded(width, height);
            assert_eq!(frame.coded, coded, "{width}x{height}");
            assert_eq!(frame.content, (width, height));
            assert!(frame.cropped());
        }
        for (width, height) in [(2560, 1440), (3840, 2160), (1920, 1200)] {
            let frame = Frame::padded(width, height);
            assert_eq!(frame, Frame::exact(width, height));
            assert!(!frame.cropped());
        }
    }
}
