//! Desktop Duplication -> D3D11 video processor -> Media Foundation H.264 or
//! H.265.
//!
//! Raw desktop pixels stay on the GPU. Only complete encoded access units cross
//! into host memory. The GPU's own Media Foundation encoder, rather than
//! application code, implements the codec. Create, use and drop this object on
//! the capture worker's desktop thread.

mod codec;
mod converter;
mod probe;

pub use probe::h265_available;

use crate::{
    BackendError, Display, FailureKind, Result, VideoCodec, error,
    qos::{ENCODER_BITRATE_MAX, ENCODER_BITRATE_MIN, scaled_size},
};
use codec::{Codec, EncoderSettings, attributes, media_type, sample_bytes};
use converter::Converter;
use std::{
    collections::VecDeque,
    thread::ThreadId,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Graphics::{
            Direct3D11::*,
            Dxgi::{Common::*, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO},
        },
        Media::MediaFoundation::*,
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
            Variant::VARIANT,
        },
    },
    core::Interface,
};
use windows_capture::{
    dxgi_duplication_api::{DxgiDuplicationApi, DxgiDuplicationFormat},
    monitor::Monitor,
};

/// Thread C's encoder bound decides how deep the pipeline runs; this only
/// keeps a caller without it from queueing more.
const MAX_IN_FLIGHT: usize = crate::worker::ENCODER_DEPTH;
/// The NV12 samples the scaler writes into: allocated up front, and at most.
const SAMPLES: (u32, u32) = (4, 6);
const _: () = assert!(
    MAX_IN_FLIGHT <= SAMPLES.1 as usize,
    "the sample pool holds every unit in flight"
);
/// The largest capture, and the largest encoded size.
const MAX_SIZE: (u32, u32) = (7680, 4320);
/// An output that took longer than this is discarded, and output resumes at
/// the next IDR. A unit's time counts from when the encoder could start it:
/// see `late`.
const LATE_OUTPUT: Duration = Duration::from_millis(250);
/// A pipeline that has submitted nothing this long after it was built is
/// stuck; `unsubmitted` says on what.
const FIRST_SUBMIT_WAIT: Duration = Duration::from_secs(3);
/// An encoder that has held a unit this long is stuck.
const OUTPUT_WAIT: Duration = Duration::from_secs(2);
const _: () = assert!(
    LATE_OUTPUT.as_millis() < OUTPUT_WAIT.as_millis(),
    "a late unit is discarded before the encoder counts as stalled"
);
/// A static desktop still submits a verified repeat this often, which keeps a
/// presented frame fresh enough to authorise input.
const LIVENESS_INTERVAL: Duration = Duration::from_millis(250);

pub struct Encoded {
    pub codec: VideoCodec,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    /// False only for a new desktop presentation; static liveness repeats do
    /// not count towards the advertised capture/display performance target.
    pub duplicate: bool,
    /// Monotonic capture timestamp relative to this encoder's creation.
    pub pts_us: u64,
    /// Submit to output, in microseconds.
    pub encode_us: u32,
    /// The encoder's `Statistics::gop`.
    pub gop: u32,
}

impl From<Encoded> for crate::EncodedFrame {
    fn from(image: Encoded) -> Self {
        Self {
            format: crate::VideoFormat {
                codec: image.codec,
                width: image.width,
                height: image.height,
                keyframe: image.keyframe,
                pts_us: image.pts_us,
                duplicate: image.duplicate,
                encode_us: image.encode_us,
                gop: image.gop,
            },
            bytes: image.bytes,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Statistics {
    pub unique_captures: u64,
    pub submitted: u64,
    pub encoded: u64,
    pub unique_encoded: u64,
    pub bytes: u64,
    pub max_in_flight: usize,
    pub encode_time_us: u64,
    /// The GOP size the encoder reads back: a `GOP_CANDIDATES` entry, or what
    /// it clamped them to. 0 when it reports none.
    pub gop: u32,
}

struct Runtime {
    thread: ThreadId,
}
impl Runtime {
    /// Every Media Foundation call follows this one. The DLL is delay-loaded,
    /// so without it `MFStartup` would raise rather than fail.
    fn new() -> Result<Self> {
        if !super::mf::media_foundation() {
            return Err(error("Windows Media Foundation is not installed"));
        }
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(error)?;
        }
        if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            unsafe {
                CoUninitialize();
            }
            return Err(error(e));
        }
        Ok(Self {
            thread: std::thread::current().id(),
        })
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            // The public calls reject moves to another thread. Never balance a
            // COM initialization on an unrelated thread during an erroneous drop.
            if self.thread == std::thread::current().id() {
                CoUninitialize();
            }
        }
    }
}

/// Bounded hardware encoder. The caller paces: `submit` sends at most one
/// capture per call, and `drain` collects finished output. `None` means no
/// access unit is ready yet; callers must continue servicing input/stop while
/// waiting, rather than busy-spinning.
pub struct Encoder {
    capture: DxgiDuplicationApi,
    codec: Codec,
    converter: Converter,
    allocator: IMFVideoSampleAllocatorEx,
    _manager: IMFDXGIDeviceManager,
    latest: Option<ID3D11Texture2D>,
    latest_pending: bool,
    pending: VecDeque<(u64, Instant, bool)>,
    /// When the last output was collected, discarded or not.
    last_output: Option<Instant>,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    created: Instant,
    last_submit: Option<Instant>,
    last_present: i64,
    force_keyframe: bool,
    wait_independent_output: bool,
    stats: Statistics,
    runtime: Runtime,
}

/// The encoded size: the capture fitted inside these bounds, keeping its aspect
/// ratio and never upscaling (a session), or exactly this size.
#[derive(Clone, Copy)]
enum OutputSize {
    Fit(u32, u32),
    Exact(u32, u32),
}

impl Encoder {
    /// Scale on the D3D11 video processor before hardware encoding, to fit
    /// inside `max_width` × `max_height` without upscaling. The latest captured
    /// desktop texture remains at native resolution; no CPU readback.
    pub fn new_with_size(
        display: &Display,
        codec: VideoCodec,
        fps: u32,
        bitrate: u32,
        max_width: u32,
        max_height: u32,
    ) -> Result<Self> {
        Self::build(
            display,
            codec,
            fps,
            bitrate,
            OutputSize::Fit(max_width, max_height),
        )
    }

    /// Encode exactly `width` × `height` (rounded down to even), upscaling a
    /// smaller display: `video_bench` measures a resolution the display does
    /// not have. A session never upscales.
    pub fn new_exact(
        display: &Display,
        codec: VideoCodec,
        fps: u32,
        bitrate: u32,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::build(
            display,
            codec,
            fps,
            bitrate,
            OutputSize::Exact(width, height),
        )
    }

    fn build(
        display: &Display,
        kind: VideoCodec,
        fps: u32,
        bitrate: u32,
        size: OutputSize,
    ) -> Result<Self> {
        if kind == VideoCodec::Jpeg {
            return Err(error("the hardware encoder encodes H.264 or H.265"));
        }
        if !(1..=120).contains(&fps)
            || !(ENCODER_BITRATE_MIN..=ENCODER_BITRATE_MAX).contains(&bitrate)
        {
            return Err(error("video rate outside supported bounds"));
        }
        let (OutputSize::Fit(max_width, max_height) | OutputSize::Exact(max_width, max_height)) =
            size;
        if !(2..=MAX_SIZE.0).contains(&max_width) || !(2..=MAX_SIZE.1).contains(&max_height) {
            return Err(error("video size outside supported bounds"));
        }
        let runtime = Runtime::new()?;
        let monitor = Monitor::enumerate()
            .map_err(error)?
            .into_iter()
            .find(|m| m.as_raw_hmonitor() as usize as u32 == display.id)
            .ok_or_else(|| {
                std::io::Error::from(BackendError::new(
                    FailureKind::DisplayLost,
                    "selected GPU output disappeared",
                ))
            })?;
        let capture = DxgiDuplicationApi::new_options(monitor, &[DxgiDuplicationFormat::Bgra8])
            .map_err(error)?;
        let desc = capture.duplication_desc();
        if desc.Rotation != DXGI_MODE_ROTATION_IDENTITY {
            return Err(error(
                "GPU video capture does not yet support a rotated display",
            ));
        }
        let input_width = desc.ModeDesc.Width;
        let input_height = desc.ModeDesc.Height;
        if input_width < 2
            || input_height < 2
            || input_width > MAX_SIZE.0
            || input_height > MAX_SIZE.1
        {
            return Err(error("unsupported GPU capture dimensions"));
        }
        let (width, height) = match size {
            OutputSize::Fit(..) => scaled_size(input_width, input_height, max_width, max_height),
            OutputSize::Exact(..) => ((max_width & !1).max(2), (max_height & !1).max(2)),
        };
        let device = capture.device();
        let manager = device_manager(device, capture.device_context())?;
        let settings = EncoderSettings {
            kind,
            width,
            height,
            fps,
            bitrate,
        };
        let codec =
            Codec::new(device, &manager, settings).map_err(|failure| codec_error(kind, failure))?;
        let converter = Converter::new(
            device,
            capture.device_context(),
            desc.ModeDesc.Width,
            desc.ModeDesc.Height,
            width,
            height,
            fps,
        )?;
        let mut allocator = std::ptr::null_mut();
        unsafe {
            MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut allocator)
                .map_err(error)?;
        }
        let allocator = unsafe { IMFVideoSampleAllocatorEx::from_raw(allocator) };
        let attributes = attributes()?;
        unsafe {
            attributes
                .SetUINT32(&MF_SA_D3D11_BINDFLAGS, D3D11_BIND_RENDER_TARGET.0 as u32)
                .map_err(error)?;
            allocator.SetDirectXManager(&manager).map_err(error)?;
            allocator
                .InitializeSampleAllocatorEx(
                    SAMPLES.0,
                    SAMPLES.1,
                    &attributes,
                    &media_type(&MFVideoFormat_NV12, width, height, fps)?,
                )
                .map_err(error)?;
        }
        let now = Instant::now();
        let stats = Statistics {
            gop: codec.gop,
            ..Statistics::default()
        };
        Ok(Self {
            capture,
            codec,
            converter,
            allocator,
            _manager: manager,
            latest: None,
            latest_pending: false,
            pending: VecDeque::with_capacity(MAX_IN_FLIGHT),
            last_output: None,
            width,
            height,
            fps,
            bitrate,
            created: now,
            last_submit: None,
            last_present: 0,
            force_keyframe: true,
            wait_independent_output: true,
            stats,
            runtime,
        })
    }

    pub fn name(&self) -> &str {
        &self.codec.name
    }
    pub fn codec(&self) -> VideoCodec {
        self.codec.kind
    }
    pub fn statistics(&self) -> Statistics {
        self.stats.clone()
    }

    /// Apply a QoS rate. Only the mean bitrate reaches the encoder, and only
    /// when it changed. The pace is the caller's own: the media type, nominal
    /// frame rate and GOP never change, so a rate change never rebuilds.
    pub fn set_rate(&mut self, rate: crate::VideoRate) -> Result<()> {
        self.check_thread()?;
        if !(ENCODER_BITRATE_MIN..=ENCODER_BITRATE_MAX).contains(&rate.bitrate) {
            return Err(error("video bitrate outside supported bounds"));
        }
        if self.bitrate == rate.bitrate {
            return Ok(());
        }
        self.codec
            .property(
                &CODECAPI_AVEncCommonMeanBitRate,
                &VARIANT::from(rate.bitrate),
            )
            .map_err(|failure| codec_error(self.codec.kind, failure))?;
        self.bitrate = rate.bitrate;
        Ok(())
    }

    fn check_thread(&self) -> Result<()> {
        if self.runtime.thread != std::thread::current().id() {
            return Err(error("video encoder must remain on its capture thread"));
        }
        Ok(())
    }

    /// Complete previously submitted work even while network admission is
    /// closed. This never acquires a desktop texture or calls ProcessInput.
    /// A stuck pipeline fails, blamed on the encoder or on the capture.
    pub fn drain(&mut self) -> Result<Option<Encoded>> {
        self.check_thread()?;
        let kind = self.codec.kind;
        let ready = self
            .collect()
            .map_err(|failure| codec_error(kind, failure))?;
        if self.stats.submitted == 0 && self.created.elapsed() > FIRST_SUBMIT_WAIT {
            return Err(unsubmitted(
                kind,
                self.codec.need_input > 0,
                self.latest.is_some(),
            ));
        }
        if self
            .pending
            .front()
            .is_some_and(|(_, at, _)| at.elapsed() > OUTPUT_WAIT)
        {
            return Err(codec_error(kind, error("hardware video encoder stalled")));
        }
        Ok(ready)
    }

    fn collect(&mut self) -> Result<Option<Encoded>> {
        self.codec.events()?;
        let mut ready = None;
        if self.codec.have_output > 0 {
            let sample = self.codec.output()?;
            if let Some(sample) = sample {
                let (pts_us, at, duplicate) = self
                    .pending
                    .pop_front()
                    .ok_or_else(|| error("unsolicited video output"))?;
                if unsafe { sample.GetSampleTime().map_err(error)? } != (pts_us * 10) as i64 {
                    return Err(error(
                        "hardware encoder reordered input despite B-frames being off",
                    ));
                }
                let bytes = sample_bytes(&sample)?;
                let (bytes, keyframe) = self.codec.annex_b(bytes)?;
                let now = Instant::now();
                let previous = self.last_output.replace(now);
                // A genuinely late codec output is not a newly captured picture.
                if late(at, previous, now) {
                    self.wait_independent_output = true;
                    self.force_keyframe = true;
                } else if self.wait_independent_output && !keyframe {
                    // A fresh P-frame may still reference an expired output
                    // discarded above. Do not expose that broken GOP downstream.
                    self.force_keyframe = true;
                } else {
                    self.wait_independent_output = false;
                    self.stats.encoded += 1;
                    self.stats.unique_encoded += u64::from(!duplicate);
                    self.stats.bytes += bytes.len() as u64;
                    let encode_us = now.saturating_duration_since(at).as_micros();
                    self.stats.encode_time_us += encode_us as u64;
                    ready = Some(Encoded {
                        codec: self.codec.kind,
                        bytes,
                        width: self.width,
                        height: self.height,
                        keyframe,
                        duplicate,
                        pts_us,
                        encode_us: u32::try_from(encode_us).unwrap_or(u32::MAX),
                        gop: self.stats.gop,
                    });
                }
                // Some hardware MFTs issue NeedInput only when ProcessOutput
                // releases their previous output. Refill in this same call
                // when that notification is already available.
                self.codec.events()?;
            }
        }
        Ok(ready)
    }

    /// Samples submitted and not yet output. The worker's submit gate counts
    /// them, so a slow encoder backs capture off instead of queueing it.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Capture the newest desktop image and submit at most one sample; the
    /// caller paces, and this never waits. Call `drain` first, which collects
    /// the encoder's input requests. Returns false without submitting while the
    /// encoder has no room (`MAX_IN_FLIGHT`, no input request, no free sample),
    /// or when nothing is worth sending: no new image, no keyframe due, and a
    /// submit within `LIVENESS_INTERVAL`. A requested keyframe stays requested
    /// until a sample carries it.
    pub fn submit(&mut self, force_keyframe: bool) -> Result<bool> {
        self.check_thread()?;
        self.force_keyframe |= force_keyframe;
        if self.pending.len() >= MAX_IN_FLIGHT || self.codec.need_input == 0 {
            return Ok(false);
        }
        self.latest_pending |= self.acquire()?;
        // Static desktops still send a fresh, verified liveness frame. These
        // repetitions are excluded from unique_captures and benchmark FPS.
        if !self.latest_pending
            && !self.force_keyframe
            && self
                .last_submit
                .is_some_and(|at| at.elapsed() < LIVENESS_INTERVAL)
        {
            return Ok(false);
        }
        let Some(texture) = &self.latest else {
            return Ok(false);
        };
        let sample = match unsafe { self.allocator.AllocateSample() } {
            Ok(sample) => sample,
            Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => return Ok(false),
            Err(e) => return Err(error(e)),
        };
        self.converter.convert(texture, &sample)?;
        let now = Instant::now();
        let pts_us = now.duration_since(self.created).as_micros() as u64;
        // Variable frame rate: a sample lasts the real interval since the
        // previous one, while MF_MT_FRAME_RATE stays nominal. Pace changes
        // therefore never touch the media type or rebuild the encoder.
        let duration = self.last_submit.map_or(self.frame_period(), |last| {
            now.duration_since(last)
                .clamp(self.frame_period(), Duration::from_secs(1))
        });
        unsafe {
            sample.SetSampleTime((pts_us * 10) as i64).map_err(error)?;
            sample
                .SetSampleDuration((duration.as_nanos() / 100) as i64)
                .map_err(error)?;
        }
        let kind = self.codec.kind;
        if self.force_keyframe {
            self.codec
                .property(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32))
                .map_err(|failure| codec_error(kind, failure))?;
        }
        unsafe {
            self.codec
                .transform
                .ProcessInput(self.codec.input_id, &sample, 0)
                .map_err(|failure| codec_error(kind, error(failure)))?;
        }
        self.codec.need_input -= 1;
        self.force_keyframe = false;
        self.last_submit = Some(now);
        self.pending.push_back((pts_us, now, !self.latest_pending));
        self.latest_pending = false;
        self.stats.submitted += 1;
        self.stats.max_in_flight = self.stats.max_in_flight.max(self.pending.len());
        Ok(true)
    }

    /// Copy a new desktop image, if there is one, into `latest`, and release
    /// the duplication frame at once. A frame held until the next acquire (as
    /// windows-capture's `acquire_next_frame` does) keeps DXGI from
    /// accumulating the next update meanwhile, so an acquire at the next tick
    /// races that update and misses about every other one.
    fn acquire(&mut self) -> Result<bool> {
        let duplication = self.capture.duplication();
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        match unsafe { duplication.AcquireNextFrame(0, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(e) => return Err(error(e)),
        }
        let copied = (|| {
            let present = info.LastPresentTime;
            // Zero: only the pointer moved.
            if present == 0 || present == self.last_present {
                return Ok(false);
            }
            let frame: ID3D11Texture2D = resource
                .as_ref()
                .ok_or_else(|| error("missing desktop frame"))?
                .cast()
                .map_err(error)?;
            if self.latest.is_none() {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe { frame.GetDesc(&mut desc) };
                desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
                desc.MiscFlags = 0;
                desc.CPUAccessFlags = 0;
                desc.Usage = D3D11_USAGE_DEFAULT;
                let mut texture = None;
                unsafe {
                    self.capture
                        .device()
                        .CreateTexture2D(&desc, None, Some(&mut texture))
                        .map_err(error)?;
                }
                self.latest = texture;
            }
            let latest = self
                .latest
                .as_ref()
                .ok_or_else(|| error("missing capture texture"))?;
            unsafe { self.capture.device_context().CopyResource(latest, &frame) };
            self.last_present = present;
            self.stats.unique_captures += 1;
            Ok(true)
        })();
        let released = unsafe { duplication.ReleaseFrame() }.map_err(error);
        copied.and_then(|copied| released.map(|()| copied))
    }

    fn frame_period(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 / u64::from(self.fps))
    }
}

/// Whether an output collected at `now`, of a unit submitted at `submitted`,
/// is later than `LATE_OUTPUT`. The encoder serves units in order, so one
/// queued behind others starts no earlier than the output before it, at
/// `previous`. The wait behind them, which thread C's `ENCODER_DEPTH` sets, is
/// not lateness: measured from the submit, a throughput-limited encoder at
/// that depth would be late on every unit, its IDRs included, and its picture
/// would freeze.
fn late(submitted: Instant, previous: Option<Instant>, now: Instant) -> bool {
    let started = previous.map_or(submitted, |previous| previous.max(submitted));
    now.saturating_duration_since(started) > LATE_OUTPUT
}

/// An error of the hardware codec itself, rather than of the capture or the
/// scaler around it. An H.265 one is `EncoderFailed`: thread C then ends the
/// session, and this host offers H.265 no more. Its kind crosses the
/// helper pipe as it is.
fn codec_error(kind: VideoCodec, failure: std::io::Error) -> std::io::Error {
    if kind == VideoCodec::H265 {
        BackendError::new(
            FailureKind::EncoderFailed { codec: kind },
            failure.to_string(),
        )
        .into()
    } else {
        failure
    }
}

/// Why an encoder took no input in its first `FIRST_SUBMIT_WAIT`. It asked
/// for input and capture had no desktop image to give it: capture is stuck,
/// and is retried. It never asked, or had an image and no free sample for it:
/// the encoder is stuck, a codec error. The probe encodes no frame, so this is
/// where an H.265 encoder that never asks for input is turned off.
fn unsubmitted(kind: VideoCodec, asked: bool, image: bool) -> std::io::Error {
    if asked && !image {
        error("desktop capture delivered no image")
    } else {
        codec_error(kind, error("hardware video encoder stalled"))
    }
}

/// A DXGI device manager over `device`, which the encoder's own threads use.
fn device_manager(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
) -> Result<IMFDXGIDeviceManager> {
    let protection: ID3D11Multithread = context.cast().map_err(error)?;
    unsafe {
        let _ = protection.SetMultithreadProtected(true);
    }
    let mut token = 0;
    let mut manager = None;
    unsafe {
        MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(error)?;
    }
    let manager = manager.ok_or_else(|| error("missing DXGI device manager"))?;
    unsafe {
        manager.ResetDevice(device, token).map_err(error)?;
    }
    Ok(manager)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_encoder_that_never_asks_for_input_is_blamed_not_capture() {
        let h265_failed = |asked, image| {
            crate::failure_kind(&unsubmitted(VideoCodec::H265, asked, image))
                == Some(FailureKind::EncoderFailed {
                    codec: VideoCodec::H265,
                })
        };
        // Asked, and capture had no image: capture is stuck, and is retried
        // with H.265 still on.
        assert!(!h265_failed(true, false));
        // Never asked, or no sample for the image: the encoder's, so H.265 is
        // turned off and the viewer reconnects to H.264.
        assert!(h265_failed(false, false));
        assert!(h265_failed(true, true));
    }
    #[test]
    fn a_unit_queued_behind_a_slow_encoder_is_not_late() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        // A serial encoder takes 70 ms a unit. Thread C fills it at 60 fps,
        // then submits one as each comes out: from its submit, every unit
        // after the first four takes 280 ms.
        let mut queued: VecDeque<u64> = [0, 17, 33, 50].into();
        assert_eq!(queued.len(), crate::worker::ENCODER_DEPTH);
        let mut previous = None;
        for unit in 1..=20 {
            let out = 70 * unit;
            let submitted = queued.pop_front().unwrap();
            assert!(!late(at(submitted), previous, at(out)), "unit {unit}");
            previous = Some(at(out));
            queued.push_back(out);
        }
        // A stuck encoder is still late, behind another unit or not.
        let submitted = queued.pop_front().unwrap();
        assert!(late(at(submitted), previous, at(70 * 20 + 251)));
        assert!(late(at(5_000), previous, at(5_251)));
    }
}
