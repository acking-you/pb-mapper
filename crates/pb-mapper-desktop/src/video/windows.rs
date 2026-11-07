//! Desktop Duplication -> D3D11 video processor -> Media Foundation H.264.
//!
//! Raw desktop pixels stay on the GPU. Only complete encoded access units cross
//! into host memory. The Windows codec, rather than application code, implements
//! H.264. Create, use and drop this object on the capture worker's desktop thread.

use crate::{Display, Result, error};
use std::{
    collections::VecDeque,
    mem::ManuallyDrop,
    thread::ThreadId,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct3D11::*,
            Dxgi::{Common::*, IDXGIDevice},
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

const MAX_IN_FLIGHT: usize = 3;
const MAX_ACCESS_UNIT: usize = 4 * 1024 * 1024;

pub struct Encoded {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    /// False only for a new desktop presentation; static liveness repeats do
    /// not count towards the advertised capture/display performance target.
    pub duplicate: bool,
    /// Monotonic capture timestamp relative to this encoder's creation.
    pub pts_us: u64,
}

impl From<Encoded> for crate::EncodedFrame {
    fn from(image: Encoded) -> Self {
        Self {
            format: crate::VideoFormat {
                codec: crate::VideoCodec::H264,
                width: image.width,
                height: image.height,
                keyframe: image.keyframe,
                pts_us: image.pts_us,
                duplicate: image.duplicate,
                encode_us: 0,
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
}

struct Runtime {
    thread: ThreadId,
}
impl Runtime {
    fn new() -> Result<Self> {
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

/// Bounded hardware encoder. `None` means no access unit is ready yet; callers
/// must continue servicing input/stop while waiting, rather than busy-spinning.
pub struct Encoder {
    capture: DxgiDuplicationApi,
    codec: Codec,
    converter: Converter,
    allocator: IMFVideoSampleAllocatorEx,
    _manager: IMFDXGIDeviceManager,
    latest: Option<ID3D11Texture2D>,
    latest_pending: bool,
    pending: VecDeque<(u64, Instant, bool)>,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    created: Instant,
    next_capture: Instant,
    last_sent: Instant,
    last_present: i64,
    force_keyframe: bool,
    wait_independent_output: bool,
    stats: Statistics,
    _runtime: Runtime,
}

impl Encoder {
    pub fn new(display: &Display, fps: u32, bitrate: u32) -> Result<Self> {
        Self::new_with_size(display, fps, bitrate, display.width, display.height)
    }

    /// Scale on the D3D11 video processor before hardware encoding. The latest
    /// captured desktop texture remains at native resolution; no CPU readback.
    pub fn new_with_size(
        display: &Display,
        fps: u32,
        bitrate: u32,
        max_width: u32,
        max_height: u32,
    ) -> Result<Self> {
        if !(1..=120).contains(&fps) || !(256_000..=80_000_000).contains(&bitrate) {
            return Err(error("video rate outside supported bounds"));
        }
        if !(2..=7680).contains(&max_width) || !(2..=4320).contains(&max_height) {
            return Err(error("video size outside supported bounds"));
        }
        let runtime = Runtime::new()?;
        let monitor = Monitor::enumerate()
            .map_err(error)?
            .into_iter()
            .find(|m| m.as_raw_hmonitor() as usize as u32 == display.id)
            .ok_or_else(|| error("selected GPU output disappeared"))?;
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
        let (width, height) = scaled_size(input_width, input_height, max_width, max_height);
        let device = capture.device();
        let protection: ID3D11Multithread = capture.device_context().cast().map_err(error)?;
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
        let codec = Codec::new(device, &manager, width, height, fps, bitrate)?;
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
        Ok(Self {
            capture,
            codec,
            converter,
            allocator,
            _manager: manager,
            latest: None,
            latest_pending: false,
            pending: VecDeque::with_capacity(MAX_IN_FLIGHT),
            width,
            height,
            fps,
            bitrate,
            created: now,
            next_capture: now,
            last_sent: now,
            last_present: 0,
            force_keyframe: true,
            wait_independent_output: true,
            stats: Statistics::default(),
            _runtime: runtime,
        })
    }

    pub fn name(&self) -> &str {
        &self.codec.name
    }
    pub fn statistics(&self) -> Statistics {
        self.stats.clone()
    }

    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.check_thread()?;
        if !(256_000..=80_000_000).contains(&bitrate) {
            return Err(error("video bitrate outside supported bounds"));
        }
        if self.bitrate == bitrate {
            return Ok(());
        }
        self.codec
            .property(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate))?;
        self.bitrate = bitrate;
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
    pub fn drain(&mut self) -> Result<Option<Encoded>> {
        self.check_thread()?;
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
                        "hardware encoder reordered input despite baseline profile",
                    ));
                }
                let bytes = sample_bytes(&sample)?;
                let (bytes, keyframe) = self.codec.annex_b(bytes)?;
                // A genuinely late codec output is not a newly captured picture.
                if at.elapsed() > Duration::from_millis(250) {
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
                    self.stats.encode_time_us += at.elapsed().as_micros() as u64;
                    ready = Some(Encoded {
                        bytes,
                        width: self.width,
                        height: self.height,
                        keyframe,
                        duplicate,
                        pts_us,
                    });
                }
                // Some hardware MFTs issue NeedInput only when ProcessOutput
                // releases their previous output. Refill in this same call
                // when that notification is already available.
                self.codec.events()?;
            }
        }
        if self
            .pending
            .front()
            .is_some_and(|(_, at, _)| at.elapsed() > Duration::from_secs(2))
            || (self.stats.submitted == 0 && self.created.elapsed() > Duration::from_secs(3))
        {
            return Err(error("hardware video encoder stalled"));
        }
        Ok(ready)
    }

    pub fn next_frame(&mut self, force_keyframe: bool) -> Result<Option<Encoded>> {
        self.force_keyframe |= force_keyframe;
        let ready = self.drain()?;
        if self.pending.len() >= MAX_IN_FLIGHT
            || self.codec.need_input == 0
            || Instant::now() < self.next_capture
        {
            return Ok(ready);
        }
        let changed = match self.capture.acquire_next_frame(0) {
            Ok(frame) => {
                let present = frame.frame_info().LastPresentTime;
                if present != 0 && present != self.last_present {
                    self.last_present = present;
                    self.stats.unique_captures += 1;
                    if self.latest.is_none() {
                        let mut desc = *frame.texture_desc();
                        desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
                        desc.MiscFlags = 0;
                        let mut texture = None;
                        unsafe {
                            frame
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
                    unsafe {
                        frame.device_context().CopyResource(latest, frame.texture());
                    }
                    true
                } else {
                    false
                }
            }
            Err(windows_capture::dxgi_duplication_api::Error::Timeout) => false,
            Err(e) => return Err(error(e)),
        };
        self.latest_pending |= changed;
        // Static desktops still send a fresh, verified liveness frame. These
        // repetitions are excluded from unique_captures and benchmark FPS.
        if !self.latest_pending
            && !self.force_keyframe
            && self.last_sent.elapsed() < Duration::from_millis(250)
        {
            return Ok(ready);
        }
        let Some(texture) = &self.latest else {
            return Ok(ready);
        };
        let sample = match unsafe { self.allocator.AllocateSample() } {
            Ok(sample) => sample,
            Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => return Ok(ready),
            Err(e) => return Err(error(e)),
        };
        self.converter.convert(texture, &sample)?;
        let pts_us = self.created.elapsed().as_micros() as u64;
        unsafe {
            sample.SetSampleTime((pts_us * 10) as i64).map_err(error)?;
            sample
                .SetSampleDuration((10_000_000 / self.fps) as i64)
                .map_err(error)?;
        }
        if self.force_keyframe {
            self.codec
                .property(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32))?;
        }
        unsafe {
            self.codec
                .transform
                .ProcessInput(self.codec.input_id, &sample, 0)
                .map_err(error)?;
        }
        self.codec.need_input -= 1;
        self.force_keyframe = false;
        self.last_sent = Instant::now();
        // Preserve the target clock across display refreshes. Starting a new
        // interval after every captured refresh rounds 120 FPS on a 165 Hz
        // display down to 82.5 FPS. Never catch up with a burst after a stall.
        self.next_capture = (self.next_capture
            + Duration::from_nanos(1_000_000_000 / u64::from(self.fps)))
        .max(self.last_sent);
        self.pending
            .push_back((pts_us, self.last_sent, !self.latest_pending));
        self.latest_pending = false;
        self.stats.submitted += 1;
        self.stats.max_in_flight = self.stats.max_in_flight.max(self.pending.len());
        Ok(ready)
    }
}

fn scaled_size(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    // Integer arithmetic avoids rounding beyond the renderer's bounds. NV12
    // needs even dimensions; at most one source edge pixel is lost to rounding.
    let target_width = width.min(max_width);
    let target_height = height.min(max_height);
    let (width, height) = if u64::from(target_width) * u64::from(height)
        <= u64::from(target_height) * u64::from(width)
    {
        (
            target_width,
            (u64::from(height) * u64::from(target_width) / u64::from(width)) as u32,
        )
    } else {
        (
            (u64::from(width) * u64::from(target_height) / u64::from(height)) as u32,
            target_height,
        )
    };
    ((width & !1).max(2), (height & !1).max(2))
}

struct Codec {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    api: ICodecAPI,
    name: String,
    input_id: u32,
    output_id: u32,
    need_input: u32,
    have_output: u32,
    parameter_sets: ParameterSets,
}
impl Codec {
    fn new(
        device: &ID3D11Device,
        manager: &IMFDXGIDeviceManager,
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
            guidSubtype: MFVideoFormat_H264,
        };
        let attrs = attributes()?;
        let dxgi: IDXGIDevice = device.cast().map_err(error)?;
        let adapter = unsafe { dxgi.GetAdapter().map_err(error)?.GetDesc().map_err(error)? };
        let luid = (u64::from(adapter.AdapterLuid.HighPart as u32) << 32)
            | u64::from(adapter.AdapterLuid.LowPart);
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
            "No compatible hardware H.264 encoder on the capture GPU{}",
            if failures.is_empty() {
                String::new()
            } else {
                format!(": {}", failures.join("; "))
            }
        )))
    }

    fn activate(
        activation: &IMFActivate,
        name: String,
        manager: &IMFDXGIDeviceManager,
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
            transform,
            events,
            api,
            name,
            input_id: 0,
            output_id: 0,
            need_input: 0,
            have_output: 0,
            parameter_sets: ParameterSets::default(),
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
        // Baseline profile forbids B slices even on drivers that reject this
        // optional property. Do not require a vendor-specific writable switch.
        let _ = codec.property(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32));
        let _ = codec.property(
            &CODECAPI_AVEncCommonRateControlMode,
            &VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
        );
        let _ = codec.property(&CODECAPI_AVEncCommonQualityVsSpeed, &VARIANT::from(0u32));
        let _ = codec.property(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(fps));
        let output = media_type(&MFVideoFormat_H264, width, height, fps)?;
        let input = media_type(&MFVideoFormat_NV12, width, height, fps)?;
        unsafe {
            output
                .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
                .map_err(error)?;
            output
                .SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)
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
                return Err(error("H.264 sequence header exceeds limit"));
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
    fn annex_b(&mut self, bytes: Vec<u8>) -> Result<(Vec<u8>, bool)> {
        // MF's elementary H.264 type produces Annex B. Refuse an unexpected
        // format instead of shipping undecodable length-prefixed data to phones.
        let units = nal_ranges(&bytes)?;
        let keyframe = units.iter().any(|&(start, _)| bytes[start] & 0x1f == 5);
        self.parameter_sets.observe(&bytes, &units)?;
        if keyframe
            && (!units.iter().any(|&(start, _)| bytes[start] & 0x1f == 7)
                || !units.iter().any(|&(start, _)| bytes[start] & 0x1f == 8))
        {
            if !self.parameter_sets.complete() {
                self.update_header()?;
            }
            if !self.parameter_sets.complete() {
                return Err(error("H.264 keyframe has no decoder configuration"));
            }
            let mut result = Vec::with_capacity(
                self.parameter_sets.sps.len() + self.parameter_sets.pps.len() + bytes.len() + 8,
            );
            result.extend_from_slice(&[0, 0, 0, 1]);
            result.extend_from_slice(&self.parameter_sets.sps);
            result.extend_from_slice(&[0, 0, 0, 1]);
            result.extend_from_slice(&self.parameter_sets.pps);
            result.extend_from_slice(&bytes);
            if result.len() > MAX_ACCESS_UNIT {
                return Err(error("H.264 access unit exceeds video limit"));
            }
            return Ok((result, true));
        }
        Ok((bytes, keyframe))
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
fn activation_name(activation: &IMFActivate) -> String {
    let mut raw = PWSTR::null();
    let mut len = 0;
    if unsafe { activation.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut raw, &mut len) }
        .is_err()
    {
        return "hardware H.264".into();
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
        Err(error("invalid H.264 access unit size"))
    } else {
        Ok(unsafe { std::slice::from_raw_parts(raw, len as usize) }.to_vec())
    };
    unsafe {
        buffer.Unlock().map_err(error)?;
    }
    result
}
fn nal_ranges(bytes: &[u8]) -> Result<Vec<(usize, usize)>> {
    if !bytes.starts_with(&[0, 0, 0, 1]) && !bytes.starts_with(&[0, 0, 1]) {
        return Err(error("hardware encoder did not produce Annex B H.264"));
    }
    let mut units = Vec::new();
    let mut position = 0;
    while position + 3 < bytes.len() {
        let prefix = if bytes[position..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[position..].starts_with(&[0, 0, 1]) {
            3
        } else {
            position += 1;
            continue;
        };
        if let Some((_, end)) = units.last_mut() {
            *end = position;
        }
        let start = position + prefix;
        if start >= bytes.len() {
            return Err(error("truncated H.264 NAL"));
        }
        units.push((start, bytes.len()));
        if units.len() > 4096 {
            return Err(error("too many H.264 NAL units"));
        }
        position = start;
    }
    if units.is_empty() {
        return Err(error("hardware encoder did not produce Annex B H.264"));
    }
    Ok(units)
}

#[derive(Default)]
struct ParameterSets {
    sps: Vec<u8>,
    pps: Vec<u8>,
}
impl ParameterSets {
    fn complete(&self) -> bool {
        !self.sps.is_empty() && !self.pps.is_empty()
    }
    fn observe(&mut self, bytes: &[u8], units: &[(usize, usize)]) -> Result<()> {
        for &(start, end) in units {
            let target = match bytes[start] & 0x1f {
                7 => &mut self.sps,
                8 => &mut self.pps,
                _ => continue,
            };
            if end - start > 64 * 1024 {
                return Err(error("H.264 parameter set exceeds limit"));
            }
            target.clear();
            target.extend_from_slice(&bytes[start..end]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gpu_output_dimensions_fit_renderer_without_upscaling() {
        assert_eq!(scaled_size(2560, 1440, 1920, 1080), (1920, 1080));
        assert_eq!(scaled_size(1280, 720, 1920, 1080), (1280, 720));
        assert_eq!(scaled_size(3440, 1440, 1920, 1080), (1920, 802));
        assert_eq!(scaled_size(1080, 1920, 1920, 1080), (606, 1080));
    }
    #[test]
    fn parameter_sets_survive_partial_updates_and_emulation_prevention() -> Result<()> {
        let mut sets = ParameterSets::default();
        let initial = [0, 0, 0, 1, 0x67, 1, 0, 0, 3, 1, 0, 0, 1, 0x68, 2];
        sets.observe(&initial, &nal_ranges(&initial)?)?;
        assert!(sets.complete());
        assert_eq!(sets.sps, &[0x67, 1, 0, 0, 3, 1]);
        let partial = [0, 0, 1, 0x68, 3];
        sets.observe(&partial, &nal_ranges(&partial)?)?;
        assert!(sets.complete());
        assert_eq!(sets.pps, &[0x68, 3]);
        assert!(nal_ranges(&[0, 0, 0, 1]).is_err());
        assert!(nal_ranges(&[0, 0, 0, 8, 0x65]).is_err());
        Ok(())
    }
}
