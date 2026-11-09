//! Desktop Duplication -> D3D11 video processor -> Media Foundation H.264 or
//! H.265.
//!
//! Raw desktop pixels stay on the GPU. Only complete encoded access units cross
//! into host memory. The GPU's own Media Foundation encoder, rather than
//! application code, implements the codec. Create, use and drop this object on
//! the capture worker's desktop thread.

use crate::{
    BackendError, Display, FailureKind, Result, VideoCodec,
    backend::H265_ENCODER_FAILED,
    codec::annexb::{MAX_ACCESS_UNIT, Nal, ParameterSets, codec_name, nal, nal_ranges},
    error,
    qos::{ENCODER_BITRATE_MAX, ENCODER_BITRATE_MIN, scaled_size},
};
use std::{
    collections::VecDeque,
    mem::ManuallyDrop,
    thread::ThreadId,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HMODULE, LUID, RECT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{
                Common::*, CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
                IDXGIAdapter1, IDXGIDevice, IDXGIFactory1,
            },
        },
        Media::MediaFoundation::*,
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            Variant::VARIANT,
        },
    },
    core::{GUID, Interface, PWSTR},
};
use windows_capture::{
    dxgi_duplication_api::{DxgiDuplicationApi, DxgiDuplicationFormat},
    monitor::Monitor,
};

/// Thread C's encoder bound decides how deep the pipeline runs; this only
/// keeps a caller without it from queueing more.
const MAX_IN_FLIGHT: usize = crate::worker::ENCODER_DEPTH;
/// An output that took longer than this is discarded, and output resumes at
/// the next IDR. A unit's time counts from when the encoder could start it:
/// see `late`.
const LATE_OUTPUT: Duration = Duration::from_millis(250);
/// A pipeline that has submitted nothing this long after it was built is
/// stuck; `unsubmitted` says on what.
const FIRST_SUBMIT_WAIT: Duration = Duration::from_secs(3);
/// An encoder that has held a unit this long is stuck.
const OUTPUT_WAIT: Duration = Duration::from_secs(2);
/// A static desktop still submits a verified repeat this often, which keeps a
/// presented frame fresh enough to authorise input.
const LIVENESS_INTERVAL: Duration = Duration::from_millis(250);
/// GOP sizes to request, longest first: keyframes come only on demand. Never
/// 0, which asks the encoder for its own default GOP rather than an endless one.
const GOP_CANDIDATES: [u32; 4] = [u32::MAX, 0x7FFF_FFFF, 65_535, 3_600];

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
        if !crate::media_foundation() {
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
    _runtime: Runtime,
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
        if !(2..=7680).contains(&max_width) || !(2..=4320).contains(&max_height) {
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
        if input_width < 2 || input_height < 2 || input_width > 7680 || input_height > 4320 {
            return Err(error("unsupported GPU capture dimensions"));
        }
        let (width, height) = match size {
            OutputSize::Fit(..) => scaled_size(input_width, input_height, max_width, max_height),
            OutputSize::Exact(..) => ((max_width & !1).max(2), (max_height & !1).max(2)),
        };
        let device = capture.device();
        let manager = device_manager(device, capture.device_context())?;
        let codec = Codec::new(device, &manager, kind, width, height, fps, bitrate)
            .map_err(|failure| codec_error(kind, failure))?;
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
                    4,
                    6,
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
            _runtime: runtime,
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
        if self._runtime.thread != std::thread::current().id() {
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
/// session, and this host offers H.265 no more. Its text still starts with
/// `H265_ENCODER_FAILED`, which is how it crosses the helper pipe.
fn codec_error(kind: VideoCodec, failure: std::io::Error) -> std::io::Error {
    if kind == VideoCodec::H265 {
        BackendError::new(
            FailureKind::EncoderFailed { codec: kind },
            format!("{H265_ENCODER_FAILED}: {failure}"),
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

fn luid_value(luid: LUID) -> u64 {
    (u64::from(luid.HighPart as u32) << 32) | u64::from(luid.LowPart)
}

/// The adapter driving display `id` (its `HMONITOR`, as `Display::id` holds it).
fn display_adapter(id: u32) -> Result<IDXGIAdapter1> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().map_err(error)? };
    for index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        for output in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(output) }) else {
                break;
            };
            let desc = unsafe { output.GetDesc().map_err(error)? };
            if desc.Monitor.0 as usize as u32 == id {
                return Ok(adapter);
            }
        }
    }
    Err(error("selected display is no longer available"))
}

/// Whether the GPU that drives display `id` has a hardware H.265 encoder this
/// pipeline can use: an MFT for that adapter that activates, takes the Main
/// 4:2:0 output and NV12 input types at 1920x1080@60 with B-frames off, and
/// starts streaming, without encoding a frame. Probed once per adapter LUID
/// and kept for the process; a display no adapter drives is not kept.
pub fn h265_available(id: u32) -> bool {
    static PROBED: parking_lot::Mutex<Vec<(u64, bool)>> = parking_lot::const_mutex(Vec::new());
    let probe = || -> Result<bool> {
        let adapter = display_adapter(id)?;
        let luid = luid_value(unsafe { adapter.GetDesc1().map_err(error)? }.AdapterLuid);
        // Held while probing: one probe per adapter, however many ask at once.
        let mut probed = PROBED.lock();
        if let Some(&(_, usable)) = probed.iter().find(|(probed, _)| *probed == luid) {
            return Ok(usable);
        }
        let started = Instant::now();
        let probe = probe_h265(&adapter);
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_h265_probe",
            usable = probe.is_ok(),
            ms = started.elapsed().as_millis() as u64,
            detail = ?probe.as_ref().err().map(ToString::to_string),
        );
        probed.push((luid, probe.is_ok()));
        Ok(probe.is_ok())
    };
    probe().unwrap_or_else(|failure| {
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_h265_probe",
            usable = false,
            detail = ?failure.to_string(),
        );
        false
    })
}

fn probe_h265(adapter: &IDXGIAdapter1) -> Result<()> {
    let _runtime = Runtime::new()?;
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(error)?;
    }
    let device = device.ok_or_else(|| error("missing D3D11 device"))?;
    let context = context.ok_or_else(|| error("missing D3D11 device context"))?;
    let manager = device_manager(&device, &context)?;
    Codec::new(
        &device,
        &manager,
        VideoCodec::H265,
        1920,
        1080,
        60,
        8_000_000,
    )
    .map(drop)
}

/// Keep the first GOP candidate the encoder accepts and reads back unchanged.
/// When it changes every one, report what it holds instead (0: nothing), so a
/// clamp to a finite GOP is visible rather than mistaken for an endless one.
fn pick_gop(mut set: impl FnMut(u32) -> bool, mut read: impl FnMut() -> Option<u32>) -> u32 {
    for gop in GOP_CANDIDATES {
        if set(gop) && read() == Some(gop) {
            return gop;
        }
    }
    read().unwrap_or(0)
}

struct Codec {
    kind: VideoCodec,
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    api: ICodecAPI,
    name: String,
    input_id: u32,
    output_id: u32,
    need_input: u32,
    have_output: u32,
    parameter_sets: ParameterSets,
    gop: u32,
}
impl Codec {
    /// The first hardware encoder of `kind` on `device`'s adapter that takes
    /// this pipeline's settings. There is no software fallback.
    fn new(
        device: &ID3D11Device,
        manager: &IMFDXGIDeviceManager,
        kind: VideoCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self> {
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: subtype(kind),
        };
        let attrs = attributes()?;
        let dxgi: IDXGIDevice = device.cast().map_err(error)?;
        let adapter = unsafe { dxgi.GetAdapter().map_err(error)?.GetDesc().map_err(error)? };
        let luid = luid_value(adapter.AdapterLuid);
        unsafe {
            attrs
                .SetBlob(&MFT_ENUM_ADAPTER_LUID, &luid.to_le_bytes())
                .map_err(error)?;
        }
        let mut raw = std::ptr::null_mut();
        let mut count = 0;
        unsafe {
            MFTEnum2(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &attrs,
                &mut raw,
                &mut count,
            )
            .map_err(error)?;
        }
        let candidates = if raw.is_null() {
            vec![]
        } else {
            let array = unsafe { std::slice::from_raw_parts_mut(raw, count as usize) };
            let candidates = array
                .iter_mut()
                .filter_map(Option::take)
                .collect::<Vec<_>>();
            unsafe {
                CoTaskMemFree(Some(raw.cast()));
            }
            candidates
        };
        let mut failures = vec![];
        for activation in candidates {
            let name = activation_name(&activation);
            match Self::activate(
                &activation,
                name.clone(),
                manager,
                kind,
                width,
                height,
                fps,
                bitrate,
            ) {
                Ok(codec) => return Ok(codec),
                Err(e) => {
                    unsafe {
                        let _ = activation.ShutdownObject();
                    }
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
        Err(error(format!(
            "No compatible hardware {} encoder on the capture GPU{}",
            codec_name(kind),
            if failures.is_empty() {
                String::new()
            } else {
                format!(": {}", failures.join("; "))
            }
        )))
    }

    #[allow(clippy::too_many_arguments)]
    fn activate(
        activation: &IMFActivate,
        name: String,
        manager: &IMFDXGIDeviceManager,
        kind: VideoCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self> {
        let transform: IMFTransform = unsafe {
            activation
                .ActivateObject()
                .map_err(|e| error(format!("activate: {e}")))?
        };
        let attrs = unsafe { transform.GetAttributes().map_err(error)? };
        unsafe {
            attrs
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .map_err(error)?;
            attrs.SetUINT32(&MF_LOW_LATENCY, 1).map_err(error)?;
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err(error("hardware encoder does not accept D3D11 surfaces"));
            }
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .map_err(|e| error(format!("D3D11 manager: {e}")))?;
        }
        let events = transform.cast().map_err(error)?;
        let api = transform.cast().map_err(error)?;
        let mut codec = Self {
            kind,
            transform,
            events,
            api,
            name,
            input_id: 0,
            output_id: 0,
            need_input: 0,
            have_output: 0,
            parameter_sets: ParameterSets::new(kind),
            gop: 0,
        };
        let mut input_ids = [0];
        let mut output_ids = [0];
        if unsafe {
            codec
                .transform
                .GetStreamIDs(&mut input_ids, &mut output_ids)
        }
        .is_ok()
        {
            codec.input_id = input_ids[0];
            codec.output_id = output_ids[0];
        }
        codec.property(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true))?;
        let b_frames = codec.property(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32));
        // B-frames would reorder output. H.264's Baseline profile forbids them
        // even on drivers that reject this optional property. H.265's Main
        // profile allows them: an encoder that implements the property must
        // turn them off and read back 0. One that does not (NVIDIA's HEVC MFT
        // answers E_NOTIMPL) has only the low-latency mode, which is required
        // above, and every output's check against reordering in `collect`.
        if kind == VideoCodec::H265
            && unsafe {
                codec
                    .api
                    .IsSupported(&CODECAPI_AVEncMPVDefaultBPictureCount)
            }
            .is_ok()
        {
            b_frames?;
            if codec.value(&CODECAPI_AVEncMPVDefaultBPictureCount) != Some(0) {
                return Err(error("B-frames could not be turned off"));
            }
        }
        let _ = codec.property(
            &CODECAPI_AVEncCommonRateControlMode,
            &VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
        );
        let _ = codec.property(&CODECAPI_AVEncCommonQualityVsSpeed, &VARIANT::from(0u32));
        // Before the media types: once streaming starts, the NVIDIA MFT still
        // accepts a GOP SetValue but keeps its default of one IDR per fps frames.
        codec.gop = codec.set_infinite_gop();
        tracing::info!(encoder = %codec.name, gop = codec.gop, "desktop video encoder GOP");
        let output = media_type(&subtype(kind), width, height, fps)?;
        let input = media_type(&MFVideoFormat_NV12, width, height, fps)?;
        let profile = match kind {
            VideoCodec::H265 => eAVEncH265VProfile_Main_420_8.0,
            _ => eAVEncH264VProfile_Base.0,
        };
        unsafe {
            output
                .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
                .map_err(error)?;
            output
                .SetUINT32(&MF_MT_MPEG2_PROFILE, profile as u32)
                .map_err(error)?;
            codec
                .transform
                .SetOutputType(codec.output_id, &output, 0)
                .map_err(|e| error(format!("output media type: {e}")))?;
            codec
                .transform
                .SetInputType(codec.input_id, &input, 0)
                .map_err(|e| error(format!("input media type: {e}")))?;
            codec
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(error)?;
            codec
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(error)?;
        }
        codec.update_header()?;
        Ok(codec)
    }
    fn property(&self, key: &GUID, value: &VARIANT) -> Result<()> {
        unsafe {
            self.api
                .SetValue(key, value)
                .map_err(|e| error(format!("codec property {key:?}: {e}")))
        }
    }
    /// What the encoder reports for `key`, when it reads it back as a number.
    fn value(&self, key: &GUID) -> Option<u32> {
        unsafe { self.api.GetValue(key) }
            .ok()
            .and_then(|value| u32::try_from(&value).ok())
    }
    /// Vendors accept a GOP setting and then clamp or ignore it, so only a
    /// value read back unchanged counts. Returns the GOP the encoder reports.
    fn set_infinite_gop(&self) -> u32 {
        pick_gop(
            |gop| {
                self.property(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(gop))
                    .is_ok()
            },
            || self.value(&CODECAPI_AVEncMPVGOPSize),
        )
    }
    fn events(&mut self) -> Result<()> {
        for _ in 0..32 {
            let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(error(e)),
            };
            unsafe {
                event.GetStatus().map_err(error)?.ok().map_err(error)?;
            }
            match unsafe { event.GetType().map_err(error)? } {
                n if n == METransformNeedInput.0 as u32 => self.need_input += 1,
                n if n == METransformHaveOutput.0 as u32 => self.have_output += 1,
                _ => {}
            }
            if self.need_input > 64 || self.have_output > MAX_IN_FLIGHT as u32 {
                return Err(error("invalid hardware encoder event queue"));
            }
        }
        Ok(())
    }
    fn output(&mut self) -> Result<Option<IMFSample>> {
        let info = unsafe {
            self.transform
                .GetOutputStreamInfo(self.output_id)
                .map_err(error)?
        };
        let sample = if info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                as u32
            != 0
        {
            None
        } else {
            if info.cbSize as usize > MAX_ACCESS_UNIT {
                return Err(error("hardware encoder output exceeds video limit"));
            }
            let sample = unsafe { MFCreateSample().map_err(error)? };
            let buffer = unsafe {
                MFCreateAlignedMemoryBuffer(
                    if info.cbSize == 0 {
                        MAX_ACCESS_UNIT as u32
                    } else {
                        info.cbSize
                    },
                    info.cbAlignment.saturating_sub(1),
                )
                .map_err(error)?
            };
            unsafe {
                sample.AddBuffer(&buffer).map_err(error)?;
            }
            Some(sample)
        };
        let mut output = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.output_id,
            pSample: ManuallyDrop::new(sample),
            ..Default::default()
        };
        let mut status = 0;
        let result = unsafe {
            self.transform
                .ProcessOutput(0, std::slice::from_mut(&mut output), &mut status)
        };
        let sample = unsafe { ManuallyDrop::take(&mut output.pSample) };
        unsafe {
            ManuallyDrop::drop(&mut output.pEvents);
        }
        match result {
            Ok(()) => {
                self.have_output -= 1;
                Ok(sample)
            }
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                let output = unsafe {
                    self.transform
                        .GetOutputAvailableType(self.output_id, 0)
                        .map_err(error)?
                };
                unsafe {
                    self.transform
                        .SetOutputType(self.output_id, &output, 0)
                        .map_err(error)?;
                }
                self.update_header()?;
                Ok(None)
            }
            Err(e) => Err(error(e)),
        }
    }
    fn update_header(&mut self) -> Result<()> {
        let media_type = unsafe {
            self.transform
                .GetOutputCurrentType(self.output_id)
                .map_err(error)?
        };
        if let Ok(size) = unsafe { media_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) } {
            if size > 64 * 1024 {
                return Err(error("sequence header exceeds limit"));
            }
            let mut bytes = vec![0; size as usize];
            unsafe {
                media_type
                    .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut bytes, None)
                    .map_err(error)?;
            }
            if bytes.starts_with(&[0, 0, 0, 1]) || bytes.starts_with(&[0, 0, 1]) {
                self.parameter_sets.observe(&bytes, &nal_ranges(&bytes)?)?;
            }
        }
        Ok(())
    }
    /// The access unit as sent, and whether it is a keyframe. MF's elementary
    /// H.264 and H.265 types produce Annex B; refuse an unexpected format
    /// instead of shipping undecodable length-prefixed data to phones. A
    /// keyframe that comes without its parameter sets gets the last ones,
    /// in-band or from the media type, in front.
    fn annex_b(&mut self, bytes: Vec<u8>) -> Result<(Vec<u8>, bool)> {
        let units = nal_ranges(&bytes)?;
        self.parameter_sets.observe(&bytes, &units)?;
        let keyframe = units
            .iter()
            .any(|&(start, _)| nal(self.kind, bytes[start]) == Nal::Irap);
        if !keyframe || self.parameter_sets.carried(&bytes, &units) {
            return Ok((bytes, keyframe));
        }
        if !self.parameter_sets.complete() {
            self.update_header()?;
        }
        Ok((self.parameter_sets.prepend(&bytes, &units)?, true))
    }
}
impl Drop for Codec {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
        }
    }
}

struct Converter {
    device: ID3D11VideoDevice,
    context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}
impl Converter {
    fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        input_width: u32,
        input_height: u32,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<Self> {
        let device: ID3D11VideoDevice = device.cast().map_err(error)?;
        let context: ID3D11VideoContext = context.cast().map_err(error)?;
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: fps,
                Denominator: 1,
            },
            InputWidth: input_width,
            InputHeight: input_height,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: fps,
                Denominator: 1,
            },
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let enumerator = unsafe {
            device
                .CreateVideoProcessorEnumerator(&desc)
                .map_err(error)?
        };
        let processor = unsafe { device.CreateVideoProcessor(&enumerator, 0).map_err(error)? };
        unsafe {
            context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            context.VideoProcessorSetStreamSourceRect(
                &processor,
                0,
                true,
                Some(&RECT {
                    left: 0,
                    top: 0,
                    right: input_width as i32,
                    bottom: input_height as i32,
                }),
            );
            context.VideoProcessorSetStreamDestRect(
                &processor,
                0,
                true,
                Some(&RECT {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                }),
            );
        }
        // Full-range RGB -> studio-range BT.709 NV12; state the matching values
        // on the encoded media type so hardware decoders render consistent color.
        let context1: ID3D11VideoContext1 = context.cast().map_err(error)?;
        unsafe {
            context1.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            context1.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            );
        }
        Ok(Self {
            device,
            context,
            enumerator,
            processor,
        })
    }
    fn convert(&self, source: &ID3D11Texture2D, sample: &IMFSample) -> Result<()> {
        let buffer = unsafe { sample.GetBufferByIndex(0).map_err(error)? };
        let buffer: IMFDXGIBuffer = buffer.cast().map_err(error)?;
        let mut raw = std::ptr::null_mut();
        unsafe {
            buffer
                .GetResource(&ID3D11Texture2D::IID, &mut raw)
                .map_err(error)?;
        }
        let texture = unsafe { ID3D11Texture2D::from_raw(raw) };
        let subresource = unsafe { buffer.GetSubresourceIndex().map_err(error)? };
        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            texture.GetDesc(&mut texture_desc);
        }
        let output_desc = if texture_desc.ArraySize > 1 {
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2DARRAY,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2DArray: D3D11_TEX2D_ARRAY_VPOV {
                        MipSlice: subresource % texture_desc.MipLevels.max(1),
                        FirstArraySlice: subresource / texture_desc.MipLevels.max(1),
                        ArraySize: 1,
                    },
                },
            }
        } else {
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV {
                        MipSlice: subresource,
                    },
                },
            }
        };
        let mut input = None;
        let mut output = None;
        unsafe {
            self.device
                .CreateVideoProcessorInputView(
                    source,
                    &self.enumerator,
                    &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                        FourCC: 0,
                        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_VPIV {
                                MipSlice: 0,
                                ArraySlice: 0,
                            },
                        },
                    },
                    Some(&mut input),
                )
                .map_err(error)?;
            self.device
                .CreateVideoProcessorOutputView(
                    &texture,
                    &self.enumerator,
                    &output_desc,
                    Some(&mut output),
                )
                .map_err(error)?;
        }
        let output = output.ok_or_else(|| error("missing NV12 processor output"))?;
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(input),
            ..Default::default()
        };
        let result = unsafe {
            self.context.VideoProcessorBlt(
                &self.processor,
                &output,
                0,
                std::slice::from_ref(&stream),
            )
        };
        unsafe {
            ManuallyDrop::drop(&mut stream.pInputSurface);
        }
        result.map_err(error)
    }
}

fn attributes() -> Result<IMFAttributes> {
    let mut attributes = None;
    unsafe {
        MFCreateAttributes(&mut attributes, 4).map_err(error)?;
    }
    attributes.ok_or_else(|| error("missing media attributes"))
}
fn media_type(subtype: &GUID, width: u32, height: u32, fps: u32) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType().map_err(error)? };
    unsafe {
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(error)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype).map_err(error)?;
        media_type
            .SetUINT64(
                &MF_MT_FRAME_SIZE,
                (u64::from(width) << 32) | u64::from(height),
            )
            .map_err(error)?;
        media_type
            .SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)
            .map_err(error)?;
        media_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
            .map_err(error)?;
        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(error)?;
        media_type
            .SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)
            .map_err(error)?;
        media_type
            .SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)
            .map_err(error)?;
        media_type
            .SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)
            .map_err(error)?;
    }
    Ok(media_type)
}
fn subtype(kind: VideoCodec) -> GUID {
    match kind {
        VideoCodec::H265 => MFVideoFormat_HEVC,
        _ => MFVideoFormat_H264,
    }
}
fn activation_name(activation: &IMFActivate) -> String {
    let mut raw = PWSTR::null();
    let mut len = 0;
    if unsafe { activation.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut raw, &mut len) }
        .is_err()
    {
        return "hardware encoder".into();
    }
    let name = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(raw.0, len as usize)) };
    unsafe {
        CoTaskMemFree(Some(raw.0.cast()));
    }
    name
}
fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer().map_err(error)? };
    let mut raw = std::ptr::null_mut();
    let mut len = 0;
    unsafe {
        buffer.Lock(&mut raw, None, Some(&mut len)).map_err(error)?;
    }
    let result = if len as usize > MAX_ACCESS_UNIT || raw.is_null() || len == 0 {
        Err(error("invalid access unit size"))
    } else {
        Ok(unsafe { std::slice::from_raw_parts(raw, len as usize) }.to_vec())
    };
    unsafe {
        buffer.Unlock().map_err(error)?;
    }
    result
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
    #[test]
    fn gop_is_trusted_only_when_read_back_unchanged() {
        // An encoder that accepts every value and holds `holds(value)`.
        let gop_with = |holds: fn(u32) -> u32| {
            let held = std::cell::Cell::new(0);
            pick_gop(
                |gop| {
                    held.set(holds(gop));
                    true
                },
                || Some(held.get()),
            )
        };
        assert_eq!(gop_with(|gop| gop), u32::MAX);
        assert_eq!(gop_with(|gop| gop.min(65_535)), 65_535);
        // Clamped below every candidate: report the clamp, not a candidate.
        assert_eq!(gop_with(|gop| gop.min(300)), 300);
        // Nothing reads back: unknown, never assumed endless.
        assert_eq!(pick_gop(|_| true, || None), 0);
    }
}
