//! ScreenCaptureKit: one display's stream, scaled by the system to the
//! encoder's size, in NV12 (sRGB through the BT.709 matrix, video range),
//! without the pointer, which the viewer draws. Its queue keeps only the
//! newest complete picture, an IOSurface-backed pixel buffer the encoder
//! reads in place; thread C takes it at its own pace. A stream the system
//! stops says why at the next take.
//!
//! From macOS 14 the display's picture fills the frame, stretched by the
//! pixel or two by which the encoder's size, rounded down to even pixels,
//! may miss the display's shape. Before, it keeps its shape, and what it
//! does not cover takes the background colour, clear unless set, which
//! NV12 has no alpha to show: the background is opaque black, which the
//! configuration does not retain. A padded frame (`frame.rs`) has the
//! display drawn into its top left, the content's size in pixels
//! (`destinationRect`), and the rest takes the background.
//!
//! The stream starts at a minimum interval between pictures from the
//! display's refresh and the session's pace (`cadence.rs`), and changes it
//! live (`Stream::set_interval`), one change at a time; a change
//! ScreenCaptureKit refuses fails the stream, as a stop does. A display is
//! found among the displays and on-screen windows only, which is all a
//! display's filter needs.
//!
//! Each picture keeps when it arrived, which thread C anchors its pacing
//! on, and how long after the window server displayed it that was
//! (`SCStreamFrameInfoDisplayTime`), which only the debug log reads. The
//! stream counts its complete pictures, its idle samples and the pictures a
//! newer one replaced before thread C took them.

use super::{cadence::Updates, cf, frame::Frame};
use crate::{BackendError, FailureKind, Result, Waker};
use block2::RcBlock;
use dispatch2::{DispatchQoS, DispatchQueue, DispatchRetained, GlobalQueueIdentifier};
use objc2::{
    AnyThread, ClassType, DefinedClass, Message, define_class, msg_send,
    rc::Retained,
    runtime::{AnyClass, NSObject, ProtocolObject},
    sel,
};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    CGColor, CGDisplayCopyDisplayMode, CGDisplayMode, CGMainDisplayID, kCGColorSpaceSRGB,
    kCGDisplayStreamYCbCrMatrix_ITU_R_709_2,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVDisplayLink, CVPixelBuffer, CVTimeFlags, kCVPixelFormatType_32BGRA,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVReturnSuccess,
};
use objc2_foundation::{
    NSArray, NSError, NSObjectProtocol, NSOperatingSystemVersion, NSProcessInfo,
};
use objc2_screen_capture_kit::{
    SCCaptureDynamicRange, SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamDelegate, SCStreamErrorCode, SCStreamErrorDomain,
    SCStreamFrameInfoDisplayTime, SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
    SCWindow,
};
use parking_lot::Mutex;
use std::{
    sync::{Arc, OnceLock, mpsc},
    time::{Duration, Instant},
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

/// Whether a stream can draw the display into part of its frame
/// (`destinationRect`, of macOS 12.3, as ScreenCaptureKit is), which a
/// padded frame needs.
pub(crate) fn places() -> bool {
    SCStreamConfiguration::class().responds_to(sel!(setDestinationRect:))
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
    /// When `picture` arrived, and how long after the window server
    /// displayed it, when its sample said.
    arrived: Option<Instant>,
    displayed: Option<Duration>,
    /// Complete pictures since the last take.
    since_take: u32,
    /// When the first complete picture arrived.
    first: Option<Instant>,
    counts: Counts,
    stopped: Option<BackendError>,
    /// Each complete picture's display time, in mach absolute time, for a
    /// measurement to read, up to `DISPLAY_TIMES`.
    #[cfg(test)]
    display_times: Vec<u64>,
}

/// The display times a measurement keeps.
#[cfg(test)]
const DISPLAY_TIMES: usize = 8_192;

/// What the stream delivered since it started.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Counts {
    /// Samples holding a new picture.
    pub complete: u64,
    /// Samples that said nothing changed.
    pub idle: u64,
    /// New pictures a newer one replaced before thread C took them.
    pub replaced: u64,
}

/// A picture thread C took.
pub(crate) struct Taken {
    pub picture: CFRetained<CVPixelBuffer>,
    /// For a picture not taken before, when it arrived.
    pub fresh: Option<Arrival>,
}

/// When a new picture arrived, and what came with it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Arrival {
    pub at: Instant,
    /// The complete pictures since the last take: it and those it replaced.
    pub pictures: u32,
    /// How long after the window server displayed it it arrived.
    pub displayed: Option<Duration>,
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

/// The frame status of `sample` (complete: a new picture; idle: nothing
/// changed), and the mach absolute time the window server displayed it,
/// read from one attachments dictionary.
fn frame_info(sample: &CMSampleBuffer) -> Option<(SCFrameStatus, Option<u64>)> {
    // SAFETY: the stream's sample, read only.
    let attachments = unsafe { sample.sample_attachments_array(false) }?;
    // SAFETY: a sample's attachments are an array of dictionaries.
    let attachments = unsafe { CFRetained::cast_unchecked::<CFArray<CFDictionary>>(attachments) };
    let info = attachments.get(0)?;
    // SAFETY: ScreenCaptureKit constants, which exist wherever its classes
    // do; an NSString is the CFString it is toll-free bridged to.
    let (status, displayed): (&CFString, &CFString) = unsafe {
        (
            &*(SCStreamFrameInfoStatus as *const _ as *const CFString),
            &*(SCStreamFrameInfoDisplayTime as *const _ as *const CFString),
        )
    };
    let status = cf::value::<CFNumber>(&info, status)?.as_i64()?;
    let displayed = cf::value::<CFNumber>(&info, displayed)
        .and_then(|time| time.as_i64())
        .and_then(|time| u64::try_from(time).ok());
    Some((SCFrameStatus(status as isize), displayed))
}

/// Mach absolute time's units, as `mach_timebase_info` gives them.
#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}
// libc declares these deprecated in favour of a crate of their own.
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut Timebase) -> i32;
}

/// The longest ago a display time is believed.
const DISPLAYED_MAX_AGE: Duration = Duration::from_secs(1);

/// How long ago the mach absolute time `ticks` was: a time a moment ahead
/// (the order of the reads) is now, and one over a second old, or with no
/// timebase, is none.
fn mach_age(ticks: u64) -> Option<Duration> {
    let (numer, denom) = timebase()?;
    // SAFETY: no arguments; reads a clock.
    let now = unsafe { mach_absolute_time() };
    ticks_age(now.saturating_sub(ticks), numer, denom)
}

/// Mach absolute time's units, once read.
fn timebase() -> Option<(u32, u32)> {
    static TIMEBASE: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *TIMEBASE.get_or_init(|| {
        let mut timebase = Timebase { numer: 0, denom: 0 };
        // SAFETY: an out parameter the call fills.
        let status = unsafe { mach_timebase_info(&mut timebase) };
        (status == 0 && timebase.numer != 0 && timebase.denom != 0)
            .then_some((timebase.numer, timebase.denom))
    })
}

/// `ticks` of mach absolute time, for a measurement.
#[cfg(test)]
pub(super) fn mach_duration(ticks: u64) -> Option<Duration> {
    let (numer, denom) = timebase()?;
    let nanos = u128::from(ticks) * u128::from(numer) / u128::from(denom);
    u64::try_from(nanos).ok().map(Duration::from_nanos)
}

/// `ticks` of mach absolute time at `numer / denom` nanoseconds each, as a
/// duration no longer than `DISPLAYED_MAX_AGE`.
fn ticks_age(ticks: u64, numer: u32, denom: u32) -> Option<Duration> {
    let nanos = u128::from(ticks) * u128::from(numer) / u128::from(denom.max(1));
    let age = Duration::from_nanos(u64::try_from(nanos).ok()?);
    (age <= DISPLAYED_MAX_AGE).then_some(age)
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
        /// On the stream's queue: keep the newest complete picture, when it
        /// arrived, and count it and the idle samples.
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            let Some((status, displayed)) = frame_info(sample) else {
                return;
            };
            if status == SCFrameStatus::Idle {
                self.ivars().latest.lock().counts.idle += 1;
                return;
            }
            if status != SCFrameStatus::Complete {
                return;
            }
            // SAFETY: the stream's sample, read only.
            let Some(image) = (unsafe { sample.image_buffer() }) else {
                return;
            };
            let arrived = Instant::now();
            #[cfg(test)]
            let display_time = displayed;
            let displayed = displayed.and_then(mach_age);
            let wake = {
                let mut latest = self.ivars().latest.lock();
                let wake = !latest.fresh;
                #[cfg(test)]
                if let Some(time) = display_time
                    && latest.display_times.len() < DISPLAY_TIMES
                {
                    latest.display_times.push(time);
                }
                latest.first.get_or_insert(arrived);
                latest.counts.complete += 1;
                latest.counts.replaced += u64::from(latest.fresh);
                latest.since_take = latest.since_take.saturating_add(1);
                latest.picture = Some(Picture(image));
                latest.fresh = true;
                latest.arrived = Some(arrived);
                latest.displayed = displayed;
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

/// What ScreenCaptureKit's shareable-content `request` gives, waited for.
/// `request` makes the call with the handler it is given.
pub(super) fn shareable(
    request: impl FnOnce(&block2::DynBlock<dyn Fn(*mut SCShareableContent, *mut NSError)>),
) -> Result<Retained<SCShareableContent>> {
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
    request(&handler);
    drop(handler);
    let ShareableContent(content) = answer(receiver, "display enumeration")?;
    Ok(content)
}

/// ScreenCaptureKit's display of id `id`, which a filter needs. Only the
/// on-screen windows are listed with it, without the desktop's: a filter of
/// a whole display excludes no window, so the picture is the same, and the
/// windows of every app off screen are not enumerated.
pub(crate) fn display(id: u32) -> Result<Retained<SCDisplay>> {
    let content = shareable(|handler| {
        // SAFETY: the handler outlives the call; ScreenCaptureKit copies it.
        unsafe {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                true, true, handler,
            )
        }
    })?;
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

/// What one display says of its refresh, read off the main thread: its
/// mode's rate, and the nominal period of a display link made on it once,
/// for a built-in panel whose mode may report none. On an M6, back to back,
/// a mode read took 5 to 24 µs, the first link a process makes 2.4 to
/// 2.7 ms and another about 190 µs, and a period read 11 ns
/// (`sck_display_lookup`).
pub(crate) struct Refresh {
    id: u32,
    link: Option<CFRetained<CVDisplayLink>>,
}

impl Refresh {
    pub(crate) fn new(id: u32) -> Self {
        let mut link = std::ptr::null_mut();
        // SAFETY: an out pointer to a local, which the call sets to a link it
        // created on success. The deprecated display link is the only source
        // of a display's refresh off the main thread (NSScreen is the main
        // thread's), and nothing starts it.
        #[expect(
            deprecated,
            reason = "CVDisplayLink is the only refresh source off the main thread"
        )]
        let created = unsafe {
            CVDisplayLink::create_with_cg_display(id, std::ptr::NonNull::from(&mut link))
        };
        let link = std::ptr::NonNull::new(link)
            .filter(|_| created == kCVReturnSuccess)
            // SAFETY: a Create-rule result, released when this drops.
            .map(|link| unsafe { CFRetained::from_raw(link) });
        Self { id, link }
    }

    /// The mode's refresh rate; 0 where it reports none.
    pub(crate) fn mode_hz(&self) -> f64 {
        CGDisplayMode::refresh_rate(CGDisplayCopyDisplayMode(self.id).as_deref())
    }

    /// The display link's nominal refresh period, where it knows one.
    pub(crate) fn link_period(&self) -> Option<Duration> {
        let link = self.link.as_ref()?;
        #[expect(
            deprecated,
            reason = "CVDisplayLink is the only refresh source off the main thread"
        )]
        let period = link.nominal_output_video_refresh_period();
        let indefinite = CVTimeFlags(period.flags).contains(CVTimeFlags::IsIndefinite);
        (!indefinite && period.timeValue > 0 && period.timeScale > 0)
            .then(|| Duration::from_secs_f64(period.timeValue as f64 / f64::from(period.timeScale)))
    }

    /// The refresh now: the mode's, else the display link's.
    pub(crate) fn hz(&self) -> Option<f64> {
        let mode = self.mode_hz();
        super::cadence::refresh_hz(mode, None)
            .or_else(|| super::cadence::refresh_hz(mode, self.link_period()))
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

/// The configuration of a picture of `frame`'s coded size in sRGB, without
/// the pointer, whose content the display fills (macOS 14), else on an
/// opaque black background, which also fills the rest: NV12 (BT.709 matrix,
/// video range) for the encoder, else BGRA, as an image takes it.
pub(crate) fn configuration(frame: Frame, nv12: bool) -> PreparedCapture {
    with_background(frame, nv12, CGColor::new_srgb(0.0, 0.0, 0.0, 1.0))
}

/// `configuration` on `background`, a stream's own when it is updated.
fn with_background(frame: Frame, nv12: bool, background: CFRetained<CGColor>) -> PreparedCapture {
    let Frame { coded, content } = frame;
    // SAFETY: a fresh configuration, set before any stream reads it; the
    // constants are Core Graphics', alive for the process.
    let config = unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(coded.0 as usize);
        config.setHeight(coded.1 as usize);
        // A padded frame only, and only where it can (`places`): the
        // content in the top left, in the frame's pixels. Unset, the
        // display fills the frame.
        if frame.cropped() {
            config.setDestinationRect(CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(f64::from(content.0), f64::from(content.1)),
            ));
        }
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
        // macOS 14. The encoder's size, rounded down to even pixels, may
        // miss the display's shape by a pixel or two: kept, the picture
        // would leave a row or column of background along an edge, while
        // viewers map the pointer across the whole frame. In a padded
        // frame, the picture fills its content.
        if SCStreamConfiguration::class().responds_to(sel!(setPreservesAspectRatio:)) {
            config.setPreservesAspectRatio(false);
        }
        config
    };
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
    /// What its interval changes and their completions share.
    retune: Arc<Retune>,
    startup: Startup,
}

/// How a stream started, for `macos_capture_started`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Startup {
    /// When the start began.
    pub began: Instant,
    /// Finding the display among the shareable content.
    pub lookup: Duration,
    /// The rest, to the start's completion.
    pub start: Duration,
}

/// What a stream's interval changes and their completion handlers share:
/// the stream with its output, the frame and background colour each new
/// configuration repeats, and the changes in flight.
struct Retune {
    /// Released before `output`, the stream's delegate, so that no
    /// reference to the stream held here outlives it.
    stream: Retained<SCStream>,
    output: Retained<Output>,
    frame: Frame,
    /// The stream's own background colour, which every configuration it is
    /// given points at; `Running` keeps it until the stream has stopped.
    background: CFRetained<CGColor>,
    updates: Mutex<Updates>,
}
// SAFETY: thread C and ScreenCaptureKit's completion queue share this. The
// stream, its output and the colour are Objective-C and Core Foundation
// objects, retained and released atomically from any thread, and never
// mutated here; the output's state is behind its own lock. The stream is
// only sent asynchronous requests, each answered on ScreenCaptureKit's own
// queue: one configuration update at a time, from thread C or from the
// completion of the last, which `updates` serialises under its lock, and
// none once the stream is stopping. The rest is plain data.
unsafe impl Send for Retune {}
// SAFETY: as for `Send`: shared references only read the retained objects,
// lock the output's state and send the stream the requests `updates`
// allows.
unsafe impl Sync for Retune {}

/// An interval change ScreenCaptureKit has yet to complete: the
/// configuration it was handed, with its colour, kept until then, and what
/// hands over the next.
struct Update {
    _prepared: PreparedCapture,
    retune: Arc<Retune>,
}

impl Retune {
    /// Hand ScreenCaptureKit `interval`, in a configuration otherwise the
    /// stream's own, without waiting. Its completion hands over the change
    /// asked for meanwhile, if any; a refused change fails the stream.
    fn issue(self: Arc<Self>, interval: Duration) {
        objc2::rc::autoreleasepool(|_| {
            let prepared = with_background(self.frame, true, self.background.clone());
            // SAFETY: a fresh configuration, before the stream reads it.
            unsafe {
                prepared
                    .config
                    .setMinimumFrameInterval(frame_time(interval));
                prepared.config.setQueueDepth(QUEUE_DEPTH);
            }
            let config = prepared.config.clone();
            let held = Mutex::new(Some(Update {
                _prepared: prepared,
                retune: self.clone(),
            }));
            let handler = RcBlock::new(move |err: *mut NSError| {
                let Some(Update { _prepared, retune }) = held.lock().take() else {
                    return;
                };
                // SAFETY: the error is null on success or valid for this call.
                let refused = unsafe { err.as_ref() }.map(|err| failure(Some(err)));
                let (next, failed) = {
                    let mut updates = retune.updates.lock();
                    let failed = refused.filter(|_| !updates.stopped());
                    if failed.is_some() {
                        updates.stop();
                    }
                    (updates.completed(), failed)
                };
                if let Some(refused) = failed {
                    tracing::warn!(
                        target: "pb_mapper_desktop::session",
                        event = "macos_capture_interval_refused",
                        interval_us = interval.as_micros() as u64,
                        detail = %refused,
                    );
                    retune.fail(refused);
                }
                if let Some(next) = next {
                    retune.issue(next);
                }
            });
            // SAFETY: a configuration like the stream's, which the handler
            // keeps, with its colour, until ScreenCaptureKit completes the
            // update; `self` keeps the stream and its delegate through the
            // call; ScreenCaptureKit copies the handler.
            unsafe {
                self.stream
                    .updateConfiguration_completionHandler(&config, Some(&handler))
            };
        });
    }

    /// The stream runs at an interval other than the one its owner thinks:
    /// fail it at thread C's next take, as the system stopping it would, so
    /// that thread C opens another at the interval it wants.
    fn fail(&self, failure: BackendError) {
        let shared = self.output.ivars();
        shared.latest.lock().stopped.get_or_insert(failure);
        shared.waker.wake();
    }
}

/// `interval` as a time in nanoseconds; zero lets every refresh through.
fn frame_time(interval: Duration) -> CMTime {
    let nanos = i64::try_from(interval.as_nanos()).unwrap_or(i64::MAX);
    // SAFETY: CMTimeMake only fills in a value.
    unsafe { CMTime::new(nanos, 1_000_000_000) }
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
    /// Display `id`'s stream filling `frame`, at least `interval` between
    /// pictures (`cadence.rs`), started. Thread C has no autorelease pool:
    /// this brings its own.
    pub(crate) fn start(id: u32, frame: Frame, interval: Duration, waker: &Waker) -> Result<Self> {
        objc2::rc::autoreleasepool(|_| Self::started(id, frame, interval, false, waker))
    }

    /// `start`, with the pointer drawn into the pictures, which a
    /// measurement moves to change the screen at a known time.
    #[cfg(test)]
    pub(super) fn start_with_pointer(
        id: u32,
        frame: Frame,
        interval: Duration,
        waker: &Waker,
    ) -> Result<Self> {
        objc2::rc::autoreleasepool(|_| Self::started(id, frame, interval, true, waker))
    }

    fn started(
        id: u32,
        frame: Frame,
        interval: Duration,
        pointer: bool,
        waker: &Waker,
    ) -> Result<Self> {
        let began = Instant::now();
        let display = display(id)?;
        let lookup = began.elapsed();
        let filter = filter(&display);
        let prepared = configuration(frame, true);
        // SAFETY: a fresh configuration, before the stream reads it.
        unsafe {
            prepared
                .config
                .setMinimumFrameInterval(frame_time(interval));
            prepared.config.setQueueDepth(QUEUE_DEPTH);
            if pointer {
                prepared.config.setShowsCursor(true);
            }
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
        let retune = Arc::new(Retune {
            stream: stream.clone(),
            output: output.clone(),
            frame,
            background: prepared.background(),
            updates: Mutex::default(),
        });
        // Stopped as it drops, should the start fail or time out.
        let mut this = Self {
            running: Some(Running {
                stream,
                output,
                _queue: queue,
                _background: prepared.background,
            }),
            shared,
            retune,
            startup: Startup {
                began,
                lookup,
                start: Duration::ZERO,
            },
        };
        drop(handler);
        answer(started, "stream start")?;
        this.startup.start = began.elapsed().saturating_sub(lookup);
        Ok(this)
    }

    /// Run at least `interval` between pictures from ScreenCaptureKit's next
    /// on, without waiting. A change asked for while another is in flight
    /// follows it, the newest only; one ScreenCaptureKit refuses fails the
    /// stream at the next take.
    pub(crate) fn set_interval(&self, interval: Duration) {
        let now = self.retune.updates.lock().ask(interval);
        if let Some(interval) = now {
            self.retune.clone().issue(interval);
        }
    }

    /// How the stream started.
    pub(super) fn startup(&self) -> Startup {
        self.startup
    }

    /// When its first complete picture arrived.
    pub(super) fn first_picture(&self) -> Option<Instant> {
        self.shared.latest.lock().first
    }

    /// The display times of the complete pictures since the last call, in
    /// mach absolute time.
    #[cfg(test)]
    pub(super) fn display_times(&self) -> Vec<u64> {
        std::mem::take(&mut self.shared.latest.lock().display_times)
    }

    /// Fails once the system stopped the stream, or refused to change its
    /// interval.
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

    /// Take a new or explicitly repeated picture: a new one with when it
    /// arrived and the complete pictures since the last take. A stopped
    /// stream never yields an old picture.
    pub(crate) fn take(&self, repeat: bool) -> Result<Option<Taken>> {
        let mut latest = self.shared.latest.lock();
        if let Some(stopped) = &latest.stopped {
            return Err(stopped.clone().into());
        }
        let fresh = std::mem::take(&mut latest.fresh);
        if !(fresh || repeat) {
            return Ok(None);
        }
        let pictures = std::mem::take(&mut latest.since_take);
        let arrival = latest.arrived.filter(|_| fresh).map(|at| Arrival {
            at,
            pictures,
            displayed: latest.displayed,
        });
        Ok(latest.picture.as_ref().map(|picture| Taken {
            picture: picture.0.clone(),
            fresh: arrival,
        }))
    }

    /// What the stream delivered since it started.
    pub(super) fn counts(&self) -> Counts {
        self.shared.latest.lock().counts
    }
}

impl Drop for Stream {
    /// Detaches the output, so that no picture arrives after this, then
    /// stops without waiting. The stream, its output and their queue live
    /// until the stop is done, on ScreenCaptureKit's completion queue. An
    /// interval change in flight hands over no other.
    fn drop(&mut self) {
        self.retune.updates.lock().stop();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Mach ticks become nanoseconds through the timebase (Apple silicon's
    /// 125/3), and an age over a second is none.
    #[test]
    fn display_times_are_aged_through_the_timebase() {
        assert_eq!(ticks_age(24_000, 125, 3), Some(Duration::from_millis(1)));
        assert_eq!(ticks_age(1_000, 1, 1), Some(Duration::from_micros(1)));
        assert_eq!(ticks_age(24_000_001, 125, 3), None);
        // The clock itself: a time 2 ms ago reads about 2 ms old, one ahead
        // as now, one 2 s ago as none.
        let mut timebase = Timebase { numer: 0, denom: 0 };
        // SAFETY: an out parameter the call fills.
        assert_eq!(unsafe { mach_timebase_info(&mut timebase) }, 0);
        let ticks =
            |ms: u64| ms * 1_000_000 * u64::from(timebase.denom) / u64::from(timebase.numer);
        // SAFETY: no arguments.
        let now = unsafe { mach_absolute_time() };
        let ago = mach_age(now - ticks(2)).unwrap();
        assert!(
            ago >= Duration::from_millis(2) && ago < Duration::from_millis(50),
            "{ago:?}"
        );
        assert!(mach_age(now + ticks(1)).unwrap() < Duration::from_millis(50));
        assert_eq!(mach_age(now - ticks(2_000)), None);
    }
}
