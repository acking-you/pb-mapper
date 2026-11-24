//! The D3D11 video processor that turns each captured BGRA desktop picture
//! upright, scales it (or crops its odd edge) and converts it into the
//! encoder's NV12 samples on the GPU.
use super::SAMPLES;
use crate::{
    Result, error,
    scale::{Rotation, crop_only},
};
use std::mem::ManuallyDrop;
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
        Media::MediaFoundation::{IMFDXGIBuffer, IMFSample},
    },
    core::Interface,
};

/// The textures pictures are converted from whose views are kept: the
/// duplication's own surfaces, of which DXGI may hand out more than one,
/// or the one copy of them a fallback makes. The oldest view goes first.
const INPUT_VIEWS: usize = 4;

/// A sample of the pool, with the texture subresource that holds its NV12
/// picture, found once as the sample is allocated.
pub(super) struct Sample {
    pub(super) sample: IMFSample,
    pub(super) texture: ID3D11Texture2D,
    pub(super) subresource: u32,
}
impl Sample {
    pub(super) fn new(sample: IMFSample) -> Result<Self> {
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
        Ok(Self {
            sample,
            texture,
            subresource,
        })
    }
}

pub(super) struct Converter {
    device: ID3D11VideoDevice,
    context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    /// A view of each texture pictures were converted from, with that
    /// texture: at most `INPUT_VIEWS`. Each holds its texture, so a texture
    /// seen again at the same address is the same one.
    inputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    /// A view of each sample texture and subresource the pool has handed
    /// out, with that texture: at most `SAMPLES.1`.
    outputs: Vec<(ID3D11Texture2D, u32, ID3D11VideoProcessorOutputView)>,
}
impl Converter {
    /// A converter of `input` pictures, as captured, into `output` ones,
    /// upright: turned by `rotation`, which a video processor without the
    /// rotation cap refuses; cropped where the upright input is at most one
    /// pixel larger than `output` (`crop_only`), so that it copies 1:1 and
    /// leaves out the right column or bottom row; scaled otherwise. The
    /// processor favours `quality` or speed.
    pub(super) fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        input: (u32, u32),
        output: (u32, u32),
        rotation: Rotation,
        fps: u32,
        quality: bool,
    ) -> Result<Self> {
        let device: ID3D11VideoDevice = device.cast().map_err(error)?;
        let context: ID3D11VideoContext = context.cast().map_err(error)?;
        let rate = DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: if quality {
                D3D11_VIDEO_USAGE_OPTIMAL_QUALITY
            } else {
                D3D11_VIDEO_USAGE_OPTIMAL_SPEED
            },
        };
        let enumerator = unsafe {
            device
                .CreateVideoProcessorEnumerator(&desc)
                .map_err(error)?
        };
        let turn = processor_rotation(rotation);
        if turn.is_some() {
            let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
            unsafe { enumerator.GetVideoProcessorCaps(&mut caps).map_err(error)? };
            if caps.FeatureCaps & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION.0 as u32 == 0 {
                return Err(error(
                    "GPU video capture does not support a rotated display on this GPU: its video processor cannot rotate",
                ));
            }
        }
        let processor = unsafe { device.CreateVideoProcessor(&enumerator, 0).map_err(error)? };
        // The source rectangle is in the captured picture, before the turn;
        // the destination and target are the upright output.
        let upright = rotation.upright(input);
        let kept = if crop_only(upright, output) {
            output
        } else {
            upright
        };
        let [left, top, right, bottom] = rotation.source_rect(input, kept);
        let source = RECT {
            left: left as i32,
            top: top as i32,
            right: right as i32,
            bottom: bottom as i32,
        };
        let target = RECT {
            left: 0,
            top: 0,
            right: output.0 as i32,
            bottom: output.1 as i32,
        };
        unsafe {
            context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source));
            context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&target));
            context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&target));
            if let Some(turn) = turn {
                context.VideoProcessorSetStreamRotation(&processor, 0, true, turn);
            }
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
            inputs: Vec::with_capacity(INPUT_VIEWS),
            outputs: Vec::with_capacity(SAMPLES.1 as usize),
        })
    }
    /// Convert `source` into `target`. The views of both are created once
    /// and kept: each holds its texture, so a texture seen again is the same
    /// one. A video processor that refuses `source`, a duplication's own
    /// surface, fails here, before anything is queued.
    pub(super) fn convert(&mut self, source: &ID3D11Texture2D, target: &Sample) -> Result<()> {
        let input = self.input_view(source)?;
        let output = self.output_view(target)?;
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(input)),
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
    /// Drop the kept input views, and the textures they hold: the
    /// duplication's surfaces, once pictures are copied out of them instead.
    pub(super) fn forget_inputs(&mut self) {
        self.inputs.clear();
    }
    fn input_view(&mut self, source: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView> {
        if let Some((_, view)) = self
            .inputs
            .iter()
            .find(|(kept, _)| kept.as_raw() == source.as_raw())
        {
            return Ok(view.clone());
        }
        let mut input = None;
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
        }
        let input = input.ok_or_else(|| error("missing BGRA processor input"))?;
        if self.inputs.len() >= INPUT_VIEWS {
            self.inputs.remove(0);
        }
        self.inputs.push((source.clone(), input.clone()));
        Ok(input)
    }
    /// The view of the texture subresource that holds `target`.
    fn output_view(&mut self, target: &Sample) -> Result<ID3D11VideoProcessorOutputView> {
        let Sample {
            texture,
            subresource,
            sample: _,
        } = target;
        let subresource = *subresource;
        if let Some((_, _, view)) = self
            .outputs
            .iter()
            .find(|(kept, index, _)| kept.as_raw() == texture.as_raw() && *index == subresource)
        {
            return Ok(view.clone());
        }
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
        let mut output = None;
        unsafe {
            self.device
                .CreateVideoProcessorOutputView(
                    texture,
                    &self.enumerator,
                    &output_desc,
                    Some(&mut output),
                )
                .map_err(error)?;
        }
        let output = output.ok_or_else(|| error("missing NV12 processor output"))?;
        // A pool that replaced its textures: the views of the old ones go.
        if self.outputs.len() >= SAMPLES.1 as usize {
            self.outputs.clear();
        }
        self.outputs
            .push((texture.clone(), subresource, output.clone()));
        Ok(output)
    }
}

/// The video processor's turn for `rotation`, all clockwise
/// (`D3D11_VIDEO_PROCESSOR_ROTATION_90` turns the stream 90° clockwise);
/// `None` for none, which sets nothing.
pub(super) fn processor_rotation(rotation: Rotation) -> Option<D3D11_VIDEO_PROCESSOR_ROTATION> {
    match rotation {
        Rotation::Identity => None,
        Rotation::Cw90 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_90),
        Rotation::Cw180 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_180),
        Rotation::Cw270 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_270),
    }
}
