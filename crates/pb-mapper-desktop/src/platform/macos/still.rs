//! Stills for a viewer with no decoder: ScreenCaptureKit's screenshot
//! (macOS 14) fitted inside 1600x1000, as a quality-65 JPEG from ImageIO,
//! as Windows sends. Below macOS 14 there is none.

use super::{capture, cf};
use crate::{
    BackendError, Display, FailureKind, Result, error, platform::StillCapture, qos::scaled_size,
};
use block2::RcBlock;
use objc2::{Message, rc::Retained, runtime::AnyClass};
use objc2_core_foundation::{CFMutableData, CFRetained, CFString};
use objc2_core_graphics::CGImage;
use objc2_foundation::NSError;
use objc2_image_io::{CGImageDestination, kCGImageDestinationLossyCompressionQuality};
use objc2_screen_capture_kit::{SCContentFilter, SCScreenshotManager};
use std::{sync::mpsc, time::Duration};

const MAX_SIZE: (u32, u32) = (1600, 1000);
const QUALITY: f64 = 0.65;
const WAIT: Duration = Duration::from_secs(5);

/// The filter of the display last captured, kept between stills.
#[derive(Default)]
pub(crate) struct SckStill {
    filter: Option<(u32, Retained<SCContentFilter>)>,
}

/// An image the screenshot handler hands back.
struct Handed(CFRetained<CGImage>);
// SAFETY: a CGImage is immutable and thread-safe.
unsafe impl Send for Handed {}

impl SckStill {
    fn filter(&mut self, id: u32) -> Result<Retained<SCContentFilter>> {
        if let Some((cached, filter)) = &self.filter
            && *cached == id
        {
            return Ok(filter.clone());
        }
        let filter = capture::filter(&*capture::display(id)?);
        self.filter = Some((id, filter.clone()));
        Ok(filter)
    }
}

impl StillCapture for SckStill {
    /// In an autorelease pool of its own, which thread C lacks.
    fn jpeg(&mut self, display: &Display) -> Result<Vec<u8>> {
        objc2::rc::autoreleasepool(|_| self.screenshot(display))
    }
}

impl SckStill {
    fn screenshot(&mut self, display: &Display) -> Result<Vec<u8>> {
        if AnyClass::get(c"SCScreenshotManager").is_none() {
            return Err(BackendError::new(
                FailureKind::Unsupported,
                "still capture needs macOS 14 or later",
            )
            .into());
        }
        let filter = self.filter(display.id)?;
        let (width, height) = scaled_size(display.width, display.height, MAX_SIZE.0, MAX_SIZE.1);
        let config = capture::configuration(width, height, false);
        let (sender, receiver) = mpsc::sync_channel(1);
        let handler = RcBlock::new(move |image: *mut CGImage, failure: *mut NSError| {
            let image = std::ptr::NonNull::new(image).map(|image| {
                // SAFETY: valid during the call; retained to outlive it.
                Handed(unsafe { CFRetained::retain(image) })
            });
            let _ = sender.send(image.ok_or_else(|| {
                // SAFETY: null or valid during the call.
                let message = unsafe { failure.as_ref() }
                    .map(|failure| failure.retain().localizedDescription().to_string())
                    .unwrap_or_default();
                format!("the screenshot failed: {message}")
            }));
        });
        // SAFETY: the handler outlives the call; ScreenCaptureKit copies it.
        unsafe {
            SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
                &filter,
                &config,
                Some(&handler),
            );
        }
        let image = match receiver.recv_timeout(WAIT) {
            Ok(Ok(Handed(image))) => image,
            Ok(Err(message)) => {
                self.filter = None;
                return Err(BackendError::new(FailureKind::CaptureFailed, message).into());
            }
            Err(_) => return Err(error("the screenshot did not arrive in time")),
        };
        jpeg(&image)
    }
}

/// `image` as a JPEG.
fn jpeg(image: &CGImage) -> Result<Vec<u8>> {
    let data = CFMutableData::new(None, 0).ok_or_else(|| error("no buffer for the JPEG"))?;
    let kind = CFString::from_static_str("public.jpeg");
    let quality = cf::float(QUALITY);
    // SAFETY: a static key.
    let options = cf::dictionary(&[(
        unsafe { kCGImageDestinationLossyCompressionQuality },
        &quality,
    )]);
    // SAFETY: a mutable data, a type identifier and no options.
    let destination = unsafe { CGImageDestination::with_data(&data, &kind, 1, None) }
        .ok_or_else(|| error("ImageIO cannot write JPEG"))?;
    // SAFETY: one image, as the destination was made for.
    let written = unsafe {
        destination.add_image(image, Some(&options));
        destination.finalize()
    };
    if !written {
        return Err(error("ImageIO could not encode the JPEG"));
    }
    Ok(data.to_vec())
}
