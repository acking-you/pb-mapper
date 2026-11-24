//! The GPU's hardware H.264 or H.265 Media Foundation encoder, as the
//! pipeline drives it, and the media types and samples it takes.
//!
//! The encoder is asynchronous: it asks for each input
//! (`METransformNeedInput`) and announces each output
//! (`METransformHaveOutput`) as an event. An encoder that can be shut down
//! (`IMFShutdown`, which every asynchronous MFT implements) gets a watcher
//! thread that waits for its events, counts them and wakes thread C, and
//! whose wait the encoder's shutdown ends; for any other, thread C reads
//! them each time it drains.
use super::MAX_IN_FLIGHT;
use crate::{
    Result, VideoCodec, Waker,
    codec::annexb::{AccessUnit, MAX_ACCESS_UNIT, ParameterSets, codec_name},
    error,
};
use std::{
    mem::ManuallyDrop,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::LUID,
        Graphics::{Direct3D11::ID3D11Device, Dxgi::IDXGIDevice},
        Media::MediaFoundation::*,
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            Variant::VARIANT,
        },
    },
    core::{GUID, Interface, PWSTR},
};

/// GOP sizes to request, longest first: keyframes come only on demand. Never
/// 0, which asks the encoder for its own default GOP rather than an endless one.
const GOP_CANDIDATES: [u32; 4] = [u32::MAX, 0x7FFF_FFFF, 65_535, 3_600];
/// The most input requests an encoder may have outstanding.
const MAX_INPUT_REQUESTS: u32 = 64;
/// The announcement times of this many outputs are kept, by sequence number,
/// for the wait until thread C collects each: more than the outputs an
/// encoder may announce before one is collected.
const OUTPUT_TIMES: usize = 8;
const _: () = assert!(
    OUTPUT_TIMES > MAX_IN_FLIGHT,
    "an announced output keeps its time until it is collected"
);
/// How long dropping an encoder waits for its event watcher to end. Shutting
/// the encoder down ends the watcher's wait at once; a watcher that goes on
/// waiting is left behind rather than holding thread C.
const WATCHER_EXIT: Duration = Duration::from_millis(100);
/// An encoder's shutdown once left its watcher waiting: that thread, and
/// the encoder its wait holds, stay behind. Later encoders, which a desktop
/// switch, a recovery or a new size builds again, get no watcher and have
/// their events polled, so that at most one is left behind per process.
static WATCHER_STUCK: AtomicBool = AtomicBool::new(false);

pub(super) fn luid_value(luid: LUID) -> u64 {
    (u64::from(luid.HighPart as u32) << 32) | u64::from(luid.LowPart)
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

/// What the encoder is set up for: a codec, the encoded size, the nominal
/// frame rate and the mean bitrate.
#[derive(Clone, Copy)]
pub(super) struct EncoderSettings {
    pub(super) kind: VideoCodec,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) fps: u32,
    pub(super) bitrate: u32,
}

pub(super) struct Codec {
    pub(super) kind: VideoCodec,
    pub(super) transform: IMFTransform,
    events: IMFMediaEventGenerator,
    /// The events counted, by the watcher or by `events`.
    counts: Arc<Events>,
    /// Waits for the events and wakes thread C; `None`: thread C polls them.
    watcher: Option<Watcher>,
    /// Outputs announced while polled, and outputs collected: the sequence
    /// numbers of their announcement times.
    announced: u64,
    collected: u64,
    /// From the last output's announcement, as the watcher saw it, to the
    /// call that collected it, as it began: the wait for thread C, without
    /// the collection itself.
    pickup: Option<Duration>,
    api: ICodecAPI,
    pub(super) name: String,
    pub(super) input_id: u32,
    output_id: u32,
    parameter_sets: ParameterSets,
    /// The NAL unit ranges of the unit being sent, kept across units.
    nal_units: Vec<(usize, usize)>,
    pub(super) gop: u32,
}
impl Codec {
    /// The first hardware encoder of `settings.kind` on `device`'s adapter
    /// that takes this pipeline's settings. There is no software fallback.
    /// Given a `waker`, an encoder that can be shut down wakes it with its
    /// events (`Codec::pushes`).
    pub(super) fn new(
        device: &ID3D11Device,
        manager: &IMFDXGIDeviceManager,
        settings: EncoderSettings,
        waker: Option<&Waker>,
    ) -> Result<Self> {
        let kind = settings.kind;
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
            match Self::activate(&activation, name.clone(), manager, settings, waker) {
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

    fn activate(
        activation: &IMFActivate,
        name: String,
        manager: &IMFDXGIDeviceManager,
        settings: EncoderSettings,
        waker: Option<&Waker>,
    ) -> Result<Self> {
        let EncoderSettings {
            kind,
            width,
            height,
            fps,
            bitrate,
        } = settings;
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
            counts: Arc::new(Events::new()),
            watcher: None,
            announced: 0,
            collected: 0,
            pickup: None,
            api,
            name,
            input_id: 0,
            output_id: 0,
            parameter_sets: ParameterSets::new(kind),
            nal_units: Vec::new(),
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
            VideoCodec::H264 | VideoCodec::Jpeg => eAVEncH264VProfile_Base.0,
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
        // Its first input requests wait in its queue meanwhile. Only an
        // encoder that something waits on says how its events come: the
        // H.265 probe's, which nothing waits on, would read as polled.
        if let Some(waker) = waker {
            codec.watch(waker);
            tracing::debug!(
                target: "pb_mapper_desktop::session",
                event = "windows_encoder_events",
                encoder = %codec.name,
                pushed = codec.pushes(),
            );
        }
        Ok(codec)
    }
    /// Wait for the encoder's events on a thread of their own, which wakes
    /// `waker` for each output and for an input request thread C waits for.
    /// Only an encoder that can be shut down gets one, since its shutdown
    /// is what ends the watcher's wait; without one, when the thread cannot
    /// start, or once an encoder's shutdown left its watcher waiting
    /// (`WATCHER_STUCK`), thread C polls the events.
    fn watch(&mut self, waker: &Waker) {
        if WATCHER_STUCK.load(Ordering::Acquire) || self.transform.cast::<IMFShutdown>().is_err() {
            return;
        }
        let source = Generator(self.events.clone());
        match Watcher::spawn(source, self.counts.clone(), waker.clone()) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(failure) => tracing::warn!(
                target: "pb_mapper_desktop::session",
                event = "windows_encoder_watcher_failed",
                detail = %failure,
            ),
        }
    }
    /// Whether the encoder's events wake thread C, rather than being polled.
    pub(super) fn pushes(&self) -> bool {
        self.watcher.is_some()
    }
    pub(super) fn property(&self, key: &GUID, value: &VARIANT) -> Result<()> {
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
    /// Count the events that came, when thread C polls them, and return the
    /// first failure, whoever counted it. A watcher is the only reader of
    /// the encoder's events: a second one would get
    /// `MF_E_MULTIPLE_SUBSCRIBERS`.
    pub(super) fn events(&mut self) -> Result<()> {
        if self.watcher.is_none() {
            for _ in 0..32 {
                // SAFETY: a COM call on an interface this codec holds; it
                // returns at once.
                let event = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                    Ok(event) => event,
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                    Err(e) => {
                        self.counts.fail(Fault::Failed(e));
                        break;
                    }
                };
                if let Err(fault) = self.counts.observe(&event, &mut self.announced) {
                    self.counts.fail(fault);
                    break;
                }
            }
        }
        self.counts.check()
    }
    /// Whether the encoder asked for input. When it has not, its watcher
    /// wakes thread C at its next request.
    pub(super) fn wants_input(&self) -> bool {
        let counts = &self.counts;
        if self.watcher.is_some() {
            // Set before the read, as the watcher counts a request before
            // it reads this: of the two, one sees the other.
            counts.awaiting_input.store(true, Ordering::SeqCst);
        }
        let wanted = counts.need_input.load(Ordering::SeqCst) > 0;
        if wanted {
            counts.awaiting_input.store(false, Ordering::SeqCst);
        }
        wanted
    }
    /// Whether the encoder has an input request outstanding.
    pub(super) fn input_requested(&self) -> bool {
        self.counts.need_input.load(Ordering::SeqCst) > 0
    }
    /// `ProcessInput` answered an input request.
    pub(super) fn input_taken(&self) {
        take(&self.counts.need_input);
    }
    /// The outputs announced and not yet collected.
    pub(super) fn outputs(&self) -> u32 {
        // Acquire: an output's announcement time is stored before it counts.
        self.counts.have_output.load(Ordering::Acquire)
    }
    /// The wait of the last output collected, from its announcement to the
    /// start of the call that collected it, when the watcher saw it.
    pub(super) fn take_pickup(&mut self) -> Option<Duration> {
        self.pickup.take()
    }
    pub(super) fn output(&mut self) -> Result<Option<IMFSample>> {
        // Before the sample and `ProcessOutput`: the wait thread C took to
        // come for the output, not the time it takes to collect it.
        let called = self.watcher.is_some().then(|| self.counts.epoch.elapsed());
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
                take(&self.counts.have_output);
                let slot = (self.collected % OUTPUT_TIMES as u64) as usize;
                self.collected += 1;
                let counts = &self.counts;
                self.pickup = called.and_then(|called| {
                    let announced = counts.announced.get(slot)?.load(Ordering::Relaxed);
                    Some(called.saturating_sub(Duration::from_micros(announced)))
                });
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
        update_header(&self.transform, self.output_id, &mut self.parameter_sets)
    }
    /// The access unit as sent, and whether it is a keyframe. MF's elementary
    /// H.264 and H.265 types produce Annex B; refuse an unexpected format
    /// instead of shipping undecodable length-prefixed data to phones. A
    /// keyframe that comes without its parameter sets gets the last ones,
    /// in-band or from the media type, in front.
    pub(super) fn annex_b(&mut self, bytes: Vec<u8>) -> Result<(Vec<u8>, bool)> {
        let unit = AccessUnit::parse(&bytes, &mut self.nal_units)?;
        self.parameter_sets.observe(&unit)?;
        let keyframe = unit.keyframe(self.kind);
        if !keyframe || self.parameter_sets.carried(&unit) {
            return Ok((bytes, keyframe));
        }
        if !self.parameter_sets.complete() {
            update_header(&self.transform, self.output_id, &mut self.parameter_sets)?;
        }
        Ok((self.parameter_sets.prepend(&unit)?, true))
    }
}
/// Keep the parameter sets the output type's sequence header carries.
fn update_header(
    transform: &IMFTransform,
    output_id: u32,
    parameter_sets: &mut ParameterSets,
) -> Result<()> {
    let media_type = unsafe { transform.GetOutputCurrentType(output_id).map_err(error)? };
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
            parameter_sets.observe(&AccessUnit::parse(&bytes, &mut Vec::new())?)?;
        }
    }
    Ok(())
}
impl Drop for Codec {
    /// Flush and shut the encoder down. Its shutdown ends the watcher's
    /// wait, and the watcher ends before the encoder's interfaces go, and
    /// with them, after the codec, the COM runtime.
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        self.counts.stopping.store(true, Ordering::Release);
        if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
            unsafe {
                let _ = shutdown.Shutdown();
            }
        }
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

/// Take one from a count only thread C takes from, so that a count it read
/// above zero stays so until it does.
fn take(count: &AtomicU32) {
    let _ = count.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
}

/// Why the encoder's events failed: thread C's next drain returns it, and
/// every one after.
#[derive(Debug)]
enum Fault {
    /// An event carried a failure, or the wait for one failed.
    Failed(windows::core::Error),
    /// The encoder announced more than it may: an input request beyond
    /// `MAX_INPUT_REQUESTS`, or an output beyond `MAX_IN_FLIGHT`.
    Overflow,
}
impl Fault {
    fn error(&self) -> std::io::Error {
        match self {
            Self::Failed(failure) => error(failure.clone()),
            Self::Overflow => error("invalid hardware encoder event queue"),
        }
    }
}

/// The encoder's events, counted: by its watcher, or by thread C when it
/// polls them. Thread C takes a request when it submits and an output when
/// it collects one.
struct Events {
    need_input: AtomicU32,
    have_output: AtomicU32,
    /// Thread C found no input request and waits for one: the watcher wakes
    /// it at the next.
    awaiting_input: AtomicBool,
    /// When each output was announced, in microseconds since `epoch`, at
    /// its sequence number modulo `OUTPUT_TIMES`.
    announced: [AtomicU64; OUTPUT_TIMES],
    epoch: Instant,
    /// The first failure.
    fault: OnceLock<Fault>,
    /// Set before the codec shuts the encoder down, so that its watcher
    /// takes the shutdown for its end rather than for a failure.
    stopping: AtomicBool,
}
impl Events {
    fn new() -> Self {
        Self {
            need_input: AtomicU32::new(0),
            have_output: AtomicU32::new(0),
            awaiting_input: AtomicBool::new(false),
            announced: [const { AtomicU64::new(0) }; OUTPUT_TIMES],
            epoch: Instant::now(),
            fault: OnceLock::new(),
            stopping: AtomicBool::new(false),
        }
    }

    /// Count `event`, with the announcement time of the `announced`th
    /// output. Whether thread C needs waking for it: for an output, and for
    /// an input request it waits for.
    fn observe(
        &self,
        event: &IMFMediaEvent,
        announced: &mut u64,
    ) -> std::result::Result<bool, Fault> {
        // SAFETY: COM calls on an event this thread holds.
        unsafe { event.GetStatus() }
            .and_then(|status| status.ok())
            .map_err(Fault::Failed)?;
        // SAFETY: as above.
        let kind = unsafe { event.GetType() }.map_err(Fault::Failed)?;
        if kind == METransformNeedInput.0 as u32 {
            if self.need_input.fetch_add(1, Ordering::SeqCst) >= MAX_INPUT_REQUESTS {
                return Err(Fault::Overflow);
            }
            // After the count, as thread C sets this before it reads the
            // count.
            Ok(self.awaiting_input.swap(false, Ordering::SeqCst))
        } else if kind == METransformHaveOutput.0 as u32 {
            let slot = (*announced % OUTPUT_TIMES as u64) as usize;
            if let Some(at) = self.announced.get(slot) {
                let since = u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX);
                at.store(since, Ordering::Relaxed);
            }
            *announced += 1;
            // Release: the time above is stored before the output counts.
            if self.have_output.fetch_add(1, Ordering::Release) >= MAX_IN_FLIGHT as u32 {
                return Err(Fault::Overflow);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn fail(&self, fault: Fault) {
        let _ = self.fault.set(fault);
    }

    fn check(&self) -> Result<()> {
        self.fault.get().map_or(Ok(()), |fault| Err(fault.error()))
    }
}

/// Where a watcher waits for events: the encoder's own generator, or a
/// test's event queue.
trait EventSource: Send + 'static {
    /// The next event, waiting for one.
    fn next_event(&self) -> windows::core::Result<IMFMediaEvent>;
}

/// The encoder's event generator, moved to its watcher.
struct Generator(IMFMediaEventGenerator);
// SAFETY: Media Foundation's objects, its hardware encoders included, are
// free-threaded, and both threads that call this one, thread C and the
// watcher, joined the process's multithreaded apartment, where an interface
// pointer serves every thread without marshaling. An asynchronous MFT's
// event generator is made to be waited on by a thread other than the one
// that feeds the MFT.
unsafe impl Send for Generator {}
impl EventSource for Generator {
    fn next_event(&self) -> windows::core::Result<IMFMediaEvent> {
        // SAFETY: a COM call on an interface this watcher holds; without
        // `MF_EVENT_FLAG_NO_WAIT` it waits for the next event.
        unsafe { self.0.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) }
    }
}

/// The thread that waits for the encoder's events.
struct Watcher {
    thread: JoinHandle<()>,
    /// Disconnects once the thread is done with COM.
    ended: mpsc::Receiver<()>,
}
impl Watcher {
    fn spawn<S: EventSource>(
        source: S,
        counts: Arc<Events>,
        waker: Waker,
    ) -> std::io::Result<Self> {
        let (exit, ended) = mpsc::sync_channel::<()>(0);
        let thread = std::thread::Builder::new()
            .name("pb-mapper-mft-events".into())
            .spawn(move || {
                watch(source, &counts, &waker);
                drop(exit);
            })?;
        Ok(Self { thread, ended })
    }

    /// Once the encoder was shut down: wait, at most `WATCHER_EXIT`, for
    /// the thread to end. Whether it did; one that did not is left behind,
    /// and later encoders poll their events (`WATCHER_STUCK`).
    fn join(self) -> bool {
        match self.ended.recv_timeout(WATCHER_EXIT) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = self.thread.join();
                true
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                WATCHER_STUCK.store(true, Ordering::Release);
                tracing::warn!(
                    target: "pb_mapper_desktop::session",
                    event = "windows_encoder_watcher_stuck",
                    "the encoder's shutdown did not end its event wait; later encoders poll theirs",
                );
                false
            }
        }
    }
}

/// The watcher: wait for each event, count it and wake thread C, until the
/// encoder shuts down, or an event or the wait fails, which wakes thread C
/// too.
fn watch<S: EventSource>(source: S, counts: &Events, waker: &Waker) {
    // SAFETY: balanced below, on this thread, when it succeeded.
    if let Err(failure) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        counts.fail(Fault::Failed(failure));
        waker.wake();
        return;
    }
    let mut announced = 0;
    loop {
        let fault = match source.next_event() {
            Ok(event) => match counts.observe(&event, &mut announced) {
                Ok(wake) => {
                    if wake {
                        waker.wake();
                    }
                    continue;
                }
                Err(fault) => fault,
            },
            Err(e) if e.code() == MF_E_SHUTDOWN && counts.stopping.load(Ordering::Acquire) => {
                break;
            }
            Err(e) => Fault::Failed(e),
        };
        counts.fail(fault);
        waker.wake();
        break;
    }
    // Its last interface goes before COM does.
    drop(source);
    // SAFETY: balances the `CoInitializeEx` above.
    unsafe { CoUninitialize() };
}

pub(super) fn attributes() -> Result<IMFAttributes> {
    let mut attributes = None;
    unsafe {
        MFCreateAttributes(&mut attributes, 4).map_err(error)?;
    }
    attributes.ok_or_else(|| error("missing media attributes"))
}
pub(super) fn media_type(
    subtype: &GUID,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<IMFMediaType> {
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
        VideoCodec::H264 | VideoCodec::Jpeg => MFVideoFormat_H264,
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
pub(super) fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>> {
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

    /// A Media Foundation event queue standing in for an encoder's event
    /// generator: hardware MFTs queue their events on one.
    struct Queue(IMFMediaEventQueue);
    // SAFETY: as `Generator`'s: the queue is free-threaded, and the test's
    // threads joined the multithreaded apartment.
    unsafe impl Send for Queue {}
    impl EventSource for Queue {
        fn next_event(&self) -> windows::core::Result<IMFMediaEvent> {
            // SAFETY: a COM call on an interface this thread holds; 0 waits.
            unsafe { self.0.GetEvent(0) }
        }
    }

    fn send(events: &IMFMediaEventQueue, kind: MF_EVENT_TYPE, status: windows::core::HRESULT) {
        // SAFETY: a COM call on an interface this thread holds; the GUID
        // outlives it.
        unsafe {
            events.QueueEventParamUnk(
                kind.0 as u32,
                &GUID::zeroed(),
                status,
                None::<&windows::core::IUnknown>,
            )
        }
        .unwrap();
    }

    /// One event as an encoder queues it, taken from a queue of its own.
    fn event(kind: MF_EVENT_TYPE, status: windows::core::HRESULT) -> IMFMediaEvent {
        // SAFETY: COM calls on a queue this thread made and holds.
        let queue = unsafe { MFCreateEventQueue() }.unwrap();
        send(&queue, kind, status);
        let event = unsafe { queue.GetEvent(MF_EVENT_FLAG_NO_WAIT.0) }.unwrap();
        unsafe { queue.Shutdown() }.unwrap();
        event
    }

    /// Until `done` holds, for at most 5 s.
    fn until(done: impl Fn() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < end, "timed out");
            std::thread::yield_now();
        }
    }

    /// Counting decides each wake: every output wakes thread C, and an
    /// input request only while thread C waits for one, which it then no
    /// longer does. A failed event, and more than the encoder may announce,
    /// are its failure.
    #[test]
    fn each_output_and_an_awaited_input_request_wake_thread_c() {
        use windows::Win32::Foundation::{E_FAIL, S_OK};
        if !crate::platform::windows::mf::media_foundation() {
            return;
        }
        let _runtime = super::super::Runtime::new().unwrap();
        let counts = Events::new();
        let mut seq = 0;
        let need_input = || counts.need_input.load(Ordering::SeqCst);
        let have_output = || counts.have_output.load(Ordering::Acquire);

        // Nobody waits for the request: counted, and no wake.
        let observed = counts.observe(&event(METransformNeedInput, S_OK), &mut seq);
        assert!(!observed.unwrap());
        assert_eq!(need_input(), 1);
        // Thread C waits for the next one: it is woken, and waits no more.
        counts.awaiting_input.store(true, Ordering::SeqCst);
        let observed = counts.observe(&event(METransformNeedInput, S_OK), &mut seq);
        assert!(observed.unwrap());
        assert_eq!(need_input(), 2);
        assert!(!counts.awaiting_input.load(Ordering::SeqCst));
        // Every output wakes thread C, and is numbered for its time.
        let observed = counts.observe(&event(METransformHaveOutput, S_OK), &mut seq);
        assert!(observed.unwrap());
        assert_eq!((have_output(), seq), (1, 1));
        // Any other event is neither counted nor woken for.
        let observed = counts.observe(&event(METransformDrainComplete, S_OK), &mut seq);
        assert!(!observed.unwrap());
        assert_eq!((need_input(), have_output(), seq), (2, 1, 1));

        let observed = counts.observe(&event(METransformHaveOutput, E_FAIL), &mut seq);
        assert!(matches!(observed, Err(Fault::Failed(_))));
        assert_eq!(have_output(), 1, "a failed event counts for nothing");
        for _ in 1..MAX_IN_FLIGHT {
            let observed = counts.observe(&event(METransformHaveOutput, S_OK), &mut seq);
            assert!(observed.unwrap());
        }
        let observed = counts.observe(&event(METransformHaveOutput, S_OK), &mut seq);
        assert!(matches!(observed, Err(Fault::Overflow)));
        for _ in 2..MAX_INPUT_REQUESTS {
            let observed = counts.observe(&event(METransformNeedInput, S_OK), &mut seq);
            assert!(!observed.unwrap());
        }
        let observed = counts.observe(&event(METransformNeedInput, S_OK), &mut seq);
        assert!(matches!(observed, Err(Fault::Overflow)));
    }

    /// The watcher counts what the encoder queues and wakes thread C for
    /// it, and ends, with its pending wait and without a wake, when the
    /// encoder shuts down. A failed event, more outputs than may be in
    /// flight and a shutdown nobody asked for each fail the encoder, wake
    /// thread C and end the watcher too.
    #[test]
    fn the_watcher_wakes_thread_c_and_ends_when_the_encoder_shuts_down() {
        use windows::Win32::Foundation::{E_FAIL, S_OK};
        if !crate::platform::windows::mf::media_foundation() {
            return;
        }
        let _runtime = super::super::Runtime::new().unwrap();
        let soon = || Instant::now() + Duration::from_secs(5);
        let watch = |events: &IMFMediaEventQueue, wake: &super::super::EncoderWake| {
            let counts = Arc::new(Events::new());
            let source = Queue(events.clone());
            let watcher = Watcher::spawn(source, counts.clone(), wake.waker().clone()).unwrap();
            (counts, watcher)
        };

        let wake = super::super::EncoderWake::new().unwrap();
        let events = unsafe { MFCreateEventQueue() }.unwrap();
        let (counts, watcher) = watch(&events, &wake);
        // Thread C waits for a request before the encoder makes any, so the
        // request's is the only wake there can be.
        counts.awaiting_input.store(true, Ordering::SeqCst);
        send(&events, METransformNeedInput, S_OK);
        assert!(wake.wait(soon()).unwrap());
        assert_eq!(counts.need_input.load(Ordering::SeqCst), 1);
        send(&events, METransformHaveOutput, S_OK);
        assert!(wake.wait(soon()).unwrap());
        assert_eq!(counts.have_output.load(Ordering::Acquire), 1);
        // Not needed for the outcome, which is the same either way: give the
        // watcher time to be waiting again when the encoder shuts down.
        std::thread::sleep(Duration::from_millis(20));
        counts.stopping.store(true, Ordering::Release);
        unsafe { events.Shutdown() }.unwrap();
        assert!(watcher.join(), "the shutdown ended the wait");
        counts.check().unwrap();
        assert!(
            !wake.wait(Instant::now()).unwrap(),
            "a shutdown asked for wakes nothing"
        );

        let failures: [&dyn Fn(&IMFMediaEventQueue); 3] = [
            &|events: &IMFMediaEventQueue| send(events, METransformHaveOutput, E_FAIL),
            &|events: &IMFMediaEventQueue| {
                for _ in 0..=MAX_IN_FLIGHT {
                    send(events, METransformHaveOutput, S_OK);
                }
            },
            &|events: &IMFMediaEventQueue| unsafe { events.Shutdown() }.unwrap(),
        ];
        for fail in failures {
            // A wake of its own, which no earlier case's wake can satisfy.
            let wake = super::super::EncoderWake::new().unwrap();
            let events = unsafe { MFCreateEventQueue() }.unwrap();
            let (counts, watcher) = watch(&events, &wake);
            fail(&events);
            assert!(wake.wait(soon()).unwrap());
            until(|| counts.check().is_err());
            assert!(watcher.join());
        }
    }
}
