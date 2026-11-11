//! The GPU's hardware H.264 or H.265 Media Foundation encoder, as the
//! pipeline drives it, and the media types and samples it takes.
use super::MAX_IN_FLIGHT;
use crate::{
    Result, VideoCodec,
    codec::annexb::{AccessUnit, MAX_ACCESS_UNIT, ParameterSets, codec_name},
    error,
};
use std::mem::ManuallyDrop;
use windows::{
    Win32::{
        Foundation::LUID,
        Graphics::{Direct3D11::ID3D11Device, Dxgi::IDXGIDevice},
        Media::MediaFoundation::*,
        System::{Com::CoTaskMemFree, Variant::VARIANT},
    },
    core::{GUID, Interface, PWSTR},
};

/// GOP sizes to request, longest first: keyframes come only on demand. Never
/// 0, which asks the encoder for its own default GOP rather than an endless one.
const GOP_CANDIDATES: [u32; 4] = [u32::MAX, 0x7FFF_FFFF, 65_535, 3_600];

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
    api: ICodecAPI,
    pub(super) name: String,
    pub(super) input_id: u32,
    output_id: u32,
    pub(super) need_input: u32,
    pub(super) have_output: u32,
    parameter_sets: ParameterSets,
    /// The NAL unit ranges of the unit being sent, kept across units.
    nal_units: Vec<(usize, usize)>,
    pub(super) gop: u32,
}
impl Codec {
    /// The first hardware encoder of `settings.kind` on `device`'s adapter
    /// that takes this pipeline's settings. There is no software fallback.
    pub(super) fn new(
        device: &ID3D11Device,
        manager: &IMFDXGIDeviceManager,
        settings: EncoderSettings,
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
            match Self::activate(&activation, name.clone(), manager, settings) {
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
            api,
            name,
            input_id: 0,
            output_id: 0,
            need_input: 0,
            have_output: 0,
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
        Ok(codec)
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
    pub(super) fn events(&mut self) -> Result<()> {
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
    pub(super) fn output(&mut self) -> Result<Option<IMFSample>> {
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
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
        }
    }
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
}
