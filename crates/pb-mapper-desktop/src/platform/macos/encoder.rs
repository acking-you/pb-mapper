//! VideoToolbox: a real-time hardware H.264 or H.265 session, no frame
//! reordering, an unbounded GOP (an IDR only when asked), its bitrate set
//! live. It reads the capture's IOSurface in place. Its output callback, on
//! VideoToolbox's thread, turns each length-prefixed sample into Annex B,
//! a keyframe's parameter sets in front, queues it and wakes thread C.
//!
//! Once the session is prepared, every call on it is made by the encoder's
//! own submit thread, one at a time: thread C only hands it a picture, or a
//! bitrate, through a one-slot mailbox and goes back to its input lane. A
//! session whose size is a multiple of 16 encodes inside
//! `VTCompressionSessionEncodeFrame` and calls back before it returns, so
//! the call took as long as the encode, and input queued on thread C waited
//! it out. The submit thread wakes thread C when it finishes a call that a
//! submit found it busy with, and when a call fails.

use super::cf;
use crate::{
    EncodedFrame, Result, VideoCodec, VideoFormat, Waker,
    codec::{
        avcc::annex_b,
        pipeline::{LATE_OUTPUT, OUTPUT_WAIT, codec_error, late},
    },
    error,
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, Type};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMVideoCodecType,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMSampleAttachmentKey_NotSync,
    kCMTimeInvalid, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
    CVPixelBuffer, kCVImageBufferColorPrimaries_ITU_R_709_2,
    kCVImageBufferTransferFunction_ITU_R_709_2, kCVImageBufferYCbCrMatrix_ITU_R_709_2,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionCopyProperty, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_DataRateLimits,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_H264_High_AutoLevel, kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::{
    collections::VecDeque,
    ffi::{c_int, c_void},
    ptr::NonNull,
    sync::{Arc, OnceLock},
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// A unit submitted and not yet output.
struct Pending {
    token: u64,
    at: Instant,
    duplicate: bool,
    keyframe: bool,
    pts_us: u64,
}

#[derive(Default)]
struct State {
    pending: VecDeque<Pending>,
    ready: VecDeque<EncodedFrame>,
    /// The first failure the callback saw; the session is spent.
    failure: Option<String>,
    /// When the last output was collected, discarded or not.
    last_output: Option<Instant>,
    /// A late unit was discarded: output resumes at the next keyframe.
    wait_independent: bool,
    /// The next submit must be a keyframe: one asked for was dropped or
    /// discarded.
    keyframe_due: bool,
    /// What the output callback converts into, reused.
    sample: Vec<u8>,
    /// The picture handed over and not yet taken by the submit thread; its
    /// `Pending` is already queued.
    job: Option<Job>,
    /// The bitrate the submit thread sets before its next encode, or at once
    /// when idle: the latest asked for.
    rate: Option<u32>,
    /// The submit thread is inside a call on the session.
    in_call: bool,
    /// A submit found the submit thread busy: thread C waits for its wake.
    c_waiting: bool,
    /// The encoder is going: the submit thread makes no further call.
    stop: bool,
    /// The submit thread invalidated the session and is ending.
    exited: bool,
    /// Thread C's side of `macos_encoder_load`, since its last line.
    gate: GateCounts,
    /// The submit thread takes nothing while set: a test's way to keep a
    /// picture in the mailbox.
    #[cfg(test)]
    held: bool,
    /// The submit thread tears the session down before its next call: a
    /// test's way to have VideoToolbox refuse an encode.
    #[cfg(test)]
    tear_down: bool,
    /// The submit thread stays in its next call once VideoToolbox returns,
    /// while set: a test's way to keep it busy after any output came.
    #[cfg(test)]
    park: bool,
    /// The submit thread stays in its call (`park`).
    #[cfg(test)]
    parked: bool,
    /// What a measurement reads back.
    #[cfg(test)]
    probe: Probe,
}

/// What a measurement reads back from an encoder: what VideoToolbox
/// dropped, each kept unit's luma error, and how long each encode call took.
#[cfg(test)]
#[derive(Default)]
pub(super) struct Probe {
    /// Outputs VideoToolbox flagged `FrameDropped`.
    pub dropped: u64,
    /// Each kept unit's presentation time and luma mean squared error, where
    /// the session was built to measure it.
    pub quality: VecDeque<(u64, f64)>,
    /// Each encode call's duration on the submit thread, in microseconds.
    pub calls: Vec<u32>,
}
impl State {
    /// Units handed over and not yet out: in VideoToolbox, or in the
    /// mailbox, whose `Pending` is queued with it. A finished unit waiting
    /// to be drained, and a failure, are not in flight.
    fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// The submit thread holds a picture or is inside a call.
    fn busy(&self) -> bool {
        self.job.is_some() || self.in_call
    }

    /// Forget unit `token`, which never reached VideoToolbox or which it
    /// refused.
    fn forget(&mut self, token: u64) {
        if let Some(index) = self
            .pending
            .iter()
            .rposition(|pending| pending.token == token)
        {
            self.pending.remove(index);
        }
    }
}

/// What thread C's submits found, for `macos_encoder_load`.
#[derive(Clone, Copy, Default)]
struct GateCounts {
    /// Due ticks that found the limit's worth of units in flight
    /// (`Encoder::full`, `Admit::Full`).
    full: u32,
    /// Submits that found the submit thread busy and left their tick armed
    /// (`Admit::Busy`).
    busy: u32,
    /// The most units in flight at a hand-off.
    peak_in_flight: usize,
}

/// A picture for the submit thread to encode.
struct Job {
    picture: CFRetained<CVPixelBuffer>,
    pts_us: u64,
    keyframe: bool,
    token: u64,
}
// SAFETY: a pixel buffer is reference counted atomically; the job only
// carries it to the submit thread, which hands it to VideoToolbox to read,
// and nothing writes the picture while it is held.
unsafe impl Send for Job {}

struct Shared {
    state: Mutex<State>,
    /// Wakes the submit thread for a job, a bitrate or the stop, and the
    /// encoder's drop once the submit thread has ended.
    signal: Condvar,
    waker: Waker,
    codec: VideoCodec,
    size: (u32, u32),
    gop: u32,
    /// DataRateLimits caps the mean bitrate.
    capped: bool,
    /// VideoToolbox measures each unit's luma error.
    #[cfg(test)]
    quality: bool,
}

/// What a session is built with, and in a test the measurement's knobs.
#[derive(Clone, Copy, Debug)]
pub(super) struct Options {
    /// Low-latency rate control.
    pub low_latency: bool,
    /// DataRateLimits caps the mean: always, but in a measurement that
    /// leaves it off.
    pub capped: bool,
    /// VideoToolbox measures each unit's luma error (`Probe::quality`).
    #[cfg(test)]
    pub quality: bool,
}
impl Options {
    fn new(low_latency: bool) -> Self {
        Self {
            low_latency,
            capped: true,
            #[cfg(test)]
            quality: false,
        }
    }
}

/// What the submit thread owns: the session and the keyframe option.
struct SendSession {
    session: CFRetained<VTCompressionSession>,
    /// `{ForceKeyFrame: true}`, made once.
    keyframe: CFRetained<CFDictionary>,
}
// SAFETY: CF objects are retained and released atomically, and from the
// moment this moves to the submit thread every VideoToolbox call on the
// session is made there, one at a time, until that thread invalidates it.
unsafe impl Send for SendSession {}

/// Whether thread C may hand the encoder a picture now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admit {
    /// The limit's worth of units is in flight: backpressure, which spends
    /// the tick (thread C asks `full` first and skips it).
    Full,
    /// The submit thread holds a picture or is inside a call: the tick stays
    /// armed, and the thread wakes thread C once it is free.
    Busy,
    /// `encode` hands the picture over at once.
    Free,
}

/// A running compression session, and the thread that calls it.
pub(crate) struct Encoder {
    shared: Arc<Shared>,
    bitrate: u32,
    created: Instant,
    /// The last presentation time, which only grows.
    pts_us: u64,
    next_token: u64,
    /// The submit thread, joined on drop.
    submit: Option<JoinHandle<()>>,
    /// The session, for a test to read back while the submit thread idles.
    #[cfg(test)]
    session: CFRetained<VTCompressionSession>,
}

fn codec_type(codec: VideoCodec) -> Result<CMVideoCodecType> {
    match codec {
        VideoCodec::H264 => Ok(kCMVideoCodecType_H264),
        VideoCodec::H265 => Ok(kCMVideoCodecType_HEVC),
        VideoCodec::Jpeg => Err(error("the hardware encoder encodes H.264 or H.265")),
    }
}

/// Constants of macOS 11.3, looked up when used: H.264's low-latency rate
/// control, and the constrained profile it takes.
const LOW_LATENCY: &std::ffi::CStr = c"kVTVideoEncoderSpecification_EnableLowLatencyRateControl";
const CONSTRAINED_HIGH: &std::ffi::CStr = c"kVTProfileLevel_H264_ConstrainedHigh_AutoLevel";

/// The encoder specification: H.265 on hardware only; H.264 on hardware
/// where there is one; both with low-latency rate control when asked.
fn specification(codec: VideoCodec, low_latency: bool) -> CFRetained<CFDictionary> {
    // SAFETY: VideoToolbox's static keys, alive for the process.
    unsafe {
        let hardware = match codec {
            VideoCodec::H265 => kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
            VideoCodec::H264 | VideoCodec::Jpeg => {
                kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder
            }
        };
        match low_latency.then(|| cf::symbol(LOW_LATENCY)).flatten() {
            Some(latency) => {
                cf::dictionary(&[(hardware, cf::boolean(true)), (latency, cf::boolean(true))])
            }
            None => cf::dictionary(&[(hardware, cf::boolean(true))]),
        }
    }
}

/// A session of `codec` at `width` x `height`, its output to `shared`.
fn create(
    codec: VideoCodec,
    width: u32,
    height: u32,
    low_latency: bool,
    shared: *const Shared,
) -> Result<CFRetained<VTCompressionSession>> {
    let mut session = std::ptr::null_mut();
    // SAFETY: the dictionary lives through the call; `shared` outlives the
    // session, which is invalidated before it drops.
    let status = unsafe {
        VTCompressionSession::create(
            None,
            width as i32,
            height as i32,
            codec_type(codec)?,
            Some(&specification(codec, low_latency)),
            None,
            None,
            Some(output),
            shared.cast_mut().cast(),
            NonNull::from(&mut session),
        )
    };
    cf::check("creating the hardware video encoder", status)?;
    let session = NonNull::new(session).ok_or_else(|| error("VideoToolbox returned no session"))?;
    // SAFETY: a Create-rule result, released when this drops.
    Ok(unsafe { CFRetained::from_raw(session) })
}

fn set(session: &VTCompressionSession, key: &CFString, value: &CFType) -> Result<()> {
    // SAFETY: a session property, a CF key and value.
    let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
    cf::check("configuring the hardware video encoder", status)
}

/// A session's property, as `T`.
fn get<T: objc2_core_foundation::ConcreteType>(
    session: &VTCompressionSession,
    key: &CFString,
) -> Option<CFRetained<T>> {
    let mut value: *const CFType = std::ptr::null();
    // SAFETY: `value` receives a retained CF value.
    let status = unsafe {
        VTSessionCopyProperty(
            session,
            key,
            None,
            (&mut value as *mut *const CFType).cast(),
        )
    };
    let value = NonNull::new(value.cast_mut()).filter(|_| status == 0)?;
    // SAFETY: a Copy-rule result.
    unsafe { CFRetained::from_raw(value) }.downcast::<T>().ok()
}

/// The data-rate cap: half again the mean, over a second.
fn data_rate_limits(bitrate: u32) -> CFRetained<CFArray> {
    let bytes = cf::number(i64::from(bitrate) / 8 * 3 / 2);
    let seconds = cf::float(1.0);
    let values: [&CFType; 2] = [&bytes, &seconds];
    let array = CFArray::<CFType>::from_objects(&values);
    // SAFETY: an array of CF numbers is an opaque array.
    unsafe { CFRetained::cast_unchecked(array) }
}

/// Whether this Mac encodes `codec` on hardware: H.264 always (software
/// where a virtual Mac has no encoder), H.265 where a session can be made.
/// Asked once per process.
pub(crate) fn encodes(codec: VideoCodec) -> bool {
    static H265: OnceLock<bool> = OnceLock::new();
    match codec {
        VideoCodec::H264 => true,
        VideoCodec::Jpeg => false,
        VideoCodec::H265 => *H265.get_or_init(|| {
            match create(VideoCodec::H265, 1280, 720, false, std::ptr::null()) {
                Ok(session) => {
                    // SAFETY: never encoded on; invalidated once.
                    unsafe { session.invalidate() };
                    true
                }
                Err(_) => false,
            }
        }),
    }
}

impl Encoder {
    /// A session of `codec` at `width` x `height`, at nominally `fps` and
    /// `bitrate`, waking `waker` with each unit.
    pub(crate) fn new(
        codec: VideoCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        waker: &Waker,
    ) -> Result<Self> {
        // Low-latency rate control (macOS 11.3) first; a session that refuses
        // it, or a property under it, is made again without.
        let low_latency = matches!(codec, VideoCodec::H264 | VideoCodec::H265)
            && cf::symbol(LOW_LATENCY).is_some();
        let build = |low_latency| {
            Self::build(
                codec,
                width,
                height,
                fps,
                bitrate,
                waker,
                Options::new(low_latency),
            )
        };
        let built = if low_latency {
            build(true).or_else(|failure| {
                tracing::info!(
                    target: "pb_mapper_desktop::session",
                    event = "macos_encoder_low_latency_refused",
                    detail = %failure,
                );
                build(false)
            })
        } else {
            build(false)
        };
        built.map_err(|failure| codec_error(codec, failure))
    }

    /// `new` with low-latency rate control on or off as asked, never
    /// retried the other way: the measurement compares the two.
    #[cfg(test)]
    pub(super) fn with_low_latency(
        codec: VideoCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        waker: &Waker,
        low_latency: bool,
    ) -> Result<Self> {
        Self::tuned(
            codec,
            width,
            height,
            fps,
            bitrate,
            waker,
            Options::new(low_latency),
        )
    }

    /// `with_low_latency` with a measurement's knobs.
    #[cfg(test)]
    pub(super) fn tuned(
        codec: VideoCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        waker: &Waker,
        options: Options,
    ) -> Result<Self> {
        if options.low_latency && cf::symbol(LOW_LATENCY).is_none() {
            return Err(error("low-latency rate control needs macOS 11.3"));
        }
        Self::build(codec, width, height, fps, bitrate, waker, options)
    }

    fn build(
        codec: VideoCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        waker: &Waker,
        options: Options,
    ) -> Result<Self> {
        let low_latency = options.low_latency;
        // The GOP the session reads back is known once it exists; the
        // callback reads `shared`, which therefore exists first.
        let mut shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
            waker: waker.clone(),
            codec,
            size: (width, height),
            gop: 0,
            capped: options.capped,
            #[cfg(test)]
            quality: options.quality,
        });
        let session = create(codec, width, height, low_latency, Arc::as_ptr(&shared))?;
        // Whether the session took the data-rate cap, for the log.
        let mut capped = false;
        let configured = (|| {
            // SAFETY: VideoToolbox's and Core Video's static keys and values.
            unsafe {
                set(
                    &session,
                    kVTCompressionPropertyKey_RealTime,
                    cf::boolean(true),
                )?;
                set(
                    &session,
                    kVTCompressionPropertyKey_AllowFrameReordering,
                    cf::boolean(false),
                )?;
                // Low latency takes H.264's constrained profiles only.
                let profile = match codec {
                    VideoCodec::H265 => kVTProfileLevel_HEVC_Main_AutoLevel,
                    VideoCodec::H264 | VideoCodec::Jpeg if low_latency => {
                        cf::symbol(CONSTRAINED_HIGH).unwrap_or(kVTProfileLevel_H264_High_AutoLevel)
                    }
                    VideoCodec::H264 | VideoCodec::Jpeg => kVTProfileLevel_H264_High_AutoLevel,
                };
                set(&session, kVTCompressionPropertyKey_ProfileLevel, profile)?;
                set(
                    &session,
                    kVTCompressionPropertyKey_MaxKeyFrameInterval,
                    &cf::number(i64::from(i32::MAX)),
                )?;
                set(
                    &session,
                    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                    &cf::float(0.0),
                )?;
                set(
                    &session,
                    kVTCompressionPropertyKey_ExpectedFrameRate,
                    &cf::number(i64::from(fps)),
                )?;
                set(
                    &session,
                    kVTCompressionPropertyKey_AverageBitRate,
                    &cf::number(i64::from(bitrate)),
                )?;
                // A cap the session may not take; the mean still holds.
                capped = options.capped
                    && set(
                        &session,
                        kVTCompressionPropertyKey_DataRateLimits,
                        &data_rate_limits(bitrate),
                    )
                    .is_ok();
                #[cfg(test)]
                if options.quality {
                    let key = cf::symbol(c"kVTCompressionPropertyKey_CalculateMeanSquaredError")
                        .ok_or_else(|| error("no luma error on this macOS"))?;
                    set(&session, key, cf::boolean(true))?;
                }
                set(
                    &session,
                    kVTCompressionPropertyKey_ColorPrimaries,
                    kCVImageBufferColorPrimaries_ITU_R_709_2,
                )?;
                set(
                    &session,
                    kVTCompressionPropertyKey_TransferFunction,
                    kCVImageBufferTransferFunction_ITU_R_709_2,
                )?;
                set(
                    &session,
                    kVTCompressionPropertyKey_YCbCrMatrix,
                    kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                )?;
                cf::check(
                    "preparing the hardware video encoder",
                    session.prepare_to_encode_frames(),
                )
            }
        })();
        if let Err(failure) = configured {
            // SAFETY: invalidated once, before `shared` can go.
            unsafe { session.invalidate() };
            return Err(failure);
        }
        // SAFETY: a static key.
        let gop = get::<CFNumber>(&session, unsafe {
            kVTCompressionPropertyKey_MaxKeyFrameInterval
        })
        .and_then(|interval| interval.as_i64())
        .and_then(|interval| u32::try_from(interval).ok())
        .unwrap_or(0);
        // SAFETY: a static key.
        let hardware = get::<objc2_core_foundation::CFBoolean>(&session, unsafe {
            kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder
        })
        .is_some_and(|hardware| hardware.as_bool());
        // Nothing was encoded yet, so nothing else holds `shared`.
        if let Some(shared) = Arc::get_mut(&mut shared) {
            shared.gop = gop;
        }
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "macos_encoder_opened",
            codec = ?codec,
            low_latency,
            width,
            height,
            fps,
            hardware,
            gop,
            data_rate_limits = capped,
        );
        // SAFETY: a static key.
        let keyframe = unsafe {
            cf::dictionary(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, cf::boolean(true))])
        };
        // From here on the submit thread makes every call on the session.
        // It runs at the default QoS class, as thread C does: on macOS a new
        // thread does not take its creator's class, and neither sets one.
        let owned = SendSession {
            session: session.clone(),
            keyframe,
        };
        let spawned = std::thread::Builder::new()
            .name("pb-mapper-vt-submit".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || serve(owned, &shared)
            });
        let submit = match spawned {
            Ok(submit) => submit,
            Err(failure) => {
                // SAFETY: invalidated once, before `shared` can go: the
                // thread never ran.
                unsafe { session.invalidate() };
                return Err(failure);
            }
        };
        Ok(Self {
            shared,
            bitrate,
            created: Instant::now(),
            pts_us: 0,
            next_token: 0,
            submit: Some(submit),
            #[cfg(test)]
            session,
        })
    }

    pub(crate) fn codec(&self) -> VideoCodec {
        self.shared.codec
    }

    /// The mean bitrate, live; nothing else changes, nothing rebuilds. The
    /// submit thread sets it before its next encode, or at once when idle,
    /// and a refusal reaches `drain` as the codec's failure.
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) {
        if bitrate == self.bitrate {
            return;
        }
        self.shared.state.lock().rate = Some(bitrate);
        self.shared.signal.notify_all();
        self.bitrate = bitrate;
    }

    /// Units submitted and not yet drained (the mailbox's among them), or a
    /// failure waiting for `drain` to surface.
    pub(crate) fn pending(&self) -> usize {
        let state = self.shared.state.lock();
        state.pending.len() + state.ready.len() + usize::from(state.failure.is_some())
    }

    /// Whether `limit` units are in flight, for a due tick, which thread C
    /// then skips: counted as a gate tick.
    pub(crate) fn full(&self, limit: usize) -> bool {
        let mut state = self.shared.state.lock();
        let full = state.in_flight() >= limit;
        state.gate.full += u32::from(full);
        full
    }

    /// Whether a picture may be handed over now: not while `limit` units
    /// are in flight, which spends the tick, nor while the submit thread is
    /// busy, which leaves it armed until the thread wakes thread C.
    pub(crate) fn admit(&self, limit: usize) -> Admit {
        let mut state = self.shared.state.lock();
        if state.in_flight() >= limit {
            state.gate.full += 1;
            Admit::Full
        } else if state.busy() {
            state.c_waiting = true;
            state.gate.busy += 1;
            Admit::Busy
        } else {
            Admit::Free
        }
    }

    /// A submit found the submit thread busy, which has yet to wake thread
    /// C: until it does, nothing can be handed over.
    pub(crate) fn waiting(&self) -> bool {
        self.shared.state.lock().c_waiting
    }

    /// A keyframe was asked for and lost: the next submit carries one.
    pub(crate) fn keyframe_due(&self) -> bool {
        std::mem::take(&mut self.shared.state.lock().keyframe_due)
    }

    pub(crate) fn wants_keyframe(&self) -> bool {
        self.shared.state.lock().keyframe_due
    }

    /// Hand `picture` to the submit thread to encode; `duplicate` when it is
    /// the last one again. Never waits; a picture the thread has yet to take
    /// refuses it (`admit` says `Busy` first). A failure of the encode
    /// itself reaches `drain`.
    pub(crate) fn encode(
        &mut self,
        picture: &CVPixelBuffer,
        keyframe: bool,
        duplicate: bool,
    ) -> Result<()> {
        let now = Instant::now();
        let mut state = self.shared.state.lock();
        if state.job.is_some() {
            return Err(error("the hardware video encoder holds a picture already"));
        }
        let pts_us = (now.duration_since(self.created).as_micros() as u64).max(self.pts_us + 1);
        self.pts_us = pts_us;
        self.next_token += 1;
        let token = self.next_token;
        // Queued with the job: its dwell, lateness and stall count from now,
        // and a synchronous encoder outputs inside the thread's call.
        state.pending.push_back(Pending {
            token,
            at: now,
            duplicate,
            keyframe,
            pts_us,
        });
        state.gate.peak_in_flight = state.gate.peak_in_flight.max(state.in_flight());
        state.job = Some(Job {
            picture: picture.retain(),
            pts_us,
            keyframe,
            token,
        });
        drop(state);
        self.shared.signal.notify_all();
        Ok(())
    }

    /// The next unit, if one is out. A failure the callback or the submit
    /// thread saw, or a unit held too long, is the codec's.
    pub(crate) fn drain(&mut self) -> Result<Option<EncodedFrame>> {
        let mut state = self.shared.state.lock();
        if let Some(failure) = &state.failure {
            return Err(codec_error(self.shared.codec, error(failure.clone())));
        }
        if state
            .pending
            .front()
            .is_some_and(|pending| pending.at.elapsed() > OUTPUT_WAIT)
        {
            return Err(codec_error(
                self.shared.codec,
                error("hardware video encoder stalled"),
            ));
        }
        Ok(state.ready.pop_front())
    }

    /// Keep the submit thread from taking anything, or let it.
    #[cfg(test)]
    pub(super) fn hold(&self, held: bool) {
        self.shared.state.lock().held = held;
        self.shared.signal.notify_all();
    }

    /// Have the submit thread tear the session down before its next call,
    /// as VideoToolbox does to a session whose media server went away.
    #[cfg(test)]
    pub(super) fn tear_down(&self) {
        self.shared.state.lock().tear_down = true;
    }

    /// Keep the submit thread in its next call once VideoToolbox returns
    /// (`parked` says it is there), or let it finish.
    #[cfg(test)]
    pub(super) fn park(&self, park: bool) {
        self.shared.state.lock().park = park;
        self.shared.signal.notify_all();
    }

    /// The submit thread stays in a call that VideoToolbox returned from.
    #[cfg(test)]
    pub(super) fn parked(&self) -> bool {
        self.shared.state.lock().parked
    }

    /// The submit thread is inside a call on the session.
    #[cfg(test)]
    pub(super) fn in_call(&self) -> bool {
        self.shared.state.lock().in_call
    }

    /// The submit thread holds nothing: no picture, no bitrate, no call.
    #[cfg(test)]
    pub(super) fn idle(&self) -> bool {
        let state = self.shared.state.lock();
        !state.busy() && state.rate.is_none()
    }

    /// Whether anything still holds what the callback reads.
    #[cfg(test)]
    pub(super) fn alive(&self) -> impl Fn() -> bool + use<> {
        let shared = Arc::downgrade(&self.shared);
        move || shared.strong_count() > 0
    }

    /// The session's mean bitrate, read back while the submit thread idles.
    #[cfg(test)]
    pub(super) fn average_bitrate(&self) -> Option<i64> {
        // SAFETY: a static key.
        get::<CFNumber>(&self.session, unsafe {
            kVTCompressionPropertyKey_AverageBitRate
        })
        .and_then(|bitrate| bitrate.as_i64())
    }

    /// What a measurement reads back, taken through `read`.
    #[cfg(test)]
    pub(super) fn probe<T>(&self, read: impl FnOnce(&mut Probe) -> T) -> T {
        read(&mut self.shared.state.lock().probe)
    }

    /// The presentation time of the last picture handed over.
    #[cfg(test)]
    pub(super) fn last_pts(&self) -> u64 {
        self.pts_us
    }
}

impl Drop for Encoder {
    /// The submit thread finishes the call it is in, drops a picture still
    /// in the mailbox, invalidates the session and ends: no callback runs
    /// once this returns, and what was in flight is dropped. A thread still
    /// inside a call after `LATE_OUTPUT` is left to finish on its own; it
    /// holds the session and `Shared`, which outlive its callbacks.
    fn drop(&mut self) {
        let Some(submit) = self.submit.take() else {
            return;
        };
        let deadline = Instant::now() + LATE_OUTPUT;
        let mut state = self.shared.state.lock();
        state.stop = true;
        self.shared.signal.notify_all();
        while !state.exited {
            if self
                .shared
                .signal
                .wait_until(&mut state, deadline)
                .timed_out()
            {
                break;
            }
        }
        let exited = state.exited;
        drop(state);
        if exited {
            // It has only to return.
            let _ = submit.join();
        } else {
            tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "macos_encoder_left_running",
                waited_ms = LATE_OUTPUT.as_millis() as u64,
            );
        }
    }
}

/// The submit thread: every call on the session, one at a time, until the
/// encoder goes; then it invalidates the session, after which no callback
/// runs. No lock is held across a call.
fn serve(owned: SendSession, shared: &Shared) {
    let SendSession { session, keyframe } = owned;
    let mut load = Load::new(Instant::now());
    let mut state = shared.state.lock();
    loop {
        if state.stop {
            break;
        }
        #[cfg(test)]
        if state.held {
            shared.signal.wait(&mut state);
            continue;
        }
        let (rate, job) = (state.rate.take(), state.job.take());
        if rate.is_none() && job.is_none() {
            shared.signal.wait(&mut state);
            continue;
        }
        if state.failure.is_some() {
            // The session is spent: nothing more is encoded, and `drain`
            // says so.
            if let Some(job) = job {
                state.forget(job.token);
            }
            if std::mem::take(&mut state.c_waiting) {
                MutexGuard::unlocked(&mut state, || shared.waker.wake());
            }
            continue;
        }
        state.in_call = true;
        let token = job.as_ref().map(|job| job.token);
        #[cfg(test)]
        if std::mem::take(&mut state.tear_down) {
            // SAFETY: on this thread, between calls; every later call fails
            // as on a session VideoToolbox tore down itself.
            unsafe { session.invalidate() };
        }
        let (refused, encoded) = MutexGuard::unlocked(&mut state, || {
            // Off the main thread, with no pool of its own: VideoToolbox may
            // autorelease.
            objc2::rc::autoreleasepool(|_| {
                let refused =
                    rate.and_then(|bitrate| apply(&session, bitrate, shared.capped).err());
                let encoded = job.filter(|_| refused.is_none()).map(|job| {
                    let started = Instant::now();
                    let status = encode_job(&session, &keyframe, &job);
                    (status, started.elapsed())
                });
                (refused, encoded)
            })
        });
        #[cfg(test)]
        if state.park {
            state.parked = true;
            while state.park && !state.stop {
                shared.signal.wait(&mut state);
            }
            state.parked = false;
        }
        state.in_call = false;
        let mut wake = std::mem::take(&mut state.c_waiting);
        let failure = match (refused, encoded) {
            (Some(refused), _) => Some(refused),
            (None, Some((status, _))) if status != 0 => {
                Some(cf::status("hardware video encoding", status))
            }
            (None, Some((_, took))) => {
                load.call(took);
                #[cfg(test)]
                state
                    .probe
                    .calls
                    .push(u32::try_from(took.as_micros()).unwrap_or(u32::MAX));
                None
            }
            (None, None) => None,
        };
        if let Some(failure) = failure {
            // The job never reached VideoToolbox, or VideoToolbox refused it:
            // no callback comes for it.
            if let Some(token) = token {
                state.forget(token);
            }
            state.failure.get_or_insert_with(|| failure.to_string());
            wake = true;
        }
        if wake {
            MutexGuard::unlocked(&mut state, || shared.waker.wake());
        }
        let now = Instant::now();
        if load.due(now) {
            let gate = std::mem::take(&mut state.gate);
            MutexGuard::unlocked(&mut state, || load.roll(now, gate));
        }
    }
    // A picture never taken goes with the session.
    state.job = None;
    drop(state);
    // SAFETY: after the last call, and once outside a test's `tear_down`;
    // `shared` and the session stay alive until this thread returns.
    unsafe { session.invalidate() };
    let mut state = shared.state.lock();
    state.exited = true;
    shared.signal.notify_all();
}

/// Set the mean bitrate and, when `capped`, its cap, on the submit thread. A
/// session that refuses the cap still holds the mean.
fn apply(session: &VTCompressionSession, bitrate: u32, capped: bool) -> Result<()> {
    // SAFETY: static keys.
    unsafe {
        set(
            session,
            kVTCompressionPropertyKey_AverageBitRate,
            &cf::number(i64::from(bitrate)),
        )?;
        if capped {
            let _ = set(
                session,
                kVTCompressionPropertyKey_DataRateLimits,
                &data_rate_limits(bitrate),
            );
        }
    }
    Ok(())
}

/// Encode `job`, on the submit thread: its status.
fn encode_job(session: &VTCompressionSession, keyframe: &CFDictionary, job: &Job) -> i32 {
    let mut flags = VTEncodeInfoFlags(0);
    // SAFETY: an IOSurface-backed picture the session retains while it reads;
    // the token is an opaque number handed back to the callback.
    unsafe {
        session.encode_frame(
            &job.picture,
            CMTime::new(job.pts_us as i64, 1_000_000),
            kCMTimeInvalid,
            job.keyframe.then_some(keyframe),
            job.token as usize as *mut c_void,
            &mut flags,
        )
    }
}

/// Where `macos_encoder_load` goes: with thread C's pacing.
const LOAD_TARGET: &str = "pb_mapper_desktop::feedback";
/// Encode calls timed per second.
const LOAD_SAMPLES: usize = 256;

/// The submit thread's debug figures, `macos_encoder_load`, after the first
/// call a second or more after the last line: the window's length, how long
/// its encode calls took (p50, p99), which thread C waited out before it
/// had this thread, and the units it encoded; and, from thread C's ticks,
/// those skipped for units in flight (`gate_ticks`), the submits
/// that found this thread busy and left their tick armed
/// (`armed_refusals`), and the most units in flight at a hand-off. Nothing
/// allocated.
struct Load {
    started: Instant,
    calls: [u32; LOAD_SAMPLES],
    len: usize,
    units: u32,
}
impl Load {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            calls: [0; LOAD_SAMPLES],
            len: 0,
            units: 0,
        }
    }

    /// An encode call that took `took`.
    fn call(&mut self, took: Duration) {
        self.units += 1;
        if let Some(slot) = self.calls.get_mut(self.len) {
            *slot = u32::try_from(took.as_micros()).unwrap_or(u32::MAX);
            self.len += 1;
        }
    }

    fn due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= Duration::from_secs(1)
    }

    /// Trace the second that ended at `now`, with what thread C counted, and
    /// start the next.
    fn roll(&mut self, now: Instant, gate: GateCounts) {
        if tracing::enabled!(target: LOAD_TARGET, tracing::Level::DEBUG) {
            let calls = self.calls.get_mut(..self.len).unwrap_or_default();
            calls.sort_unstable();
            let at = |quantile: usize| {
                calls
                    .get((calls.len() * quantile / 100).min(calls.len().saturating_sub(1)))
                    .copied()
            };
            tracing::debug!(
                target: LOAD_TARGET,
                event = "macos_encoder_load",
                seconds = now.saturating_duration_since(self.started).as_secs_f64(),
                call_p50_us = at(50),
                call_p99_us = at(99),
                units = self.units,
                gate_ticks = gate.full,
                armed_refusals = gate.busy,
                peak_in_flight = gate.peak_in_flight,
            );
        }
        self.started = now;
        self.len = 0;
        self.units = 0;
    }
}

/// The keyframe flag of a VideoToolbox sample: it is a sync sample unless
/// it says it is not.
fn sync(sample: &CMSampleBuffer) -> bool {
    // SAFETY: the encoder's sample, read only.
    let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else {
        return true;
    };
    // SAFETY: a sample's attachments are an array of dictionaries.
    let attachments = unsafe { CFRetained::cast_unchecked::<CFArray<CFDictionary>>(attachments) };
    let Some(first) = attachments.get(0) else {
        return true;
    };
    // SAFETY: a static key.
    let key = unsafe { kCMSampleAttachmentKey_NotSync };
    !cf::value::<objc2_core_foundation::CFBoolean>(&first, key).is_some_and(|not| not.as_bool())
}

/// A unit's luma mean squared error, where its session measures it.
#[cfg(test)]
fn luma_error(sample: &CMSampleBuffer) -> Option<f64> {
    let metrics = cf::symbol(c"kVTSampleAttachmentKey_QualityMetrics")?;
    let luma = cf::symbol(c"kVTSampleAttachmentQualityMetricsKey_LumaMeanSquaredError")?;
    // SAFETY: the encoder's sample, read only.
    let attachments = unsafe { sample.sample_attachments_array(false) }?;
    // SAFETY: a sample's attachments are an array of dictionaries.
    let attachments = unsafe { CFRetained::cast_unchecked::<CFArray<CFDictionary>>(attachments) };
    let first = attachments.get(0)?;
    cf::value::<CFDictionary>(&first, metrics)
        .and_then(|metrics| cf::value::<CFNumber>(&metrics, luma))
        .and_then(|error| error.as_f64())
}

/// The most parameter sets read from a format: H.265's VPS, SPS and PPS,
/// and the few SEI units an encoder may add.
const MAX_PARAMETER_SETS: usize = 8;

/// A format description's NAL length size and, when `include_sets` is
/// set, up to `MAX_PARAMETER_SETS` parameter sets in order. They borrow
/// from `format` without allocating.
fn format_sets(
    codec: VideoCodec,
    format: &CMFormatDescription,
    include_sets: bool,
) -> Result<([&[u8]; MAX_PARAMETER_SETS], usize, usize)> {
    let read = |index: usize,
                pointer: *mut *const u8,
                size: *mut usize,
                count: *mut usize,
                length: *mut c_int| {
        // SAFETY: every out pointer is to a local or null; the format is the
        // sample's own.
        unsafe {
            match codec {
                VideoCodec::H265 => CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format, index, pointer, size, count, length,
                ),
                VideoCodec::H264 | VideoCodec::Jpeg => {
                    CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        format, index, pointer, size, count, length,
                    )
                }
            }
        }
    };
    let (mut count, mut length) = (0usize, 0 as c_int);
    cf::check(
        "reading the encoder's parameter sets",
        read(
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut count,
            &mut length,
        ),
    )?;
    let mut sets: [&[u8]; MAX_PARAMETER_SETS] = [&[]; MAX_PARAMETER_SETS];
    let count = if include_sets {
        count.min(MAX_PARAMETER_SETS)
    } else {
        0
    };
    for (index, slot) in sets.iter_mut().enumerate().take(count) {
        let (mut pointer, mut size): (*const u8, usize) = (std::ptr::null(), 0);
        cf::check(
            "reading a parameter set",
            read(
                index,
                &mut pointer,
                &mut size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ),
        )?;
        if pointer.is_null() || size == 0 || size > 64 * 1024 {
            return Err(error("invalid parameter set"));
        }
        // SAFETY: VideoToolbox's pointer into the format's own storage,
        // `size` bytes, alive while `format` is.
        *slot = unsafe { std::slice::from_raw_parts(pointer, size) };
    }
    Ok((sets, count, usize::try_from(length).unwrap_or(0)))
}

/// A sample's bytes: the block buffer's own when contiguous, else copied
/// into `scratch`.
fn bytes<'a>(sample: &'a CMSampleBuffer, scratch: &'a mut Vec<u8>) -> Result<&'a [u8]> {
    // SAFETY: the encoder's sample, read only.
    let block =
        unsafe { sample.data_buffer() }.ok_or_else(|| error("encoded sample has no data"))?;
    let (mut here, mut total, mut pointer) = (0usize, 0usize, std::ptr::null_mut());
    // SAFETY: out pointers to locals.
    let status = unsafe { block.data_pointer(0, &mut here, &mut total, &mut pointer) };
    cf::check("reading an encoded sample", status)?;
    if total > crate::codec::annexb::MAX_ACCESS_UNIT {
        return Err(error("access unit exceeds video limit"));
    }
    if here == total && !pointer.is_null() {
        // SAFETY: `total` contiguous bytes the block holds; the sample, and
        // its block, outlive the borrow.
        return Ok(unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), total) });
    }
    scratch.clear();
    scratch.resize(total, 0);
    let destination =
        NonNull::new(scratch.as_mut_ptr().cast()).ok_or_else(|| error("no buffer"))?;
    // SAFETY: `scratch` holds `total` bytes.
    let status = unsafe { block.copy_data_bytes(0, total, destination) };
    cf::check("copying an encoded sample", status)?;
    Ok(scratch)
}

/// The callback of every session, on VideoToolbox's thread.
unsafe extern "C-unwind" fn output(
    refcon: *mut c_void,
    token: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: the session's refcon is its `Shared`, alive until the session
    // is invalidated; null only for the probe, which encodes nothing.
    let Some(shared) = (unsafe { refcon.cast::<Shared>().as_ref() }) else {
        return;
    };
    // SAFETY: null or the sample, valid during the call.
    let sample = unsafe { sample.as_ref() };
    if receive(shared, token as usize as u64, status, flags, sample) {
        shared.waker.wake();
    }
}

/// One output, into the queue. Whether thread C has something to do.
fn receive(
    shared: &Shared,
    token: u64,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: Option<&CMSampleBuffer>,
) -> bool {
    let now = Instant::now();
    let mut state = shared.state.lock();
    if state.failure.is_some() {
        return false;
    }
    let Some(pending) = state.pending.pop_front() else {
        state.failure = Some("unsolicited video output".into());
        return true;
    };
    if pending.token != token {
        state.failure = Some("the hardware encoder reordered its output".into());
        return true;
    }
    if status != 0 {
        state.failure = Some(format!(
            "hardware video encoding failed (OSStatus {status})"
        ));
        return true;
    }
    let previous = state.last_output.replace(now);
    let Some(sample) = sample.filter(|_| !flags.contains(VTEncodeInfoFlags::FrameDropped)) else {
        // Dropped: what follows still decodes; a keyframe asked for is
        // asked for again.
        #[cfg(test)]
        {
            state.probe.dropped += 1;
        }
        return pending.keyframe && !std::mem::replace(&mut state.keyframe_due, true);
    };
    let keyframe = sync(sample);
    if late(pending.at, previous, now) {
        state.wait_independent = true;
        return !std::mem::replace(&mut state.keyframe_due, true);
    }
    if state.wait_independent && !keyframe {
        // A picture after a discarded one may refer to it.
        return !std::mem::replace(&mut state.keyframe_due, true);
    }
    let converted = (|| {
        // SAFETY: the sample's format, retained for the borrow below.
        let format = unsafe { sample.format_description() }
            .ok_or_else(|| error("encoded sample has no format"))?;
        let (sets, count, length) = format_sets(shared.codec, &format, keyframe)?;
        let mut scratch = std::mem::take(&mut state.sample);
        let mut out = Vec::new();
        let result = bytes(sample, &mut scratch)
            .and_then(|bytes| annex_b(shared.codec, bytes, length, &sets[..count], &mut out));
        state.sample = scratch;
        result.map(|()| out)
    })();
    match converted {
        Ok(bytes) => {
            state.wait_independent = false;
            #[cfg(test)]
            if shared.quality
                && let Some(error) = luma_error(sample)
            {
                state.probe.quality.push_back((pending.pts_us, error));
            }
            let encode_us = now.saturating_duration_since(pending.at).as_micros();
            state.ready.push_back(EncodedFrame {
                format: VideoFormat {
                    codec: shared.codec,
                    width: shared.size.0,
                    height: shared.size.1,
                    keyframe,
                    pts_us: pending.pts_us,
                    duplicate: pending.duplicate,
                    encode_us: u32::try_from(encode_us).unwrap_or(u32::MAX),
                    gop: shared.gop,
                },
                bytes,
            });
        }
        Err(failure) => state.failure = Some(failure.to_string()),
    }
    true
}
