//! Desktop Duplication -> D3D11 video processor -> Media Foundation H.264 or
//! H.265.
//!
//! Raw desktop pixels stay on the GPU. Only complete encoded access units cross
//! into host memory. The GPU's own Media Foundation encoder, rather than
//! application code, implements the codec. Create, use and drop this object on
//! the capture worker's desktop thread.
//!
//! A new picture is dated by when the display presented it (DXGI's
//! `LastPresentTime`, a performance counter reading), which thread C anchors
//! its pacing on and its sample time follows.
//!
//! Given a waker, the encoder's events wake its caller: each output, and the
//! input request a `Submit::Waiting` waits for (`codec.rs`).

mod codec;
mod converter;
mod probe;

pub use crate::codec::sps::SpsReorder;
pub use probe::h265_available;

use super::gpu_priority::GpuPriority;
use crate::{
    BackendError, DRAIN_POLL, Display, FailureKind, Presented, Result, VideoCodec, Waker,
    codec::{
        annexb::codec_name,
        pipeline::{FIRST_SUBMIT_WAIT, LIVENESS_INTERVAL, OUTPUT_WAIT, codec_error, late},
    },
    error,
    qos::{ENCODER_BITRATE_MAX, ENCODER_BITRATE_MIN, scaled_size},
    scheduler::{Waiter, Wake},
};
use codec::{Codec, EncoderSettings, attributes, media_type, sample_bytes};
use converter::Converter;
use std::{
    collections::VecDeque,
    sync::OnceLock,
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
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
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
/// The oldest present that still dates a picture.
const PRESENT_MAX_AGE: Duration = Duration::from_secs(1);
/// `Statistics::pickup`'s buckets: the upper bounds, in microseconds, of all
/// but the last, which holds the rest.
pub const PICKUP_BOUNDS_US: [u32; 7] = [100, 250, 500, 1_000, 2_000, 4_000, 8_000];
pub const PICKUP_BUCKETS: usize = PICKUP_BOUNDS_US.len() + 1;

/// An H.264 profile family to ask the encoder for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum H264Profile {
    /// Baseline: CAVLC, no 8x8 transform.
    Base,
    /// Constrained High, else High (CABAC and the 8x8 transform), where
    /// nothing rules it out; else Baseline.
    #[default]
    High,
}

/// How the hardware encoder is set up beyond its codec, size and rates.
/// A session takes the defaults; `video_bench` varies each, so that a
/// default changes only by measurement.
#[derive(Clone, Copy, Debug, Default)]
pub struct EncoderOptions {
    /// The H.264 profile family to ask for first.
    pub h264_profile: H264Profile,
    /// `CODECAPI_AVEncCommonQualityVsSpeed`, 0 (speed) to 100 (quality).
    pub quality_vs_speed: u32,
    /// Set `CODECAPI_AVScenarioInfo` to display remoting.
    pub display_remoting: bool,
    /// Raise the capture device's GPU thread priority and hold this
    /// process's GPU scheduling class at `HIGH` while the encoder lives
    /// (`gpu_priority.rs`).
    pub gpu_priority: bool,
}

/// What a hardware encoder took of its settings, read back as it opened:
/// what `desktop_video_encoder_opened` logs and `Statistics::opened` keeps.
/// `None` where the encoder reads nothing back.
#[derive(Clone, Debug, Default)]
pub struct EncoderOpened {
    /// The capture adapter's PCI vendor ID: 0x10de NVIDIA, 0x8086 Intel,
    /// 0x1002 AMD.
    pub vendor: u32,
    /// `CODECAPI_AVEncCommonRateControlMode`: 0 is CBR, which is asked for.
    pub rate_control: Option<u32>,
    /// `CODECAPI_AVEncCommonQualityVsSpeed`.
    pub quality_vs_speed: Option<u32>,
    /// `CODECAPI_AVScenarioInfo`: 1 is display remoting.
    pub scenario: Option<u32>,
    /// `CODECAPI_AVEncMPVDefaultBPictureCount`; `None` from an encoder that
    /// does not implement it.
    pub b_frames: Option<u32>,
    /// `CODECAPI_AVEncH264CABACEnable`, for H.264.
    pub cabac: Option<bool>,
    /// The output type's `MF_MT_MPEG2_PROFILE`: H.264's 257 (Constrained
    /// High), 100 (High) or 66 (Baseline); H.265's 1 (Main).
    pub profile: u32,
    /// The `profile_idc` of the encoder's H.264 SPS, once it wrote one.
    pub sps_profile: Option<u8>,
    /// What that SPS said about output reordering, and what is sent.
    pub sps_reorder: Option<SpsReorder>,
    /// That SPS NAL unit as the encoder wrote it, before any rewrite: the
    /// one to compare with what is sent. Empty before it wrote one.
    pub sps: Vec<u8>,
}

/// Bytes as lowercase hex, for a log line.
struct Hex<'a>(&'a [u8]);
impl std::fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

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

/// What one [`Encoder::submit`] did with the caller's tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submit {
    /// A sample went to the encoder.
    Submitted,
    /// No room: the encoder holds `MAX_IN_FLIGHT`, or has no free sample.
    /// Backpressure, which spends the tick; a requested keyframe stays
    /// requested.
    Full,
    /// Room, but the encoder has not asked for input yet: some MFTs ask
    /// only once they have output the unit before. Nothing was captured,
    /// the caller's tick stays armed, and it asks again at its poll or as
    /// the encoder's request wakes it (`Encoder::idle_until`); a requested
    /// keyframe stays requested.
    Waiting,
    /// Room, and nothing worth sending: no new present, no keyframe due and
    /// a submit within `LIVENESS_INTERVAL`, or no image captured yet. The
    /// caller's tick stays armed, and it asks again.
    Unchanged,
}
impl Submit {
    /// Whether the caller's tick is spent: by a sample and by backpressure,
    /// while a tick that waits for the encoder's input request or for the
    /// next present stays armed.
    pub fn spends_tick(self) -> bool {
        match self {
            Self::Submitted | Self::Full => true,
            Self::Waiting | Self::Unchanged => false,
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
    /// The presents no submitted picture showed: for each dated new picture
    /// submitted, those it covers beyond one (`Presented::presents - 1`),
    /// whether DXGI accumulated them into one acquire or a newer acquire
    /// replaced a picture still waiting for the encoder. Thread C's
    /// `coalesced_per_s` counts the same.
    pub coalesced_presents: u64,
    /// This encoder's first acquire, once there was one.
    pub first_acquire: Option<FirstAcquire>,
    /// Submits that found room but no input request (`Submit::Waiting`).
    pub waiting: u64,
    /// Whether the encoder's events wake the caller (its event watcher
    /// runs); else they are polled at each drain.
    pub events_pushed: bool,
    /// From each output the event watcher saw announced
    /// (`METransformHaveOutput`) to the start of the drain's call that
    /// collected it (`ProcessOutput` and the sample it fills), so without
    /// the collection itself: how many fell in each bucket of
    /// `PICKUP_BOUNDS_US`, the last holding the rest. Nothing while the
    /// events are polled.
    pub pickup: [u64; PICKUP_BUCKETS],
    /// What the encoder read back as it opened; the SPS's fields from the
    /// first SPS it wrote, when its media type carried none.
    pub opened: EncoderOpened,
}

/// The bucket of `Statistics::pickup` a wait of `wait` falls in.
fn pickup_bucket(wait: Duration) -> usize {
    let us = wait.as_micros();
    PICKUP_BOUNDS_US
        .iter()
        .position(|&bound| us <= u128::from(bound))
        .unwrap_or(PICKUP_BOUNDS_US.len())
}

/// A wake for a caller that drives an [`Encoder`] itself, as `video_bench`
/// does: built with its `waker`, the encoder's event watcher wakes `wait`.
pub struct EncoderWake {
    waker: Waker,
    waiter: Waiter,
}
impl EncoderWake {
    pub fn new() -> Result<Self> {
        let wake = Wake::new();
        let waiter = Waiter::new(&wake)?;
        Ok(Self {
            waker: Waker(wake),
            waiter,
        })
    }
    pub fn waker(&self) -> &Waker {
        &self.waker
    }
    /// Wait until `deadline` or a wake; true when a wake, sent before or
    /// during the wait, ended it.
    pub fn wait(&self, deadline: Instant) -> Result<bool> {
        self.waiter.wait(deadline)
    }
}

/// What a duplication's first frame reported: whether a still desktop's
/// first frame carries a present, which only a device shows.
#[derive(Clone, Copy, Debug)]
pub struct FirstAcquire {
    /// `LastPresentTime`: 0 when only the pointer changed.
    pub last_present_time: i64,
    /// `AccumulatedFrames`: the desktop updates folded into it.
    pub accumulated_frames: u32,
    /// Whether a desktop image came with it.
    pub resource: bool,
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
    /// `video_bench --dump-source`'s: the last submitted sample, kept for
    /// `source_nv12`, and the texture it is read back through. Released
    /// before the sample allocator.
    source: Option<Source>,
    allocator: IMFVideoSampleAllocatorEx,
    _manager: IMFDXGIDeviceManager,
    latest: Option<ID3D11Texture2D>,
    latest_pending: bool,
    /// When the display presented `latest`'s picture, where DXGI says, and
    /// the presents it covers, summed while it waits (`latest_pending`).
    latest_at: Option<Instant>,
    latest_presents: u32,
    /// The last submit's new, dated picture.
    presented: Option<Presented>,
    /// The last sample time, in microseconds since `created`.
    last_pts_us: Option<u64>,
    pending: VecDeque<(u64, Instant, bool)>,
    /// When the last output was collected, discarded or not.
    last_output: Option<Instant>,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    created: Instant,
    last_submit: Option<Instant>,
    /// When the last submit found no input request (`Submit::Waiting`);
    /// `None` after any other outcome.
    waiting: Option<Instant>,
    last_present: i64,
    force_keyframe: bool,
    wait_independent_output: bool,
    stats: Statistics,
    /// The raised GPU priority (`EncoderOptions::gpu_priority`), given back
    /// as the encoder goes.
    _gpu_priority: Option<GpuPriority>,
    runtime: Runtime,
}

/// The last submitted sample, which `Encoder::source_nv12` reads back.
#[derive(Default)]
struct Source {
    sample: Option<IMFSample>,
    staging: Option<ID3D11Texture2D>,
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
    /// desktop texture remains at native resolution; no CPU readback. Given a
    /// `waker`, the encoder's events wake it where they can
    /// (`Statistics::events_pushed`, `drain_poll`); without one, `drain`
    /// polls them. A session takes the default `options`.
    #[expect(
        clippy::too_many_arguments,
        reason = "the public constructor video_bench and the backend call; each is a plain value"
    )]
    pub fn new_with_size(
        display: &Display,
        codec: VideoCodec,
        fps: u32,
        bitrate: u32,
        max_width: u32,
        max_height: u32,
        waker: Option<&Waker>,
        options: EncoderOptions,
    ) -> Result<Self> {
        Self::build(
            display,
            codec,
            fps,
            bitrate,
            OutputSize::Fit(max_width, max_height),
            waker,
            options,
        )
    }

    /// Encode exactly `width` × `height` (rounded down to even), upscaling a
    /// smaller display: `video_bench` measures a resolution the display does
    /// not have. A session never upscales.
    #[expect(
        clippy::too_many_arguments,
        reason = "the public constructor video_bench calls; each is a plain value"
    )]
    pub fn new_exact(
        display: &Display,
        codec: VideoCodec,
        fps: u32,
        bitrate: u32,
        width: u32,
        height: u32,
        waker: Option<&Waker>,
        options: EncoderOptions,
    ) -> Result<Self> {
        Self::build(
            display,
            codec,
            fps,
            bitrate,
            OutputSize::Exact(width, height),
            waker,
            options,
        )
    }

    fn build(
        display: &Display,
        kind: VideoCodec,
        fps: u32,
        bitrate: u32,
        size: OutputSize,
        waker: Option<&Waker>,
        options: EncoderOptions,
    ) -> Result<Self> {
        if kind == VideoCodec::Jpeg {
            return Err(error("the hardware encoder encodes H.264 or H.265"));
        }
        if !(1..=120).contains(&fps)
            || !(ENCODER_BITRATE_MIN..=ENCODER_BITRATE_MAX).contains(&bitrate)
        {
            return Err(error("video rate outside supported bounds"));
        }
        if options.quality_vs_speed > 100 {
            return Err(error("quality against speed outside 0..=100"));
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
            options,
        };
        // Before the encoder starts, so that its first unit has it too.
        let gpu_priority = options.gpu_priority.then(|| GpuPriority::hold(device));
        let codec = Codec::new(device, &manager, settings, waker)
            .map_err(|failure| codec_error(kind, failure))?;
        let opened = codec.opened.clone();
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_video_encoder_opened",
            encoder = %codec.name,
            codec = codec_name(kind),
            vendor = format_args!("{:#06x}", opened.vendor),
            width,
            height,
            fps,
            gop = codec.gop,
            rate_control = ?opened.rate_control,
            quality_vs_speed = ?opened.quality_vs_speed,
            scenario = ?opened.scenario,
            b_frames = ?opened.b_frames,
            cabac = ?opened.cabac,
            profile = opened.profile,
            sps_profile = ?opened.sps_profile,
            sps_reorder = %opened
                .sps_reorder
                .map_or_else(|| "unknown".to_owned(), |reorder| reorder.to_string()),
            sps = %Hex(&opened.sps),
            gpu_priority = options.gpu_priority,
        );
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
            events_pushed: codec.pushes(),
            opened,
            ..Statistics::default()
        };
        Ok(Self {
            capture,
            codec,
            converter,
            source: None,
            allocator,
            _manager: manager,
            latest: None,
            latest_pending: false,
            latest_at: None,
            latest_presents: 0,
            presented: None,
            last_pts_us: None,
            pending: VecDeque::with_capacity(MAX_IN_FLIGHT),
            last_output: None,
            width,
            height,
            fps,
            bitrate,
            created: now,
            last_submit: None,
            waiting: None,
            last_present: 0,
            force_keyframe: true,
            wait_independent_output: true,
            stats,
            _gpu_priority: gpu_priority,
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
    /// When the display presented the new picture the last `submit`
    /// submitted, and the presents it covers; `None` after a submit that
    /// sent a repeat, an undated picture or nothing.
    pub fn presented(&self) -> Option<Presented> {
        self.presented
    }
    /// How soon the caller drains again if nothing wakes it first:
    /// `DRAIN_POLL` while the events are polled, and while an output they
    /// announced is still uncollected (a drain collects at most one, and
    /// returns none after the encoder changed its output type or for an
    /// output it discards), which no further event will wake the caller
    /// for. `None` otherwise: each output wakes it.
    pub fn drain_poll(&self) -> Option<Duration> {
        (!self.codec.pushes() || self.codec.outputs() > 0).then_some(DRAIN_POLL)
    }
    /// While the last submit waited for the encoder to ask for input and
    /// the request wakes the caller: when to ask again. `DRAIN_POLL` after
    /// that submit until the encoder asks, and that submit's own time once
    /// it has, so that a request that came before the caller chose its
    /// next poll is answered at once all the same. `None` after any other
    /// outcome, and while the events are polled, whose caller polls its
    /// armed tick.
    pub fn idle_until(&self) -> Option<Instant> {
        let asked = self.waiting?;
        self.codec.pushes().then(|| {
            if self.codec.input_requested() {
                asked
            } else {
                asked + DRAIN_POLL
            }
        })
    }

    /// Keep each sample `submit` gives the encoder, for `source_nv12`
    /// (`video_bench --dump-source`): a reference to one sample of the pool.
    /// A session never does.
    pub fn keep_source(&mut self) {
        self.source.get_or_insert_with(Source::default);
    }

    /// Read the NV12 picture the last `submit` gave the encoder back from
    /// the GPU into `out`: `height` rows of `width` luma bytes, then
    /// `height / 2` rows of `width` interleaved chroma bytes. Once per
    /// submitted sample: false without `keep_source`, or when no sample was
    /// submitted since the last read. It waits for the GPU to finish the
    /// picture.
    pub fn source_nv12(&mut self, out: &mut Vec<u8>) -> Result<bool> {
        self.check_thread()?;
        let (width, height) = (self.width as usize, self.height as usize);
        let Some(source) = self.source.as_mut() else {
            return Ok(false);
        };
        let Some(sample) = source.sample.take() else {
            return Ok(false);
        };
        let buffer = unsafe { sample.GetBufferByIndex(0).map_err(error)? };
        let buffer: IMFDXGIBuffer = buffer.cast().map_err(error)?;
        let mut raw = std::ptr::null_mut();
        unsafe {
            buffer
                .GetResource(&ID3D11Texture2D::IID, &mut raw)
                .map_err(error)?;
        }
        // SAFETY: `GetResource` returned a referenced texture of the IID asked.
        let texture = unsafe { ID3D11Texture2D::from_raw(raw) };
        let subresource = unsafe { buffer.GetSubresourceIndex().map_err(error)? };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        // The pool's textures may be taller than the picture: the staging
        // copy takes a whole slice of one, and its chroma starts below all
        // of its rows.
        if source.staging.is_none() {
            let staging = D3D11_TEXTURE2D_DESC {
                MipLevels: 1,
                ArraySize: 1,
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..desc
            };
            let mut created = None;
            unsafe {
                self.capture
                    .device()
                    .CreateTexture2D(&staging, None, Some(&mut created))
                    .map_err(error)?;
            }
            source.staging = created;
        }
        let staging = source
            .staging
            .as_ref()
            .ok_or_else(|| error("missing staging texture"))?;
        let context = self.capture.device_context();
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            context.CopySubresourceRegion(staging, 0, 0, 0, 0, &texture, subresource, None);
            context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(error)?;
        }
        let pitch = mapped.RowPitch as usize;
        let rows = desc.Height as usize;
        // SAFETY: a mapped NV12 staging texture holds `rows` luma rows, then
        // `rows / 2` chroma rows, each `pitch` bytes apart, until `Unmap`.
        let data =
            unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * rows * 3 / 2) };
        out.clear();
        let (luma, chroma) = data.split_at(pitch * rows);
        let copied = luma
            .chunks(pitch)
            .take(height)
            .chain(chroma.chunks(pitch).take(height / 2))
            .try_for_each(|row| {
                out.extend_from_slice(row.get(..width)?);
                Some(())
            });
        unsafe { context.Unmap(staging, 0) };
        copied.ok_or_else(|| error("NV12 picture larger than its texture"))?;
        Ok(true)
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
            .map_err(|failure| self.codec_failed(failure))?;
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
        let ready = self
            .collect()
            .map_err(|failure| self.codec_failed(failure))?;
        if self.stats.submitted == 0 && self.created.elapsed() > FIRST_SUBMIT_WAIT {
            let (asked, image) = (self.codec.input_requested(), self.latest.is_some());
            let failure = unsubmitted(self.codec.kind, asked, image);
            if encoder_blamed(asked, image) {
                self.codec.failed(&failure);
            }
            return Err(failure);
        }
        if self
            .pending
            .front()
            .is_some_and(|(_, at, _)| at.elapsed() > OUTPUT_WAIT)
            || self.idle_without_request()
        {
            return Err(self.codec_failed(error("hardware video encoder stalled")));
        }
        Ok(ready)
    }

    /// `failure`, the hardware encoder's own, as `codec_error` makes it.
    /// One of an H.264 encoder built in a High profile, before its first
    /// output, turns High off for the process (`Codec::failed`).
    fn codec_failed(&self, failure: std::io::Error) -> std::io::Error {
        self.codec.failed(&failure);
        codec_error(self.codec.kind, failure)
    }

    /// An encoder that holds nothing asks for its next input at once. One
    /// that has not, `OUTPUT_WAIT` after it last took or gave a unit, never
    /// will: a `Submit::Waiting` that keeps the tick armed would ask it
    /// forever.
    fn idle_without_request(&self) -> bool {
        self.pending.is_empty()
            && !self.codec.input_requested()
            && self
                .last_submit
                .max(self.last_output)
                .is_some_and(|at| at.elapsed() > OUTPUT_WAIT)
    }

    fn collect(&mut self) -> Result<Option<Encoded>> {
        self.codec.events()?;
        let mut ready = None;
        if self.codec.outputs() > 0 {
            let sample = self.codec.output()?;
            if let Some(pickup) = self.codec.take_pickup()
                && let Some(bucket) = self.stats.pickup.get_mut(pickup_bucket(pickup))
            {
                *bucket += 1;
            }
            if let Some(sample) = sample {
                let (pts_us, at, duplicate) = self
                    .pending
                    .pop_front()
                    .ok_or_else(|| error("unsolicited video output"))?;
                if unsafe { sample.GetSampleTime().map_err(error)? } != (pts_us * 10) as i64 {
                    // Before the failure, so that the capture's retry
                    // builds H.264 in Baseline.
                    self.codec.reordered();
                    return Err(error(
                        "hardware encoder reordered input despite B-frames being off",
                    ));
                }
                let bytes = sample_bytes(&sample)?;
                let (bytes, keyframe) = self.codec.annex_b(bytes)?;
                if self.stats.opened.sps_reorder.is_none()
                    && let Some(reorder) = self.codec.sps_reorder()
                {
                    // An encoder whose media type carried no SPS writes
                    // its first in band: logged once, as the encoder's
                    // opening could not say it.
                    let opened = &mut self.stats.opened;
                    opened.sps_reorder = Some(reorder);
                    opened.sps_profile = self.codec.sps_profile();
                    opened.sps = self.codec.sps_written().to_vec();
                    tracing::info!(
                        target: "pb_mapper_desktop::session",
                        event = "desktop_video_encoder_sps",
                        encoder = %self.codec.name,
                        sps_profile = ?opened.sps_profile,
                        sps_reorder = %reorder,
                        sps = %Hex(&opened.sps),
                    );
                }
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
                // when that notification is already available; a watcher
                // wakes the caller for it otherwise.
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
    /// the encoder's output and, when they are polled, its input requests.
    /// Submits nothing while the encoder has no room (`Full`: `MAX_IN_FLIGHT`,
    /// no free sample), before it asks for input (`Waiting`: nothing is
    /// captured), or when nothing is worth sending (`Unchanged`: no new
    /// image, no keyframe due, and a submit within `LIVENESS_INTERVAL`; or no
    /// image yet). A requested keyframe stays requested until a sample
    /// carries it.
    pub fn submit(&mut self, force_keyframe: bool) -> Result<Submit> {
        self.check_thread()?;
        self.presented = None;
        self.waiting = None;
        self.force_keyframe |= force_keyframe;
        if self.pending.len() >= MAX_IN_FLIGHT {
            return Ok(Submit::Full);
        }
        if !self.codec.wants_input() {
            self.waiting = Some(Instant::now());
            self.stats.waiting += 1;
            return Ok(Submit::Waiting);
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
            return Ok(Submit::Unchanged);
        }
        let Some(texture) = &self.latest else {
            return Ok(Submit::Unchanged);
        };
        let sample = match unsafe { self.allocator.AllocateSample() } {
            Ok(sample) => sample,
            Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => return Ok(Submit::Full),
            Err(e) => return Err(error(e)),
        };
        self.converter.convert(texture, &sample)?;
        let now = Instant::now();
        // A new picture's sample time is when the display presented it, so
        // durations follow the display's cadence rather than the polls'; a
        // repeat's is now. Strictly increasing either way.
        let fresh = self.latest_pending;
        let presented = self.latest_at.filter(|_| fresh);
        let pts_us = sample_time(presented.unwrap_or(now), self.created, self.last_pts_us);
        // Variable frame rate: a sample lasts the real interval since the
        // previous one, while MF_MT_FRAME_RATE stays nominal. Pace changes
        // therefore never touch the media type or rebuild the encoder.
        let duration = self.last_pts_us.map_or(self.frame_period(), |last| {
            Duration::from_micros(pts_us - last).clamp(self.frame_period(), Duration::from_secs(1))
        });
        unsafe {
            sample.SetSampleTime((pts_us * 10) as i64).map_err(error)?;
            sample
                .SetSampleDuration((duration.as_nanos() / 100) as i64)
                .map_err(error)?;
        }
        if self.force_keyframe {
            self.codec
                .property(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32))
                .map_err(|failure| self.codec_failed(failure))?;
        }
        unsafe {
            self.codec
                .transform
                .ProcessInput(self.codec.input_id, &sample, 0)
                .map_err(|failure| self.codec_failed(error(failure)))?;
        }
        self.codec.input_taken();
        if let Some(source) = &mut self.source {
            source.sample = Some(sample);
        }
        self.force_keyframe = false;
        self.last_submit = Some(now);
        self.last_pts_us = Some(pts_us);
        self.pending.push_back((pts_us, now, !fresh));
        if fresh {
            self.presented = presented.map(|at| Presented::new(at, self.latest_presents));
            if let Some(presented) = self.presented {
                self.stats.coalesced_presents += u64::from(presented.presents.saturating_sub(1));
            }
            self.latest_at = None;
            self.latest_presents = 0;
        }
        self.latest_pending = false;
        self.stats.submitted += 1;
        self.stats.max_in_flight = self.stats.max_in_flight.max(self.pending.len());
        Ok(Submit::Submitted)
    }

    /// Copy a new desktop image, if there is one, into `latest`, and release
    /// the duplication frame at once. A frame held until the next acquire (as
    /// windows-capture's `acquire_next_frame` does) keeps DXGI from
    /// accumulating the next update meanwhile, so an acquire at the next tick
    /// races that update and misses about every other one. A new image is
    /// dated by its present, and counts the presents DXGI accumulated in it;
    /// the first one is copied whatever its present says, so a still
    /// desktop shows at once, and is left undated.
    fn acquire(&mut self) -> Result<bool> {
        let duplication = self.capture.duplication();
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        match unsafe { duplication.AcquireNextFrame(0, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(e) => return Err(error(e)),
        }
        let first = self.latest.is_none();
        if self.stats.first_acquire.is_none() {
            let acquired = FirstAcquire {
                last_present_time: info.LastPresentTime,
                accumulated_frames: info.AccumulatedFrames,
                resource: resource.is_some(),
            };
            self.stats.first_acquire = Some(acquired);
            tracing::debug!(
                target: "pb_mapper_desktop::session",
                event = "windows_capture_first_acquire",
                last_present_time = acquired.last_present_time,
                accumulated_frames = acquired.accumulated_frames,
                resource = acquired.resource,
            );
        }
        let copied = (|| {
            let present = info.LastPresentTime;
            // Zero: only the pointer moved.
            if !(first && resource.is_some()) && (present == 0 || present == self.last_present) {
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
            if present != 0 {
                self.last_present = present;
            }
            self.stats.unique_captures += 1;
            // A picture left waiting keeps its presents, and takes the newer
            // one's time.
            self.latest_at = if first {
                None
            } else {
                present_instant(present)
            };
            self.latest_presents = self
                .latest_presents
                .saturating_add(info.AccumulatedFrames.max(1));
            Ok(true)
        })();
        let released = unsafe { duplication.ReleaseFrame() }.map_err(error);
        copied.and_then(|copied| released.map(|()| copied))
    }

    fn frame_period(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 / u64::from(self.fps))
    }
}

/// A sample time in microseconds since `created` for a picture of `at`,
/// later than the `last` one.
fn sample_time(at: Instant, created: Instant, last: Option<u64>) -> u64 {
    let pts_us =
        u64::try_from(at.saturating_duration_since(created).as_micros()).unwrap_or(u64::MAX);
    last.map_or(pts_us, |last| pts_us.max(last.saturating_add(1)))
}

/// When the display presented at performance counter reading `ticks`
/// (`LastPresentTime`), as an `Instant`, by its age against the counter
/// now. std's `Instant` reads the same counter on Windows, so this is exact
/// but for the moment between the two reads.
fn present_instant(ticks: i64) -> Option<Instant> {
    static FREQUENCY: OnceLock<Option<i64>> = OnceLock::new();
    let frequency = (*FREQUENCY.get_or_init(|| {
        let mut frequency: i64 = 0;
        // SAFETY: an out parameter the call fills.
        unsafe { QueryPerformanceFrequency(&mut frequency) }.ok()?;
        (frequency > 0).then_some(frequency)
    }))?;
    let mut now: i64 = 0;
    // SAFETY: as above.
    unsafe { QueryPerformanceCounter(&mut now) }.ok()?;
    counter_instant(ticks, now, frequency, Instant::now())
}

/// The instant of counter reading `ticks`, given that the counter, at
/// `frequency` a second, read `now` at `at`: a reading a moment ahead of
/// `now` (the order of the reads) is `at`, and one over `PRESENT_MAX_AGE`
/// old is none.
fn counter_instant(ticks: i64, now: i64, frequency: i64, at: Instant) -> Option<Instant> {
    let age = (i128::from(now) - i128::from(ticks)).max(0);
    let nanos = age * 1_000_000_000 / i128::from(frequency.max(1));
    let age = Duration::from_nanos(u64::try_from(nanos).ok()?);
    if age > PRESENT_MAX_AGE {
        return None;
    }
    at.checked_sub(age)
}

/// Why an encoder took no input in its first `FIRST_SUBMIT_WAIT`. It asked
/// for input and capture had no desktop image to give it: capture is stuck,
/// and is retried. It never asked, or had an image and no free sample for it:
/// the encoder is stuck, a codec error. The probe encodes no frame, so this is
/// where an H.265 encoder that never asks for input is turned off.
fn unsubmitted(kind: VideoCodec, asked: bool, image: bool) -> std::io::Error {
    if encoder_blamed(asked, image) {
        codec_error(kind, error("hardware video encoder stalled"))
    } else {
        error("desktop capture delivered no image")
    }
}

/// Whether an encoder that took no input in its first `FIRST_SUBMIT_WAIT`
/// is itself stuck: it never `asked` for input, or had an `image` to take.
fn encoder_blamed(asked: bool, image: bool) -> bool {
    !asked || image
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

    /// A present's counter reading becomes an `Instant` by its age: now is
    /// now, an older reading earlier, one ahead now, one over a second old
    /// none; and the sample times built on them only rise.
    #[test]
    fn presents_are_dated_by_their_age_on_the_performance_counter() {
        let mut frequency: i64 = 0;
        // SAFETY: out parameters the calls fill.
        unsafe { QueryPerformanceFrequency(&mut frequency) }.unwrap();
        // Bracketed by two clock reads, so a preemption anywhere between
        // them widens the bracket instead of failing the test; the slack
        // covers only rounding.
        let before = Instant::now();
        let mut now: i64 = 0;
        // SAFETY: as above.
        unsafe { QueryPerformanceCounter(&mut now) }.unwrap();
        let read = present_instant(now).unwrap();
        let earlier = present_instant(now - frequency / 10).unwrap();
        let ahead = present_instant(now + frequency).unwrap();
        let after = Instant::now();
        let slack = Duration::from_millis(1);
        let within = |at: Instant| at + slack >= before && at <= after + slack;
        assert!(within(read), "{read:?} not in {before:?}..{after:?}");
        assert!(within(earlier + Duration::from_millis(100)));
        assert!(within(ahead));
        assert_eq!(present_instant(now - frequency * 2), None);

        let at = Instant::now();
        assert_eq!(counter_instant(1_000, 1_000, 10_000_000, at), Some(at));
        assert_eq!(
            counter_instant(1_000, 11_000, 10_000_000, at),
            Some(at - Duration::from_millis(1))
        );
        assert_eq!(counter_instant(2_000, 1_000, 10_000_000, at), Some(at));
        assert_eq!(counter_instant(0, 10_000_001, 10_000_000, at), None);

        let created = at - Duration::from_secs(1);
        assert_eq!(sample_time(at, created, None), 1_000_000);
        assert_eq!(sample_time(at, created, Some(999_000)), 1_000_000);
        assert_eq!(sample_time(at, created, Some(1_000_000)), 1_000_001);
        assert_eq!(
            sample_time(created - Duration::from_secs(1), created, Some(5)),
            6
        );
    }

    #[test]
    fn only_a_sample_or_a_full_encoder_spends_the_tick() {
        assert!(Submit::Submitted.spends_tick());
        // A full encoder is backpressure: thread C skips to the next tick.
        assert!(Submit::Full.spends_tick());
        // An encoder that has not asked for input yet frees up within its
        // encode: thread C asks again as it asks, rather than a tick later.
        assert!(!Submit::Waiting.spends_tick());
        // Nothing new: thread C asks again, so the next present goes out as
        // it appears rather than a tick later.
        assert!(!Submit::Unchanged.spends_tick());
    }

    #[test]
    fn a_pickup_falls_in_the_first_bucket_that_holds_it() {
        let us = Duration::from_micros;
        assert_eq!(pickup_bucket(Duration::ZERO), 0);
        assert_eq!(pickup_bucket(us(100)), 0);
        assert_eq!(pickup_bucket(us(101)), 1);
        assert_eq!(pickup_bucket(us(2_000)), 4);
        assert_eq!(pickup_bucket(us(8_000)), 6);
        assert_eq!(pickup_bucket(us(8_001)), PICKUP_BUCKETS - 1);
        assert_eq!(pickup_bucket(Duration::from_secs(5)), PICKUP_BUCKETS - 1);
    }
}
