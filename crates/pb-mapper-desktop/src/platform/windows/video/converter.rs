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
        self.convert_into(source, &target.texture, target.subresource)
    }
    /// Convert `source` into `subresource` of NV12 `texture`.
    fn convert_into(
        &mut self,
        source: &ID3D11Texture2D,
        texture: &ID3D11Texture2D,
        subresource: u32,
    ) -> Result<()> {
        let input = self.input_view(source)?;
        let output = self.output_view(texture, subresource)?;
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
    /// The view of `subresource` of `texture`, which holds a sample's
    /// picture.
    fn output_view(
        &mut self,
        texture: &ID3D11Texture2D,
        subresource: u32,
    ) -> Result<ID3D11VideoProcessorOutputView> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1},
        },
    };

    /// The test picture's shades of grey, which every RGB-to-YCbCr matrix
    /// turns into the same luma, so that only where each lands is tested.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Shade {
        Black,
        Dark,
        /// The upright picture's right column and bottom row, which a crop
        /// leaves out.
        Edge,
        Light,
        White,
    }
    impl Shade {
        const ALL: [Self; 5] = [
            Self::Black,
            Self::Dark,
            Self::Edge,
            Self::Light,
            Self::White,
        ];
        fn level(self) -> u8 {
            match self {
                Self::Black => 0,
                Self::Dark => 64,
                Self::Edge => 128,
                Self::Light => 192,
                Self::White => 255,
            }
        }
        /// The shade an NV12 luma sample is nearest, in studio range (16 +
        /// 219 × level / 255, as the converter writes) or full.
        fn of_luma(luma: u8) -> Self {
            let distance = |shade: Self| {
                let studio = 16 + u32::from(shade.level()) * 219 / 255;
                u32::from(luma)
                    .abs_diff(studio)
                    .min(luma.abs_diff(shade.level()).into())
            };
            Self::ALL
                .into_iter()
                .min_by_key(|shade| distance(*shade))
                .unwrap()
        }
        /// Upright pixel `(x, y)` of a picture whose top-left `kept` is
        /// sent: a quadrant's shade of that, and the edge beyond it.
        fn at((x, y): (u32, u32), kept: (u32, u32)) -> Self {
            if x >= kept.0 || y >= kept.1 {
                return Self::Edge;
            }
            match (x < kept.0 / 2, y < kept.1 / 2) {
                (true, true) => Self::Black,
                (false, true) => Self::Dark,
                (true, false) => Self::Light,
                (false, false) => Self::White,
            }
        }
    }

    /// A D3D11 device with video support on each hardware adapter, with the
    /// adapter's name.
    fn devices() -> Vec<(String, ID3D11Device, ID3D11DeviceContext)> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.unwrap();
        let mut devices = Vec::new();
        for index in 0.. {
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            let desc = unsafe { adapter.GetDesc1() }.unwrap();
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let name = String::from_utf16_lossy(&desc.Description)
                .trim_end_matches('\0')
                .to_owned();
            let (mut device, mut context) = (None, None);
            let created = unsafe {
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            match (created, device, context) {
                (Ok(()), Some(device), Some(context)) => devices.push((name, device, context)),
                (created, ..) => println!("{name}: no D3D11 video device: {created:?}"),
            }
        }
        devices
    }

    /// A `size` texture of `format`: one the video processor writes, or,
    /// given `pixels`, one it reads, bound as the duplication's surfaces
    /// are; or a `staging` one to read back through.
    fn texture(
        device: &ID3D11Device,
        format: DXGI_FORMAT,
        (width, height): (u32, u32),
        pixels: Option<&[[u8; 4]]>,
        staging: bool,
    ) -> ID3D11Texture2D {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: if staging {
                D3D11_USAGE_STAGING
            } else {
                D3D11_USAGE_DEFAULT
            },
            BindFlags: if staging {
                0
            } else if pixels.is_some() {
                (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32
            } else {
                D3D11_BIND_RENDER_TARGET.0 as u32
            },
            CPUAccessFlags: if staging {
                D3D11_CPU_ACCESS_READ.0 as u32
            } else {
                0
            },
            MiscFlags: 0,
        };
        let data = pixels.map(|pixels| D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        });
        let mut texture = None;
        unsafe {
            device
                .CreateTexture2D(
                    &desc,
                    data.as_ref().map(std::ptr::from_ref),
                    Some(&mut texture),
                )
                .unwrap();
        }
        texture.unwrap()
    }

    /// The luma plane of NV12 `texture`, `size` and packed, read back once
    /// the GPU has written it.
    fn luma(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        texture: &ID3D11Texture2D,
        (width, height): (u32, u32),
    ) -> Vec<u8> {
        let staging = self::texture(device, DXGI_FORMAT_NV12, (width, height), None, true);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            context.CopyResource(&staging, texture);
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .unwrap();
        }
        let pitch = mapped.RowPitch as usize;
        // SAFETY: a mapped NV12 staging texture starts with `height` luma
        // rows `pitch` bytes apart, until `Unmap`.
        let rows = unsafe {
            std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * height as usize)
        };
        let luma = rows
            .chunks(pitch)
            .flat_map(|row| &row[..width as usize])
            .copied()
            .collect();
        unsafe { context.Unmap(&staging, 0) };
        luma
    }

    /// The video processor, set up as `Converter::new` sets it, turns a
    /// captured picture upright by the clockwise turn `scale::Rotation`
    /// names; a crop copies the upright picture pixel for pixel and leaves
    /// out its right column and bottom row, whatever the turn; and a
    /// downscale keeps each quadrant where it is. On each hardware
    /// adapter, at speed and at quality; a video processor without the
    /// rotation cap is reported and its turns skipped. A device check:
    /// `cargo test -p pb-mapper-desktop --all-features --lib converter --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "needs a GPU with a D3D11 video processor: run on a Windows device"]
    fn the_video_processor_turns_crops_and_scales_as_the_geometry_says() {
        let devices = devices();
        assert!(
            !devices.is_empty(),
            "no hardware adapter with a D3D11 video device"
        );
        let mut failures = Vec::new();
        for (name, device, context) in &devices {
            for rotation in [
                Rotation::Identity,
                Rotation::Cw90,
                Rotation::Cw180,
                Rotation::Cw270,
            ] {
                // An odd upright desktop cropped to the even size below it,
                // and an even one halved.
                for (upright, output) in [((161, 97), (160, 96)), ((256, 128), (128, 64))] {
                    let cropped = crop_only(upright, output);
                    let kept = if cropped { output } else { upright };
                    // A quarter turn's swap is its own inverse: the size the
                    // upright picture is captured at.
                    let captured = rotation.upright(upright);
                    let pixels: Vec<[u8; 4]> = (0..captured.1)
                        .flat_map(|y| (0..captured.0).map(move |x| (x, y)))
                        .map(|point| {
                            let level =
                                Shade::at(rotation.upright_point(point, captured), kept).level();
                            [level, level, level, 255]
                        })
                        .collect();
                    let source = texture(
                        device,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        captured,
                        Some(&pixels),
                        false,
                    );
                    for quality in [false, true] {
                        let case = format!(
                            "{name} {rotation:?} {captured:?} -> {output:?} quality={quality}"
                        );
                        let mut converter = match Converter::new(
                            device, context, captured, output, rotation, 30, quality,
                        ) {
                            Ok(converter) => converter,
                            Err(failure) if rotation != Rotation::Identity => {
                                println!("{case}: skipped: {failure}");
                                continue;
                            }
                            Err(failure) => panic!("{case}: {failure}"),
                        };
                        let target = texture(device, DXGI_FORMAT_NV12, output, None, false);
                        converter.convert_into(&source, &target, 0).unwrap();
                        let luma = luma(device, context, &target, output);
                        // A crop is checked at every pixel; a scaling at each
                        // quadrant's middle, away from the filtered borders.
                        let points: Vec<(u32, u32)> = if cropped {
                            (0..output.1)
                                .flat_map(|y| (0..output.0).map(move |x| (x, y)))
                                .collect()
                        } else {
                            [1, 3]
                                .into_iter()
                                .flat_map(|j| [1, 3].map(|i| (output.0 * i / 4, output.1 * j / 4)))
                                .collect()
                        };
                        let wrong: Vec<String> = points
                            .into_iter()
                            .filter_map(|(x, y)| {
                                let want =
                                    Shade::at((x * kept.0 / output.0, y * kept.1 / output.1), kept);
                                let got = Shade::of_luma(luma[(y * output.0 + x) as usize]);
                                (got != want).then(|| format!("({x}, {y}) {got:?} not {want:?}"))
                            })
                            .collect();
                        println!("{case}: {} wrong", wrong.len());
                        if !wrong.is_empty() {
                            failures.push(format!(
                                "{case}: {} wrong, first {:?}",
                                wrong.len(),
                                &wrong[..wrong.len().min(8)]
                            ));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
