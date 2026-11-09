//! A JPEG still of a display, for viewers that decode nothing else: xcap's
//! capture, scaled into 1600x1000 at quality 65.
use super::desktop::display_lost;
use crate::{Display, Result, error, platform::StillCapture};

pub(crate) struct XcapJpeg;
impl StillCapture for XcapJpeg {
    fn jpeg(&mut self, display: &Display) -> Result<Vec<u8>> {
        let monitor = xcap::Monitor::all()
            .map_err(error)?
            .into_iter()
            .find(|m| m.id().ok() == Some(display.id))
            .ok_or_else(|| {
                display_lost(
                    "Selected display is no longer available; stop sharing and select a display",
                )
            })?;
        let image = monitor.capture_image().map_err(error)?;
        let image = image::DynamicImage::ImageRgba8(image);
        let rgb = image
            .resize(1600, 1000, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 65)
            .encode_image(&rgb)
            .map_err(error)?;
        Ok(jpeg)
    }
}
