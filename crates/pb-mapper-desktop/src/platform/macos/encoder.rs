//! VideoToolbox: a real-time hardware H.264 or H.265 session, no frame
//! reordering, an unbounded GOP (an IDR only when asked), its bitrate set
//! live. It reads the capture's IOSurface in place. Its output callback, on
//! VideoToolbox's thread, turns each length-prefixed sample into Annex B,
//! a keyframe's parameter sets in front, queues it and wakes thread C.

use super::cf;
use crate::{
    EncodedFrame, Result, VideoCodec, VideoFormat, Waker,
    codec::{
        avcc::annex_b,
        pipeline::{OUTPUT_WAIT, codec_error, late},
    },
    error,
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
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
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    ffi::{c_int, c_void},
    ptr::NonNull,
    sync::{Arc, OnceLock},
    time::Instant,
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
}

struct Shared {
    state: Mutex<State>,
    waker: Waker,
    codec: VideoCodec,
    size: (u32, u32),
    gop: u32,
}

/// A running compression session.
pub(crate) struct Encoder {
    session: CFRetained<VTCompressionSession>,
    shared: Arc<Shared>,
    bitrate: u32,
    created: Instant,
    /// The last presentation time, which only grows.
    pts_us: u64,
    next_token: u64,
    /// `{ForceKeyFrame: true}`, made once.
    keyframe: CFRetained<CFDictionary>,
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
/// where there is one, with low-latency rate control when asked.
fn specification(codec: VideoCodec, low_latency: bool) -> CFRetained<CFDictionary> {
    // SAFETY: VideoToolbox's static keys, alive for the process.
    unsafe {
        match codec {
            VideoCodec::H265 => cf::dictionary(&[(
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                cf::boolean(true),
            )]),
            VideoCodec::H264 | VideoCodec::Jpeg => {
                let latency = low_latency.then(|| cf::symbol(LOW_LATENCY)).flatten();
                let mut pairs = vec![(
                    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
                    cf::boolean(true),
                )];
                pairs.extend(latency.map(|key| (key, cf::boolean(true))));
                cf::dictionary(&pairs)
            }
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
        // H.264's low-latency rate control (macOS 11.3) first; a session
        // that refuses it, or a property under it, is made again without.
        let low_latency = codec == VideoCodec::H264 && cf::symbol(LOW_LATENCY).is_some();
        let build =
            |low_latency| Self::build(codec, width, height, fps, bitrate, waker, low_latency);
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

    fn build(
        codec: VideoCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        waker: &Waker,
        low_latency: bool,
    ) -> Result<Self> {
        // The GOP the session reads back is known once it exists; the
        // callback reads `shared`, which therefore exists first.
        let mut shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            waker: waker.clone(),
            codec,
            size: (width, height),
            gop: 0,
        });
        let session = create(codec, width, height, low_latency, Arc::as_ptr(&shared))?;
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
                let _ = set(
                    &session,
                    kVTCompressionPropertyKey_DataRateLimits,
                    &data_rate_limits(bitrate),
                );
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
        );
        // SAFETY: a static key.
        let keyframe = unsafe {
            cf::dictionary(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, cf::boolean(true))])
        };
        Ok(Self {
            session,
            shared,
            bitrate,
            created: Instant::now(),
            pts_us: 0,
            next_token: 0,
            keyframe,
        })
    }

    pub(crate) fn codec(&self) -> VideoCodec {
        self.shared.codec
    }

    /// The mean bitrate, live; nothing else changes, nothing rebuilds.
    pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        if bitrate == self.bitrate {
            return Ok(());
        }
        // SAFETY: static keys.
        unsafe {
            set(
                &self.session,
                kVTCompressionPropertyKey_AverageBitRate,
                &cf::number(i64::from(bitrate)),
            )
            .map_err(|failure| codec_error(self.shared.codec, failure))?;
            let _ = set(
                &self.session,
                kVTCompressionPropertyKey_DataRateLimits,
                &data_rate_limits(bitrate),
            );
        }
        self.bitrate = bitrate;
        Ok(())
    }

    /// Units submitted and not yet output.
    pub(crate) fn pending(&self) -> usize {
        self.shared.state.lock().pending.len()
    }

    /// A keyframe was asked for and lost: the next submit carries one.
    pub(crate) fn keyframe_due(&self) -> bool {
        std::mem::take(&mut self.shared.state.lock().keyframe_due)
    }

    /// Encode `picture`; `duplicate` when it is the last one again.
    pub(crate) fn encode(
        &mut self,
        picture: &CVPixelBuffer,
        keyframe: bool,
        duplicate: bool,
    ) -> Result<()> {
        let now = Instant::now();
        let pts_us = (now.duration_since(self.created).as_micros() as u64).max(self.pts_us + 1);
        self.pts_us = pts_us;
        self.next_token += 1;
        let token = self.next_token;
        // Queued before the call: a synchronous encoder outputs inside it.
        self.shared.state.lock().pending.push_back(Pending {
            token,
            at: now,
            duplicate,
            keyframe,
            pts_us,
        });
        let mut flags = VTEncodeInfoFlags(0);
        // SAFETY: an IOSurface-backed picture the session retains while it
        // reads; the token is an opaque number handed back to the callback.
        let status = unsafe {
            self.session.encode_frame(
                picture,
                CMTime::new(pts_us as i64, 1_000_000),
                kCMTimeInvalid,
                keyframe.then_some(&*self.keyframe),
                token as usize as *mut c_void,
                &mut flags,
            )
        };
        if status != 0 {
            let mut state = self.shared.state.lock();
            if state
                .pending
                .back()
                .is_some_and(|pending| pending.token == token)
            {
                state.pending.pop_back();
            }
            return Err(codec_error(
                self.shared.codec,
                cf::status("hardware video encoding", status),
            ));
        }
        Ok(())
    }

    /// The next unit, if one is out. A failure the callback saw, or a unit
    /// held too long, is the codec's.
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
}

impl Drop for Encoder {
    /// No callback runs once this returns; what was in flight is dropped.
    fn drop(&mut self) {
        // SAFETY: invalidated once; `shared` drops after.
        unsafe { self.session.invalidate() };
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

/// A format description's parameter sets, in order, and its NAL length
/// size. They borrow from `format`.
fn parameter_sets(codec: VideoCodec, format: &CMFormatDescription) -> Result<(Vec<&[u8]>, usize)> {
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
    let mut sets = Vec::with_capacity(count);
    for index in 0..count.min(8) {
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
        sets.push(unsafe { std::slice::from_raw_parts(pointer, size) });
    }
    Ok((sets, usize::try_from(length).unwrap_or(0)))
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
        state.keyframe_due |= pending.keyframe;
        return false;
    };
    let keyframe = sync(sample);
    if late(pending.at, previous, now) {
        state.wait_independent = true;
        state.keyframe_due = true;
        return false;
    }
    if state.wait_independent && !keyframe {
        // A picture after a discarded one may refer to it.
        state.keyframe_due = true;
        return false;
    }
    let converted = (|| {
        // SAFETY: the sample's format, retained for the borrow below.
        let format = unsafe { sample.format_description() }
            .ok_or_else(|| error("encoded sample has no format"))?;
        let (sets, length) = parameter_sets(shared.codec, &format)?;
        let mut scratch = std::mem::take(&mut state.sample);
        let mut out = Vec::new();
        let result = bytes(sample, &mut scratch).and_then(|bytes| {
            annex_b(
                shared.codec,
                bytes,
                length,
                if keyframe { &sets } else { &[] },
                &mut out,
            )
        });
        state.sample = scratch;
        result.map(|()| out)
    })();
    match converted {
        Ok(bytes) => {
            state.wait_independent = false;
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
