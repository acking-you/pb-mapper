//! The D3D11 video processor that scales the captured BGRA desktop into the
//! encoder's NV12 samples on the GPU.
use crate::{Result, error};
use std::mem::ManuallyDrop;
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
        Media::MediaFoundation::{IMFDXGIBuffer, IMFSample},
    },
    core::Interface,
};

pub(super) struct Converter {
    device: ID3D11VideoDevice,
    context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}
impl Converter {
    pub(super) fn new(
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
    pub(super) fn convert(&self, source: &ID3D11Texture2D, sample: &IMFSample) -> Result<()> {
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
