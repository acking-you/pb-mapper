//! Windows HDR desktops, turned into the SDR picture the video processor
//! converts.
//!
//! With HDR (or Auto Color Management) on, Windows composes the desktop in
//! FP16: linear scRGB, sRGB primaries, 1.0 = 80 nits, with SDR content at
//! the display's SDR white level (the SDR content brightness setting)
//! rather than at 1.0. A duplication that takes only BGRA8 gets DXGI's own
//! conversion of that desktop, which leaves the SDR white level out, so
//! SDR content arrives brighter or darker than the display shows it. The
//! encoder's duplication takes FP16 first, which DXGI hands out only for
//! such a desktop (an SDR desktop still arrives in BGRA8, which nothing
//! here touches), and each FP16 picture is drawn into a BGRA8 texture of
//! this module's by one full-screen triangle (`tonemap.hlsl`): divided by
//! the SDR white level thread C read as it built the encoder
//! (`sdr_white`), anything brighter than SDR white brought down to it by
//! one gain on every channel, and encoded sRGB. The video processor then
//! converts that texture, turning and scaling it, as it converts any
//! other.
//!
//! The shaders are compiled from `tonemap.hlsl` with `D3DCompile` as the
//! first FP16 picture arrives, once per process: `d3dcompiler_47.dll`,
//! which ships with Windows, is loaded from System32 for that and let go,
//! so that nothing imports it. The draw is recorded on a deferred context
//! once per texture DXGI hands out and replayed on the immediate context
//! with that context's state restored, since the video processor and the
//! encoder queue their work on it too. A tone map that cannot be built
//! turns FP16 duplication off for the process: the capture fails, and
//! its retry duplicates in BGRA8, which DXGI converts as before.
use crate::{Result, error};
use std::{
    ffi::c_void,
    num::NonZeroU32,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use windows::{
    Win32::{
        Devices::Display::{
            DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
            DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
            DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
            GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
        },
        Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, FreeLibrary, HMODULE},
        Graphics::{
            Direct3D::{
                D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_SHADER_MACRO,
                Fxc::D3DCOMPILE_OPTIMIZATION_LEVEL3, ID3DBlob,
            },
            Direct3D11::{
                D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
                D3D11_BUFFER_DESC, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
                D3D11_USAGE_DEFAULT, D3D11_USAGE_IMMUTABLE, D3D11_VIEWPORT, ID3D11Buffer,
                ID3D11CommandList, ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader,
                ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D,
                ID3D11VertexShader,
            },
            Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
        },
        System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
    },
    core::{HRESULT, Interface, PCSTR, s, w},
};

/// The shaders' source.
const HLSL: &str = include_str!("tonemap.hlsl");
/// scRGB's 1.0, in nits.
const SCRGB_WHITE_NITS: f32 = 80.0;
/// The textures pictures come from whose draw is kept: the duplication's
/// surfaces, of which DXGI may hand out more than one. The oldest goes
/// first.
const DRAWS: usize = 4;
/// How often the display paths are asked for again when a display arrived
/// between counting them and reading them.
const QUERY_ATTEMPTS: usize = 3;

/// A tone map could not be built in this process: duplications take BGRA8
/// only from then on.
static REFUSED: AtomicBool = AtomicBool::new(false);

/// Whether a duplication may take FP16 desktops: until a tone map could
/// not be built in this process.
pub(super) fn wanted() -> bool {
    !REFUSED.load(Ordering::Acquire)
}

/// A display's SDR white level, as `DISPLAYCONFIG_SDR_WHITE_LEVEL` gives
/// it: in thousandths of scRGB's 1.0 (80 nits), so 1000 is 80 nits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SdrWhite(NonZeroU32);
impl SdrWhite {
    /// The level `SDRWhiteLevel` reads; none for 0, which says nothing.
    pub(super) fn new(level: u32) -> Option<Self> {
        NonZeroU32::new(level).map(Self)
    }

    /// The level in nits.
    pub(super) fn nits(self) -> f32 {
        self.0.get() as f32 * SCRGB_WHITE_NITS / 1000.0
    }

    /// The gain that brings SDR white to 1.0: 80 nits over the level.
    pub(super) fn gain(self) -> f32 {
        1000.0 / self.0.get() as f32
    }
}

/// The gain the tone map divides by `white`: with no level read, 1.0,
/// which takes SDR white for scRGB's own 80 nits, as DXGI's conversion
/// does.
fn gain(white: Option<SdrWhite>) -> f32 {
    white.map_or(1.0, SdrWhite::gain)
}

/// The SDR white level of the display whose GDI device name (`\\.\DISPLAY1`,
/// `DXGI_OUTPUT_DESC::DeviceName`) is `device`: the active display path
/// whose source has that name, and its target's level. None where Windows
/// tells none, as for a process outside the console session.
pub(super) fn sdr_white(device: &[u16]) -> Option<SdrWhite> {
    let (mut paths, mut modes) = (Vec::new(), Vec::new());
    // A display that arrives between the two calls outgrows the arrays:
    // count again.
    for _ in 0..QUERY_ATTEMPTS {
        let (mut path_count, mut mode_count) = (0, 0);
        // SAFETY: out parameters the call fills.
        let counted = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if counted != ERROR_SUCCESS {
            return None;
        }
        paths.resize(path_count as usize, DISPLAYCONFIG_PATH_INFO::default());
        modes.resize(mode_count as usize, DISPLAYCONFIG_MODE_INFO::default());
        // SAFETY: arrays of the counts given, which the call fills and
        // lowers to what it wrote.
        let status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != ERROR_SUCCESS {
            return None;
        }
        paths.truncate(path_count as usize);
        return paths.iter().find_map(|path| path_white(path, device));
    }
    None
}

/// `path`'s target's SDR white level, if its source is the GDI device
/// `device`.
fn path_white(path: &DISPLAYCONFIG_PATH_INFO, device: &[u16]) -> Option<SdrWhite> {
    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: a request whose header, its first field, gives its type and
    // its whole size, which the call fills.
    let named = unsafe { DisplayConfigGetDeviceInfo((&raw mut source).cast()) };
    if named != 0 || !same_device(&source.viewGdiDeviceName, device) {
        return None;
    }
    let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
            size: size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        SDRWhiteLevel: 0,
    };
    // SAFETY: as above.
    let read = unsafe { DisplayConfigGetDeviceInfo((&raw mut white).cast()) };
    if read != 0 {
        return None;
    }
    SdrWhite::new(white.SDRWhiteLevel)
}

/// Whether two GDI device names, each up to its first NUL, are the same
/// device: an empty name is none.
fn same_device(a: &[u16], b: &[u16]) -> bool {
    fn name(name: &[u16]) -> &[u16] {
        name.split(|&unit| unit == 0).next().unwrap_or_default()
    }
    let (a, b) = (name(a), name(b));
    !a.is_empty() && a == b
}

/// `D3DCompile`, as `d3dcompiler_47.dll` exports it: `windows`' own
/// declaration of its entry.
type Compile = unsafe extern "system" fn(
    *const c_void,
    usize,
    PCSTR,
    *const D3D_SHADER_MACRO,
    *mut c_void,
    PCSTR,
    PCSTR,
    u32,
    u32,
    *mut *mut c_void,
    *mut *mut c_void,
) -> HRESULT;

/// The shaders' bytecode.
struct Bytecode {
    vertex: Vec<u8>,
    pixel: Vec<u8>,
}

/// The shaders' bytecode, compiled once per process; the `bool` says
/// whether this call compiled it.
fn bytecode() -> Result<(&'static Bytecode, bool)> {
    static BYTECODE: OnceLock<Bytecode> = OnceLock::new();
    if let Some(bytecode) = BYTECODE.get() {
        return Ok((bytecode, false));
    }
    let compiler = Compiler::load()?;
    let compiled = Bytecode {
        vertex: compiler.compile(s!("vertex"), s!("vs_4_0"))?,
        pixel: compiler.compile(s!("pixel"), s!("ps_4_0"))?,
    };
    drop(compiler);
    Ok((BYTECODE.get_or_init(|| compiled), true))
}

/// `d3dcompiler_47.dll`, loaded from System32 until this drops.
struct Compiler {
    module: HMODULE,
    compile: Compile,
}
impl Compiler {
    fn load() -> Result<Self> {
        // SAFETY: a NUL-terminated name; System32 only, so that no other
        // copy of the DLL is loaded in its place.
        let module =
            unsafe { LoadLibraryExW(w!("d3dcompiler_47.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(error)?;
        // SAFETY: the module just loaded, and a NUL-terminated name.
        let Some(entry) = (unsafe { GetProcAddress(module, s!("D3DCompile")) }) else {
            // SAFETY: the module loaded above, let go once.
            let _ = unsafe { FreeLibrary(module) };
            return Err(error("d3dcompiler_47.dll exports no D3DCompile"));
        };
        // SAFETY: the export `D3DCompile` has `Compile`'s signature.
        let compile =
            unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, Compile>(entry) };
        Ok(Self { module, compile })
    }

    /// The bytecode of `HLSL`'s function `entry` for shader model `target`.
    fn compile(&self, entry: PCSTR, target: PCSTR) -> Result<Vec<u8>> {
        let (mut code, mut messages) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: the source and its length, NUL-terminated names, no
        // macros or includes, and out parameters the call fills with a
        // referenced blob or leaves null.
        let compiled = unsafe {
            (self.compile)(
                HLSL.as_ptr().cast(),
                HLSL.len(),
                s!("tonemap.hlsl"),
                std::ptr::null(),
                std::ptr::null_mut(),
                entry,
                target,
                D3DCOMPILE_OPTIMIZATION_LEVEL3,
                0,
                &mut code,
                &mut messages,
            )
        };
        // SAFETY: each is null, or a blob the call referenced and that is
        // released as it drops.
        let (code, messages) = unsafe {
            (
                (!code.is_null()).then(|| ID3DBlob::from_raw(code)),
                (!messages.is_null()).then(|| ID3DBlob::from_raw(messages)),
            )
        };
        if let Err(failure) = compiled.ok() {
            let messages = messages.as_ref().map(blob_text).unwrap_or_default();
            return Err(error(format!(
                "D3DCompile of the HDR tone map's shaders failed ({failure}): {messages}"
            )));
        }
        code.as_ref()
            .map(|code| blob_bytes(code).to_vec())
            .ok_or_else(|| error("D3DCompile returned no bytecode"))
    }
}
impl Drop for Compiler {
    fn drop(&mut self) {
        // SAFETY: the module `load` loaded, let go once; nothing it made
        // outlives this, as each blob was copied out and released.
        let _ = unsafe { FreeLibrary(self.module) };
    }
}

/// `blob`'s bytes, while it lives.
fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    // SAFETY: a blob's buffer holds its size in bytes for as long as the
    // blob lives, which the returned slice's lifetime ties it to.
    unsafe {
        let (data, size) = (blob.GetBufferPointer(), blob.GetBufferSize());
        if data.is_null() {
            return &[];
        }
        std::slice::from_raw_parts(data.cast::<u8>(), size)
    }
}

/// A blob of the compiler's messages as text.
fn blob_text(blob: &ID3DBlob) -> String {
    String::from_utf8_lossy(blob_bytes(blob))
        .trim_end_matches('\0')
        .trim()
        .to_owned()
}

/// The capture thread's tone map of one encoder's FP16 desktop.
pub(super) struct ToneMap {
    /// Where each draw is recorded, so that the immediate context, which
    /// the video processor and the encoder share, keeps its state.
    deferred: ID3D11DeviceContext,
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    /// The pixel shader's constant: `gain`.
    gain: ID3D11Buffer,
    /// The SDR picture each draw writes and the video processor converts:
    /// BGRA8, at the captured size.
    target: ID3D11Texture2D,
    target_view: ID3D11RenderTargetView,
    size: (u32, u32),
    /// A copy of the FP16 picture to read, for a duplication whose
    /// surfaces the shader cannot: made as first needed.
    readable: Option<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
    /// The draw recorded for each texture pictures came from: at most
    /// `DRAWS`.
    draws: Vec<Draw>,
}
impl ToneMap {
    /// The tone map of `size` FP16 pictures on `device`, at the SDR white
    /// level `white`: compiled, and logged, as the first FP16 picture
    /// arrives. One that cannot be built turns FP16 duplication off for
    /// the process (`wanted`).
    pub(super) fn build(
        device: &ID3D11Device,
        size: (u32, u32),
        white: Option<SdrWhite>,
    ) -> Result<Self> {
        let started = Instant::now();
        match Self::new(device, size, gain(white)) {
            Ok((tone_map, compiled)) => {
                tracing::info!(
                    target: "pb_mapper_desktop::session",
                    event = "desktop_capture_hdr",
                    sdr_white_nits = ?white.map(SdrWhite::nits),
                    compiled,
                    build_ms = started.elapsed().as_secs_f64() * 1e3,
                );
                Ok(tone_map)
            }
            Err(failure) => {
                if !REFUSED.swap(true, Ordering::AcqRel) {
                    tracing::warn!(
                        target: "pb_mapper_desktop::session",
                        event = "desktop_capture_hdr_failed",
                        error = %failure,
                    );
                }
                Err(failure)
            }
        }
    }

    /// The tone map, and whether its shaders were compiled for it.
    fn new(device: &ID3D11Device, (width, height): (u32, u32), gain: f32) -> Result<(Self, bool)> {
        let (bytecode, compiled) = bytecode()?;
        let (mut deferred, mut vertex, mut pixel, mut constant) = (None, None, None, None);
        // A constant buffer is a whole number of 16 bytes.
        let constants = [gain, 0.0, 0.0, 0.0];
        let constant_desc = D3D11_BUFFER_DESC {
            ByteWidth: size_of_val(&constants) as u32,
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let constant_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: constants.as_ptr().cast(),
            ..Default::default()
        };
        let target_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let (mut target, mut target_view) = (None, None);
        // SAFETY: descriptions, data and bytecode that outlive each call,
        // and out parameters the calls fill.
        unsafe {
            device
                .CreateDeferredContext(0, Some(&mut deferred))
                .map_err(error)?;
            device
                .CreateVertexShader(&bytecode.vertex, None, Some(&mut vertex))
                .map_err(error)?;
            device
                .CreatePixelShader(&bytecode.pixel, None, Some(&mut pixel))
                .map_err(error)?;
            device
                .CreateBuffer(&constant_desc, Some(&constant_data), Some(&mut constant))
                .map_err(error)?;
            device
                .CreateTexture2D(&target_desc, None, Some(&mut target))
                .map_err(error)?;
        }
        let target = target.ok_or_else(|| error("missing SDR picture texture"))?;
        // SAFETY: a render target texture of this device, and an out
        // parameter the call fills.
        unsafe {
            device
                .CreateRenderTargetView(&target, None, Some(&mut target_view))
                .map_err(error)?;
        }
        let missing = |what: &str| error(format!("missing HDR tone map {what}"));
        Ok((
            Self {
                deferred: deferred.ok_or_else(|| missing("deferred context"))?,
                vertex: vertex.ok_or_else(|| missing("vertex shader"))?,
                pixel: pixel.ok_or_else(|| missing("pixel shader"))?,
                gain: constant.ok_or_else(|| missing("constant"))?,
                target,
                target_view: target_view.ok_or_else(|| missing("render target"))?,
                size: (width, height),
                readable: None,
                draws: Vec::with_capacity(DRAWS),
            },
            compiled,
        ))
    }

    /// Queue the draw of FP16 `frame` on `device`'s immediate `context`,
    /// which the caller holds, and return the SDR picture it writes. The
    /// draw is recorded once per texture DXGI hands out.
    pub(super) fn draw(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        frame: &ID3D11Texture2D,
    ) -> Result<&ID3D11Texture2D> {
        let kept = |draws: &[Draw]| {
            draws
                .iter()
                .position(|draw| draw.frame.as_raw() == frame.as_raw())
        };
        if kept(&self.draws).is_none() {
            let draw = self.record(device, frame)?;
            if self.draws.len() >= DRAWS {
                self.draws.remove(0);
            }
            self.draws.push(draw);
        }
        let draw = kept(&self.draws)
            .and_then(|index| self.draws.get(index))
            .ok_or_else(|| error("missing HDR tone map draw"))?;
        // SAFETY: a command list of this device, replayed on its immediate
        // context with that context's state put back after.
        unsafe { context.ExecuteCommandList(&draw.list, true) };
        Ok(&self.target)
    }

    /// Record the draw of `frame` into the SDR picture: read where DXGI
    /// hands it out where the shader can, else copied first into a
    /// texture it can read.
    fn record(&mut self, device: &ID3D11Device, frame: &ID3D11Texture2D) -> Result<Draw> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: an out parameter the call fills.
        unsafe { frame.GetDesc(&mut desc) };
        let deferred = &self.deferred;
        let in_place = (desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0)
            .then(|| shader_view(device, frame).ok())
            .flatten();
        let view = match in_place {
            Some(view) => view,
            None => {
                let (copy, view) = match &self.readable {
                    Some(readable) => readable.clone(),
                    None => {
                        let copy_desc = D3D11_TEXTURE2D_DESC {
                            MipLevels: 1,
                            ArraySize: 1,
                            Usage: D3D11_USAGE_DEFAULT,
                            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                            CPUAccessFlags: 0,
                            MiscFlags: 0,
                            ..desc
                        };
                        let mut copy = None;
                        // SAFETY: a description and an out parameter that
                        // outlive the call.
                        unsafe {
                            device
                                .CreateTexture2D(&copy_desc, None, Some(&mut copy))
                                .map_err(error)?;
                        }
                        let copy = copy.ok_or_else(|| error("missing FP16 picture copy"))?;
                        let view = shader_view(device, &copy)?;
                        self.readable.insert((copy, view)).clone()
                    }
                };
                // SAFETY: two textures of the same size and format on
                // this device.
                unsafe { deferred.CopyResource(&copy, frame) };
                view
            }
        };
        let (width, height) = self.size;
        let mut list = None;
        // SAFETY: calls on the deferred context this tone map alone
        // records on, with objects of its device, and an out parameter the
        // last call fills.
        unsafe {
            deferred.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            deferred.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            deferred.VSSetShader(&self.vertex, None);
            deferred.PSSetShader(&self.pixel, None);
            deferred.PSSetConstantBuffers(0, Some(&[Some(self.gain.clone())]));
            deferred.PSSetShaderResources(0, Some(&[Some(view.clone())]));
            deferred.OMSetRenderTargets(Some(&[Some(self.target_view.clone())]), None);
            deferred.Draw(3, 0);
            deferred
                .FinishCommandList(false, Some(&mut list))
                .map_err(error)?;
        }
        Ok(Draw {
            frame: frame.clone(),
            _view: view,
            list: list.ok_or_else(|| error("missing HDR tone map command list"))?,
        })
    }
}

/// A draw recorded for one texture pictures come from.
struct Draw {
    /// The texture, held so that one seen again at the same address is
    /// the same one.
    frame: ID3D11Texture2D,
    /// The view the list reads it through, held as long as the list.
    _view: ID3D11ShaderResourceView,
    list: ID3D11CommandList,
}

/// A view the shader reads `texture` through.
fn shader_view(
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
) -> Result<ID3D11ShaderResourceView> {
    let mut view = None;
    // SAFETY: a texture of this device, and an out parameter the call
    // fills.
    unsafe {
        device
            .CreateShaderResourceView(texture, None, Some(&mut view))
            .map_err(error)?;
    }
    view.ok_or_else(|| error("missing FP16 picture view"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::{
        Direct3D::D3D_DRIVER_TYPE_WARP,
        Direct3D11::{
            D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
            D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_USAGE_STAGING, D3D11CreateDevice,
        },
        Dxgi::{Common::DXGI_FORMAT_R16G16B16A16_FLOAT, CreateDXGIFactory1, IDXGIFactory1},
    };

    /// The SDR white level reads in thousandths of 80 nits: 1000 is
    /// scRGB's 1.0, which a gain of 1 leaves; 2500 is 200 nits, which
    /// 0.4 brings to 1.0; 0 says nothing, and nothing read takes SDR white
    /// for 80 nits.
    #[test]
    fn the_sdr_white_level_is_thousandths_of_80_nits() {
        let white = |level| SdrWhite::new(level).unwrap();
        assert_eq!(white(1000).nits(), 80.0);
        assert_eq!(white(1000).gain(), 1.0);
        assert_eq!(white(2500).nits(), 200.0);
        assert_eq!(white(2500).gain(), 0.4);
        assert_eq!(white(6000).nits(), 480.0);
        assert_eq!(SdrWhite::new(0), None);
        assert_eq!(gain(None), 1.0);
        assert_eq!(gain(Some(white(4000))), 0.25);
    }

    /// A display path's source matches the output by its GDI device name,
    /// up to the first NUL of each fixed array; an empty name is none.
    #[test]
    fn a_display_path_matches_the_output_by_gdi_device_name() {
        let name = |text: &str| {
            let mut name = [0u16; 32];
            for (unit, from) in name.iter_mut().zip(text.encode_utf16()) {
                *unit = from;
            }
            name
        };
        assert!(same_device(&name(r"\\.\DISPLAY1"), &name(r"\\.\DISPLAY1")));
        assert!(!same_device(
            &name(r"\\.\DISPLAY1"),
            &name(r"\\.\DISPLAY12")
        ));
        assert!(!same_device(&name(r"\\.\DISPLAY2"), &name(r"\\.\DISPLAY1")));
        assert!(!same_device(&name(""), &name("")));
        // What follows the NUL does not count.
        let mut trailing = name(r"\\.\DISPLAY1");
        trailing[20] = u16::from(b'X');
        assert!(same_device(&trailing, &name(r"\\.\DISPLAY1")));
    }

    /// Reading a real output's SDR white level never faults: each DXGI
    /// output of this machine reads a level or none (a process outside the
    /// console session reads none), and a name no display has reads none.
    #[test]
    fn reading_the_sdr_white_level_of_each_output_never_faults() {
        assert_eq!(sdr_white(&[0; 32]), None);
        assert_eq!(sdr_white(&[u16::from(b'X'); 32]), None);
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.unwrap();
        for adapter in (0..).map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok()) {
            for output in (0..).map_while(|index| unsafe { adapter.EnumOutputs(index) }.ok()) {
                let desc = unsafe { output.GetDesc() }.unwrap();
                let white = sdr_white(&desc.DeviceName);
                println!(
                    "{}: {:?} nits",
                    String::from_utf16_lossy(&desc.DeviceName).trim_end_matches('\0'),
                    white.map(SdrWhite::nits)
                );
            }
        }
    }

    /// A D3D11 device on WARP, the software rasterizer every Windows has.
    fn warp() -> (ID3D11Device, ID3D11DeviceContext) {
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .unwrap();
        }
        (device.unwrap(), context.unwrap())
    }

    /// An IEEE half of `value`, a normal number or zero, rounded to
    /// nearest.
    fn half(value: f32) -> u16 {
        if value == 0.0 {
            return 0;
        }
        let bits = value.to_bits();
        let sign = ((bits >> 16) & 0x8000) as u16;
        let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
        assert!((1..31).contains(&exponent), "{value} is no normal half");
        let mantissa = bits & 0x007f_ffff;
        // Rounded to nearest; a carry into the exponent is still right.
        let rounded = ((exponent as u32) << 10) + ((mantissa + 0x1000) >> 13);
        sign | rounded as u16
    }

    /// The tone map, compiled with D3DCompile and drawn on WARP, turns an
    /// FP16 desktop at an SDR white level of 160 nits into the SDR picture
    /// the display shows: SDR white is 255, a mid-grey at that level keeps
    /// its sRGB value, black stays black, a highlight brighter than SDR
    /// white comes down to it with its hue, and a colour outside the sRGB
    /// gamut is clipped to it. From a texture the shader reads where it is
    /// and from one it must copy first, and again from the draw recorded
    /// for each.
    #[test]
    fn an_fp16_desktop_is_drawn_as_the_sdr_picture_the_display_shows() {
        let (device, context) = warp();
        let (width, height) = (3, 2);
        // 160 nits: SDR white is scRGB 2.0.
        let white = SdrWhite::new(2000);
        // scRGB pixels and the BGRA8 each must become: 0.735357 (the sRGB
        // value of linear 0.5) is 187.52; that of 0.025 is 43.83; that of
        // 0.25 is 136.96.
        let cases: [([f32; 4], [u8; 4]); 6] = [
            ([2.0, 2.0, 2.0, 1.0], [255, 255, 255, 255]),
            ([1.0, 1.0, 1.0, 1.0], [188, 188, 188, 255]),
            ([0.0, 0.0, 0.0, 1.0], [0, 0, 0, 255]),
            // Twice SDR white's red at 4, half of it in green: down to
            // (1, 0.5, 0.025), the same hue.
            ([8.0, 4.0, 0.2, 1.0], [44, 188, 255, 255]),
            // Outside the gamut: red below 0.
            ([-1.0, 0.0, 0.5, 1.0], [137, 0, 0, 255]),
            ([2.0, 0.0, 0.0, 1.0], [0, 0, 255, 255]),
        ];
        let pixels: Vec<u16> = cases.iter().flat_map(|(rgba, _)| rgba.map(half)).collect();
        let expected: Vec<u8> = cases.iter().flat_map(|(_, bgra)| *bgra).collect();
        let mut tone_map = ToneMap::build(&device, (width, height), white).unwrap();
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        unsafe {
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .unwrap();
        }
        let staging = staging.unwrap();
        let zeros = vec![0u8; (width * height * 4) as usize];
        for bind in [D3D11_BIND_SHADER_RESOURCE.0 as u32, 0] {
            let desc = D3D11_TEXTURE2D_DESC {
                Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: bind,
                CPUAccessFlags: 0,
                ..staging_desc
            };
            let data = D3D11_SUBRESOURCE_DATA {
                pSysMem: pixels.as_ptr().cast(),
                SysMemPitch: width * 8,
                SysMemSlicePitch: 0,
            };
            let mut frame = None;
            unsafe {
                device
                    .CreateTexture2D(&desc, Some(&data), Some(&mut frame))
                    .unwrap();
            }
            let frame = frame.unwrap();
            // Recorded, then replayed from what was recorded.
            for round in 0..2 {
                // Cleared first, so that a draw that wrote nothing fails.
                unsafe {
                    context.UpdateSubresource(
                        &tone_map.target,
                        0,
                        None,
                        zeros.as_ptr().cast(),
                        width * 4,
                        0,
                    );
                }
                let target = tone_map.draw(&device, &context, &frame).unwrap().clone();
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                let mut got = Vec::new();
                unsafe {
                    context.CopyResource(&staging, &target);
                    context
                        .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                        .unwrap();
                    for row in 0..height as usize {
                        let start = mapped
                            .pData
                            .cast::<u8>()
                            .add(row * mapped.RowPitch as usize);
                        got.extend_from_slice(std::slice::from_raw_parts(
                            start,
                            width as usize * 4,
                        ));
                    }
                    context.Unmap(&staging, 0);
                }
                assert!(
                    got.iter()
                        .zip(&expected)
                        .all(|(got, want)| got.abs_diff(*want) <= 1),
                    "bind {bind:#x}, round {round}: {got:?}, not {expected:?}"
                );
            }
        }
        // Each source texture has its own recorded draw; the copy is made
        // once.
        assert_eq!(tone_map.draws.len(), 2);
        assert!(tone_map.readable.is_some());
    }
}
