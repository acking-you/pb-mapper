//! ScreenCaptureKit: one display's stream, scaled by the system to the
//! encoder's size, in NV12 (BT.709 video range, sRGB primaries), without the
//! pointer. Its queue keeps only the newest complete picture, an
//! IOSurface-backed pixel buffer the encoder reads in place; thread C takes
//! it at its own pace. A stream the system stops says why at the next take.

use super::cf;
use crate::{BackendError, FailureKind, Result, error};
use block2::RcBlock;
use dispatch2::{DispatchQoS, DispatchQueue, DispatchRetained, GlobalQueueIdentifier};
use objc2::{
    AnyThread, DefinedClass, Message, define_class, msg_send,
    rc::Retained,
    runtime::{AnyClass, NSObject, ProtocolObject},
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString};
use objc2_core_graphics::{
    CGMainDisplayID, kCGColorSpaceSRGB, kCGDisplayStreamYCbCrMatrix_ITU_R_709_2,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBuffer, kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_foundation::{
    NSArray, NSError, NSObjectProtocol, NSOperatingSystemVersion, NSProcessInfo,
};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType, SCWindow,
};
use parking_lot::Mutex;
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

/// The longest an asynchronous ScreenCaptureKit call is waited for.
const WAIT: Duration = Duration::from_secs(5);
/// Pictures the stream keeps in flight: the newest held here, those the
/// encoder holds (`ENCODER_DEPTH`), one being delivered.
const QUEUE_DEPTH: isize = 6;
const _: () = assert!(
    QUEUE_DEPTH as usize >= crate::worker::ENCODER_DEPTH + 2,
    "the stream never runs out of surfaces while the encoder holds its share"
);
/// `SCStreamErrorUserDeclined`, `SCStreamErrorUserStopped`.
const USER_DECLINED: isize = -3801;
const USER_STOPPED: isize = -3817;

/// macOS 12.3 or later, where ScreenCaptureKit is: the library links it
/// weakly, so below that its classes are missing and nothing calls them.
pub(crate) fn available() -> bool {
    let version = NSOperatingSystemVersion {
        majorVersion: 12,
        minorVersion: 3,
        patchVersion: 0,
    };
    // Off the main thread, with no pool of its own: the call may autorelease.
    objc2::rc::autoreleasepool(|_| {
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(version)
            && AnyClass::get(c"SCStream").is_some()
    })
}

/// A picture the stream delivered.
pub(crate) struct Picture(pub CFRetained<CVPixelBuffer>);
// SAFETY: a pixel buffer is reference counted atomically; the stream wrote
// it before delivering it and never writes it again while it is held.
unsafe impl Send for Picture {}

/// Why the system stopped a stream.
#[derive(Clone, Debug)]
struct Stopped {
    code: isize,
    message: String,
}

#[derive(Default)]
pub(crate) struct Latest {
    pub picture: Option<Picture>,
    /// Not taken since it arrived.
    pub fresh: bool,
    /// Pictures delivered.
    pub delivered: u64,
    stopped: Option<Stopped>,
}

#[derive(Default)]
pub(crate) struct Shared {
    latest: Mutex<Latest>,
}

/// Whether `sample` holds a new picture: its frame status is complete. An
/// idle frame (nothing changed) and the others hold none.
fn complete(sample: &CMSampleBuffer) -> bool {
    // SAFETY: the stream's sample, read only.
    let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else {
        return false;
    };
    // SAFETY: a sample's attachments are an array of dictionaries.
    let attachments = unsafe { CFRetained::cast_unchecked::<CFArray<CFDictionary>>(attachments) };
    let Some(info) = attachments.get(0) else {
        return false;
    };
    // SAFETY: a ScreenCaptureKit constant, which exists wherever its classes
    // do; an NSString is the CFString it is toll-free bridged to.
    let key: &CFString = unsafe { &*(SCStreamFrameInfoStatus as *const _ as *const CFString) };
    cf::value::<CFNumber>(&info, key)
        .and_then(|status| status.as_i64())
        .is_some_and(|status| status == SCFrameStatus::Complete.0 as i64)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class has
    // no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "PbMapperDesktopCaptureOutput"]
    #[ivars = Arc<Shared>]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        /// On the stream's queue: keep the newest complete picture.
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen || !complete(sample) {
                return;
            }
            // SAFETY: the stream's sample, read only.
            let Some(image) = (unsafe { sample.image_buffer() }) else {
                return;
            };
            let mut latest = self.ivars().latest.lock();
            latest.picture = Some(Picture(image));
            latest.fresh = true;
            latest.delivered += 1;
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            self.ivars().latest.lock().stopped = Some(Stopped {
                code: error.code(),
                message: error.localizedDescription().to_string(),
            });
        }
    }
);

impl Output {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(shared);
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

/// What a completion handler hands back across threads.
struct Handed<T>(T);
// SAFETY: ScreenCaptureKit's results are immutable once handed over, and
// are only read on the thread that waits for them.
unsafe impl<T> Send for Handed<T> {}

/// A ScreenCaptureKit error as a failure: the owner refusing or stopping
/// the capture says so.
fn failure(code: isize, message: &str) -> std::io::Error {
    match code {
        USER_DECLINED => super::platform::screen_capture_missing(),
        USER_STOPPED => BackendError::new(
            FailureKind::ConsentRevoked,
            format!("the owner stopped screen sharing: {message}"),
        )
        .into(),
        _ => BackendError::new(
            FailureKind::CaptureFailed,
            format!("ScreenCaptureKit failed ({code}): {message}"),
        )
        .into(),
    }
}

fn ns_failure(error: *mut NSError) -> std::io::Error {
    // SAFETY: a completion handler's error, null or valid during the call.
    match unsafe { error.as_ref() } {
        Some(error) => failure(error.code(), &error.localizedDescription().to_string()),
        None => error_without_cause(),
    }
}

fn error_without_cause() -> std::io::Error {
    error("ScreenCaptureKit failed without an error")
}

/// Wait for a completion handler's answer.
fn answer<T>(receiver: &mpsc::Receiver<Result<Handed<T>>>, what: &str) -> Result<T> {
    match receiver.recv_timeout(WAIT) {
        Ok(answer) => answer.map(|handed| handed.0),
        Err(_) => Err(BackendError::new(
            FailureKind::CaptureFailed,
            format!("ScreenCaptureKit did not answer {what} in time"),
        )
        .into()),
    }
}

/// ScreenCaptureKit's display of id `id`, which a filter needs.
pub(crate) fn display(id: u32) -> Result<Retained<SCDisplay>> {
    // ScreenCaptureKit asserts the window server connection is up; a Core
    // Graphics call makes sure it is.
    CGMainDisplayID();
    let (sender, receiver) = mpsc::sync_channel(1);
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, failure: *mut NSError| {
            // SAFETY: null or valid during the call.
            let content = unsafe { content.as_ref() }.map(|content| Handed(content.retain()));
            let _ = sender.send(content.ok_or_else(|| ns_failure(failure)));
        },
    );
    // SAFETY: the handler outlives the call; ScreenCaptureKit copies it.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    let content = answer(&receiver, "with the shareable displays")?;
    // SAFETY: the content's displays, read on this thread.
    let displays = unsafe { content.displays() };
    displays
        .to_vec()
        .into_iter()
        // SAFETY: a display's id, read only.
        .find(|display| unsafe { display.displayID() } == id)
        .ok_or_else(|| {
            BackendError::new(
                FailureKind::DisplayLost,
                "ScreenCaptureKit does not list the selected display",
            )
            .into()
        })
}

/// A filter of all of `display`.
pub(crate) fn filter(display: &SCDisplay) -> Retained<SCContentFilter> {
    let none: Retained<NSArray<SCWindow>> = NSArray::new();
    // SAFETY: a display and an empty window list.
    unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), display, &none)
    }
}

/// The configuration of a picture `width` x `height` pixels in sRGB,
/// without the pointer: NV12 (BT.709 video range) for the encoder, else
/// BGRA, as an image takes it.
pub(crate) fn configuration(
    width: u32,
    height: u32,
    nv12: bool,
) -> Retained<SCStreamConfiguration> {
    // SAFETY: a fresh configuration, set before any stream reads it; the
    // constants are Core Graphics', alive for the process.
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(width as usize);
        config.setHeight(height as usize);
        config.setShowsCursor(false);
        config.setColorSpaceName(kCGColorSpaceSRGB);
        if nv12 {
            config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
            config.setColorMatrix(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2);
        } else {
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
        }
        config
    }
}

/// One display's running stream.
pub(crate) struct Stream {
    /// The stream, its output and the output's queue, until it stops.
    running: Option<Running>,
    shared: Arc<Shared>,
}

/// The output receives the stream's pictures on the queue for as long as
/// they live.
struct Running {
    stream: Retained<SCStream>,
    output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
}

/// A stream's parts, handed to ScreenCaptureKit's completion queue to be
/// dropped there once the stream stopped.
struct Released {
    _running: Running,
}
// SAFETY: the stream, its output and their queue are Objective-C and
// dispatch objects, retained and released atomically from any thread; once
// handed over, nothing but the completion handler touches them, and it only
// drops them.
unsafe impl Send for Released {}

impl Stream {
    /// Display `id`'s stream at `width` x `height`, at most `fps` pictures a
    /// second, started. Thread C has no autorelease pool: this brings its
    /// own.
    pub(crate) fn start(id: u32, width: u32, height: u32, fps: u32) -> Result<Self> {
        objc2::rc::autoreleasepool(|_| Self::started(id, width, height, fps))
    }

    fn started(id: u32, width: u32, height: u32, fps: u32) -> Result<Self> {
        let display = display(id)?;
        let filter = filter(&display);
        let config = configuration(width, height, true);
        // SAFETY: a fresh configuration, before the stream reads it.
        unsafe {
            config.setMinimumFrameInterval(CMTime::new(1, fps.max(1) as i32));
            config.setQueueDepth(QUEUE_DEPTH);
        }
        let shared = Arc::new(Shared::default());
        let output = Output::new(shared.clone());
        let delegate = ProtocolObject::<dyn SCStreamDelegate>::from_ref(&*output);
        // SAFETY: a filter, a configuration and a delegate that lives as
        // long as the stream.
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                Some(delegate),
            )
        };
        let target = DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(
            DispatchQoS::UserInteractive,
        ));
        let queue =
            DispatchQueue::new_with_target("pb-mapper.desktop.capture", None, Some(&target));
        let receiver = ProtocolObject::<dyn SCStreamOutput>::from_ref(&*output);
        // SAFETY: the output and its serial queue live as long as the stream.
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                receiver,
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|failure| ns_failure(Retained::as_ptr(&failure).cast_mut()))?;
        let (sender, started) = mpsc::sync_channel(1);
        let handler = RcBlock::new(move |failure: *mut NSError| {
            let result = if failure.is_null() {
                Ok(Handed(()))
            } else {
                Err(ns_failure(failure))
            };
            let _ = sender.send(result);
        });
        // SAFETY: the handler outlives the call; ScreenCaptureKit copies it.
        unsafe { stream.startCaptureWithCompletionHandler(Some(&handler)) };
        // Stopped as it drops, should the start fail or time out.
        let this = Self {
            running: Some(Running {
                stream,
                output,
                _queue: queue,
            }),
            shared,
        };
        answer(&started, "the start of capture")?;
        Ok(this)
    }

    /// Fails once the system stopped the stream.
    pub(crate) fn check(&self) -> Result<()> {
        match &self.shared.latest.lock().stopped {
            Some(stopped) => Err(failure(stopped.code, &stopped.message)),
            None => Ok(()),
        }
    }

    /// The newest picture and whether it is new since the last take; it
    /// stays for a repeat.
    pub(crate) fn take(&self) -> Option<(CFRetained<CVPixelBuffer>, bool)> {
        let mut latest = self.shared.latest.lock();
        let fresh = std::mem::take(&mut latest.fresh);
        latest
            .picture
            .as_ref()
            .map(|picture| (picture.0.clone(), fresh))
    }

    /// Whether any picture arrived.
    pub(crate) fn delivered(&self) -> bool {
        self.shared.latest.lock().delivered > 0
    }
}

impl Drop for Stream {
    /// Detaches the output, so that no picture arrives after this, then
    /// stops without waiting. The stream, its output and their queue live
    /// until the stop is done, on ScreenCaptureKit's completion queue.
    fn drop(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        let receiver = ProtocolObject::<dyn SCStreamOutput>::from_ref(&*running.output);
        // SAFETY: the output this stream was given, removed once. A stream
        // that never started may refuse; then nothing is delivered anyway.
        let _ = unsafe {
            running
                .stream
                .removeStreamOutput_type_error(receiver, SCStreamOutputType::Screen)
        };
        let stream = running.stream.clone();
        let held = Mutex::new(Some(Released { _running: running }));
        let stopped = RcBlock::new(move |_: *mut NSError| drop(held.lock().take()));
        // SAFETY: stopping a stream is allowed at any time; the handler,
        // which only drops what it holds under a lock, outlives the call.
        unsafe { stream.stopCaptureWithCompletionHandler(Some(&stopped)) };
    }
}
