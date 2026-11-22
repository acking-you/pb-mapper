//! ScreenCaptureKit: one display's stream, scaled by the system to the
//! encoder's size, in NV12 (sRGB through the BT.709 matrix, video range),
//! without the pointer, which the viewer draws. Its queue keeps only the
//! newest complete picture, an IOSurface-backed pixel buffer the encoder
//! reads in place; thread C takes it at its own pace. A stream the system
//! stops says why at the next take.
//!
//! What the display's picture does not cover takes the background colour,
//! clear unless set, and NV12 has no alpha to show it: the background is
//! opaque black, which the configuration does not retain.

use super::cf;
use crate::{BackendError, FailureKind, Result, Waker};
use block2::RcBlock;
use dispatch2::{DispatchQoS, DispatchQueue, DispatchRetained, GlobalQueueIdentifier};
use objc2::{
    AnyThread, ClassType, DefinedClass, Message, define_class, msg_send,
    rc::Retained,
    runtime::{AnyClass, NSObject, ProtocolObject},
    sel,
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString};
use objc2_core_graphics::{
    CGColor, CGMainDisplayID, kCGColorSpaceSRGB, kCGDisplayStreamYCbCrMatrix_ITU_R_709_2,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBuffer, kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_foundation::{
    NSArray, NSError, NSObjectProtocol, NSOperatingSystemVersion, NSProcessInfo,
};
use objc2_screen_capture_kit::{
    SCCaptureDynamicRange, SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamDelegate, SCStreamErrorCode, SCStreamErrorDomain,
    SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType, SCWindow,
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

#[derive(Default)]
pub(crate) struct Latest {
    pub picture: Option<Picture>,
    /// Not taken since it arrived.
    pub fresh: bool,
    stopped: Option<BackendError>,
}

pub(super) enum CaptureState {
    /// A fresh picture or a capture failure.
    Ready,
    /// The last picture is available for a repeat.
    Repeatable,
    /// No picture has arrived.
    Waiting,
}

pub(crate) struct Shared {
    latest: Mutex<Latest>,
    waker: Waker,
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
            let wake = {
                let mut latest = self.ivars().latest.lock();
                let wake = !latest.fresh;
                latest.picture = Some(Picture(image));
                latest.fresh = true;
                wake
            };
            if wake {
                self.ivars().waker.wake();
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            self.ivars().latest.lock().stopped = Some(failure(Some(error)));
            self.ivars().waker.wake();
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

/// An immutable snapshot retained by the shareable-content callback and
/// read by the thread that requested it.
struct ShareableContent(Retained<SCShareableContent>);
// SAFETY: the retained snapshot is read-only on the receiving thread.
unsafe impl Send for ShareableContent {}

/// A numeric error code identifies a ScreenCaptureKit failure only in its domain.
pub(super) fn failure(error: Option<&NSError>) -> BackendError {
    let Some(error) = error else {
        return BackendError::new(
            FailureKind::CaptureFailed,
            "ScreenCaptureKit returned no error",
        );
    };
    let domain = error.domain();
    // SAFETY: the framework's static error domain, used only after its availability check.
    if domain.isEqualToString(unsafe { SCStreamErrorDomain }) {
        match SCStreamErrorCode(error.code()) {
            SCStreamErrorCode::UserDeclined => return super::platform::screen_capture_missing(),
            SCStreamErrorCode::UserStopped => {
                return BackendError::new(
                    FailureKind::ConsentRevoked,
                    format!(
                        "the owner stopped screen sharing: {}",
                        error.localizedDescription()
                    ),
                );
            }
            _ => {}
        }
    }
    BackendError::new(
        FailureKind::CaptureFailed,
        format!(
            "ScreenCaptureKit failed ({domain}, {}): {}",
            error.code(),
            error.localizedDescription()
        ),
    )
}

/// Wait for one display, stream-start or screenshot completion.
pub(super) fn answer<T>(receiver: mpsc::Receiver<Result<T>>, what: &str) -> Result<T> {
    receiver.recv_timeout(WAIT).map_err(|reason| {
        let detail = match reason {
            mpsc::RecvTimeoutError::Timeout => "timed out",
            mpsc::RecvTimeoutError::Disconnected => "ended without a result",
        };
        BackendError::new(
            FailureKind::CaptureFailed,
            format!("ScreenCaptureKit {what} {detail}"),
        )
    })?
}

/// ScreenCaptureKit's display of id `id`, which a filter needs.
pub(crate) fn display(id: u32) -> Result<Retained<SCDisplay>> {
    // ScreenCaptureKit asserts the window server connection is up; a Core
    // Graphics call makes sure it is.
    CGMainDisplayID();
    let (sender, receiver) = mpsc::sync_channel(1);
    let handler = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        // SAFETY: null or valid during the call.
        let content = unsafe { content.as_ref() }.map(|content| ShareableContent(content.retain()));
        // SAFETY: the callback owns the optional error for this call.
        let _ = sender.send(content.ok_or_else(|| failure(unsafe { err.as_ref() }).into()));
    });
    // SAFETY: the handler outlives the call; ScreenCaptureKit copies it.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    drop(handler);
    let ShareableContent(content) = answer(receiver, "display enumeration")?;
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

/// A stream configuration and the background colour its property does not
/// retain. A stream holds the colour until the stream has stopped. A
/// screenshot's completion handler holds another retain ([`Self::background`]):
/// the manager keeps the configuration, but not this colour, and the wait
/// can return before the callback.
pub(crate) struct PreparedCapture {
    pub(crate) config: Retained<SCStreamConfiguration>,
    background: CFRetained<CGColor>,
}

impl PreparedCapture {
    /// Another retain of the background colour. The same object
    /// `config.backgroundColor` points at.
    pub(crate) fn background(&self) -> CFRetained<CGColor> {
        self.background.clone()
    }
}

/// The configuration of a picture `width` x `height` pixels in sRGB,
/// without the pointer, on an opaque black background: NV12 (BT.709
/// matrix, video range) for the encoder, else BGRA, as an image takes it.
pub(crate) fn configuration(width: u32, height: u32, nv12: bool) -> PreparedCapture {
    // SAFETY: a fresh configuration, set before any stream reads it; the
    // constants are Core Graphics', alive for the process.
    let config = unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(width as usize);
        config.setHeight(height as usize);
        config.setShowsCursor(false);
        // sRGB, not BT.709: every viewer converts with the BT.709 matrix and
        // shows the result as sRGB, which is also what the Windows host
        // encodes. Core Graphics' BT.709 space has a 2.4 gamma that would
        // lift dark greys (sRGB 26 of 255 becomes 37).
        config.setColorSpaceName(kCGColorSpaceSRGB);
        if nv12 {
            config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
            config.setColorMatrix(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2);
        } else {
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
        }
        // macOS 15. SDR is the default; set where the selector exists, so
        // that a later system cannot switch an XDR display to HDR under the
        // 8-bit stream.
        if SCStreamConfiguration::class().responds_to(sel!(setCaptureDynamicRange:)) {
            config.setCaptureDynamicRange(SCCaptureDynamicRange::SDR);
        }
        config
    };
    let background = CGColor::new_srgb(0.0, 0.0, 0.0, 1.0);
    // SAFETY: the property does not retain `background`; the returned value
    // does, and a caller that can stop waiting before the read keeps
    // another retain from `PreparedCapture::background`.
    unsafe { config.setBackgroundColor(&background) };
    PreparedCapture { config, background }
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
    /// `SCStreamConfiguration.backgroundColor` does not retain. Dropped only
    /// after the stream has stopped producing frames.
    _background: CFRetained<CGColor>,
}

/// A stream's parts, handed to ScreenCaptureKit's completion queue to be
/// dropped there once the stream stopped.
struct Released {
    _running: Running,
}
// SAFETY: the stream, its output, their queue and the background colour are
// Objective-C and Core Foundation objects, retained and released atomically
// from any thread; once handed over, nothing but the completion handler
// touches them, and it only drops them.
unsafe impl Send for Released {}

impl Stream {
    /// Display `id`'s stream at `width` x `height`, at most `fps` pictures a
    /// second, started. Thread C has no autorelease pool: this brings its
    /// own.
    pub(crate) fn start(id: u32, width: u32, height: u32, fps: u32, waker: &Waker) -> Result<Self> {
        objc2::rc::autoreleasepool(|_| Self::started(id, width, height, fps, waker))
    }

    fn started(id: u32, width: u32, height: u32, fps: u32, waker: &Waker) -> Result<Self> {
        let display = display(id)?;
        let filter = filter(&display);
        let prepared = configuration(width, height, true);
        // SAFETY: a fresh configuration, before the stream reads it.
        unsafe {
            // Give refresh timestamps a 10% margin around the requested
            // rate. Thread C still applies the negotiated frame cap.
            prepared
                .config
                .setMinimumFrameInterval(CMTime::new(9, (fps.clamp(1, 1_000) * 10) as i32));
            prepared.config.setQueueDepth(QUEUE_DEPTH);
        }
        let shared = Arc::new(Shared {
            latest: Mutex::default(),
            waker: waker.clone(),
        });
        let output = Output::new(shared.clone());
        let delegate = ProtocolObject::<dyn SCStreamDelegate>::from_ref(&*output);
        // SAFETY: a filter, a configuration and a delegate that lives as
        // long as the stream.
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &prepared.config,
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
        .map_err(|err| failure(Some(&err)))?;
        let (sender, started) = mpsc::sync_channel(1);
        let handler = RcBlock::new(move |err: *mut NSError| {
            // SAFETY: the error is null on success or valid for this call.
            let result =
                unsafe { err.as_ref() }.map_or(Ok(()), |err| Err(failure(Some(err)).into()));
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
                _background: prepared.background,
            }),
            shared,
        };
        drop(handler);
        answer(started, "stream start")?;
        Ok(this)
    }

    /// Fails once the system stopped the stream.
    pub(crate) fn check(&self) -> Result<()> {
        match &self.shared.latest.lock().stopped {
            Some(stopped) => Err(stopped.clone().into()),
            None => Ok(()),
        }
    }

    pub(super) fn stopped(&self) -> bool {
        self.shared.latest.lock().stopped.is_some()
    }

    /// What thread C can do without waiting for the next stream callback.
    pub(super) fn state(&self) -> CaptureState {
        let latest = self.shared.latest.lock();
        if latest.fresh || latest.stopped.is_some() {
            CaptureState::Ready
        } else if latest.picture.is_some() {
            CaptureState::Repeatable
        } else {
            CaptureState::Waiting
        }
    }

    /// Take a new or explicitly repeated picture. A stopped stream never
    /// yields an old picture.
    pub(crate) fn take(&self, repeat: bool) -> Result<Option<(CFRetained<CVPixelBuffer>, bool)>> {
        let mut latest = self.shared.latest.lock();
        if let Some(stopped) = &latest.stopped {
            return Err(stopped.clone().into());
        }
        let fresh = std::mem::take(&mut latest.fresh);
        Ok(if fresh || repeat {
            latest
                .picture
                .as_ref()
                .map(|picture| (picture.0.clone(), fresh))
        } else {
            None
        })
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
