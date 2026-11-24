//! The Windows host's capture thread: Desktop Duplication as a push source
//! (`codec::present_waiter`), so that thread C no longer polls DXGI.
//!
//! Thread C builds the duplication, the video processor and the encoder,
//! then hands the duplication and the video processor to this thread. Its
//! first calls put it on thread C's desktop (the privileged worker's bound
//! input desktop, Winlogon's included; in the app's process the one it
//! already has), under per-monitor DPI awareness, in the multithreaded
//! apartment. Armed, it waits in `AcquireNextFrame`, at most `ACQUIRE_WAIT`
//! a time. A frame with a new present, or the duplication's first frame,
//! is converted at once into the sample thread C handed over (straight from
//! the duplication's surface, or copied out of it first where the video
//! processor refuses that), the GPU is handed the work and the frame goes
//! back to DXGI, and the picture is published, dated by its present. A
//! frame that only moved the pointer is released and waited past.
//!
//! Both threads queue work on the device's one immediate context: each
//! holds the device's own lock (`ID3D11Multithread`, which the encoder's
//! device manager turns on) across a picture's work, so that neither's
//! calls land between the other's.

use super::{
    FirstAcquire, Fresh,
    converter::{Converter, Sample},
    gpu_time::{GpuSpan, GpuTimer},
    present_instant,
};
use crate::{Result, codec::present_waiter::PresentSource, error};
use parking_lot::Mutex;
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use windows::{
    Win32::{
        Graphics::{
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device,
                ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
            },
            Dxgi::{DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIResource},
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
            StationsAndDesktops::{GetThreadDesktop, SetThreadDesktop},
            Threading::GetCurrentThreadId,
        },
        UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
    },
    core::Interface,
};
use windows_capture::dxgi_duplication_api::DxgiDuplicationApi;

/// The longest one `AcquireNextFrame` waits: what bounds how long the
/// thread takes to see that it was stopped, and so the join as an encoder
/// goes.
pub(super) const ACQUIRE_WAIT: Duration = Duration::from_millis(16);

/// A converted new picture: its sample, and when the display presented it.
pub(super) struct Picture {
    pub(super) sample: Sample,
    pub(super) fresh: Fresh,
}

/// What thread C and the capture thread share besides the pictures:
/// `video_bench`'s requests, what the capture reports for `Statistics`,
/// and the device's lock.
pub(super) struct Shared {
    /// `copy_captures` asked for each picture to be copied out of the
    /// duplication first.
    copy: AtomicBool,
    /// Pictures are converted straight from the duplication's surface.
    direct: AtomicBool,
    /// `time_gpu` asked for each picture's GPU work to be timed, in
    /// `gpu_timer`.
    timing: AtomicBool,
    gpu_timer: Mutex<Option<GpuTimer>>,
    first_acquire: OnceLock<FirstAcquire>,
    /// `AcquireNextFrame` calls, whatever each returned.
    acquires: AtomicU64,
    multithread: ID3D11Multithread,
}
impl Shared {
    /// What the threads of the device `context` belongs to share; its
    /// multithread protection is on.
    pub(super) fn new(context: &ID3D11DeviceContext) -> Result<Self> {
        Ok(Self {
            copy: AtomicBool::new(false),
            direct: AtomicBool::new(true),
            timing: AtomicBool::new(false),
            gpu_timer: Mutex::new(None),
            first_acquire: OnceLock::new(),
            acquires: AtomicU64::new(0),
            multithread: context.cast().map_err(error)?,
        })
    }

    /// Copy each later picture out of the duplication before converting
    /// it.
    pub(super) fn copy_captures(&self) {
        self.copy.store(true, Ordering::Release);
        self.direct.store(false, Ordering::Release);
    }

    pub(super) fn direct(&self) -> bool {
        self.direct.load(Ordering::Acquire)
    }

    pub(super) fn first_acquire(&self) -> Option<FirstAcquire> {
        self.first_acquire.get().copied()
    }

    pub(super) fn acquires(&self) -> u64 {
        self.acquires.load(Ordering::Relaxed)
    }

    /// Time each later picture's GPU work.
    pub(super) fn time_gpu(&self) {
        self.gpu_timer.lock().get_or_insert_with(GpuTimer::default);
        self.timing.store(true, Ordering::Release);
    }

    /// Move the spans the GPU has finished into `out`; never waits.
    pub(super) fn gpu_times(&self, context: &ID3D11DeviceContext, out: &mut Vec<GpuSpan>) {
        if let Some(timer) = self.gpu_timer.lock().as_mut() {
            timer.collect(context, out);
        }
    }

    /// Do `work`, a picture's work (a `repeat` or a new one) on `device`'s
    /// immediate `context`, holding the device's lock, and time it on the
    /// GPU where `time_gpu` asked. With `flush`, the GPU is handed the work
    /// once it succeeded, the span's closing timestamps with it.
    pub(super) fn work<R>(
        &self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        repeat: bool,
        flush: bool,
        work: impl FnOnce() -> Result<R>,
    ) -> Result<R> {
        let mut timer = self
            .timing
            .load(Ordering::Acquire)
            .then(|| self.gpu_timer.lock());
        let mut timer = timer.as_deref_mut().and_then(Option::as_mut);
        let _device = DeviceLock::enter(&self.multithread);
        let queries = match timer.as_deref_mut() {
            Some(timer) => timer.begin(device, context)?,
            None => None,
        };
        let done = work();
        if let (Some(timer), Some(queries)) = (timer, queries) {
            timer.end(context, queries, repeat);
        }
        if flush && done.is_ok() {
            // SAFETY: a call on the context the caller holds, under the
            // device's lock.
            unsafe { context.Flush() };
        }
        done
    }
}

/// The device's own lock, held until this drops.
struct DeviceLock<'a>(&'a ID3D11Multithread);
impl<'a> DeviceLock<'a> {
    fn enter(multithread: &'a ID3D11Multithread) -> Self {
        // SAFETY: a COM call on an interface the caller holds; `Leave`
        // balances it as this drops.
        unsafe { multithread.Enter() };
        Self(multithread)
    }
}
impl Drop for DeviceLock<'_> {
    fn drop(&mut self) {
        // SAFETY: balances the `Enter` of `enter`, on the same thread.
        unsafe { self.0.Leave() };
    }
}

/// The capture thread's part in the multithreaded apartment, left as it
/// drops: last, once the rest of the source has gone.
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances the `CoInitializeEx` that made this, on its
        // thread.
        unsafe { CoUninitialize() };
    }
}

/// Desktop Duplication, and the video processor that converts its
/// pictures, on the capture thread.
pub(super) struct DxgiSource {
    capture: DxgiDuplicationApi,
    converter: Converter,
    /// The captured picture's size, as scanned out: what the first frame's
    /// texture must be.
    input: (u32, u32),
    shared: Arc<Shared>,
    /// Thread C's id: the thread whose desktop this one joins.
    thread_c: u32,
    /// Pictures are converted straight from the duplication's surface:
    /// until the video processor refuses it, after which each is copied out
    /// into `copied` first.
    direct: bool,
    copied: Option<ID3D11Texture2D>,
    /// The last converted picture's `LastPresentTime`.
    last_present: i64,
    /// No picture was converted yet.
    first: bool,
    /// Last, so that the apartment is left after everything above went.
    apartment: Option<Apartment>,
}
impl DxgiSource {
    /// On thread C, which built `capture` and `converter` for `input`
    /// pictures.
    pub(super) fn new(
        capture: DxgiDuplicationApi,
        converter: Converter,
        input: (u32, u32),
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            capture,
            converter,
            input,
            shared,
            // SAFETY: no arguments; it returns the calling thread's id.
            thread_c: unsafe { GetCurrentThreadId() },
            direct: true,
            copied: None,
            last_present: 0,
            first: true,
            apartment: None,
        }
    }

    /// Convert the frame `next` holds into the sample `take` hands out, if
    /// it shows a new picture, and hand the GPU that work before the frame
    /// goes back to DXGI.
    fn convert_frame(
        &mut self,
        info: &DXGI_OUTDUPL_FRAME_INFO,
        resource: Option<IDXGIResource>,
        take: impl FnOnce() -> Option<Sample>,
    ) -> Result<Option<Picture>> {
        let present = info.LastPresentTime;
        // Zero: only the pointer moved. The duplication's first frame is
        // converted whatever it says, so that a still desktop shows at once.
        if !(self.first && resource.is_some()) && (present == 0 || present == self.last_present) {
            return Ok(None);
        }
        let frame: ID3D11Texture2D = resource
            .ok_or_else(|| error("missing desktop frame"))?
            .cast()
            .map_err(error)?;
        if self.first {
            // The converter's source rectangle and turn take the frame at
            // the size its display mode is scanned out at, a rotated
            // display's included.
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: an out parameter the call fills.
            unsafe { frame.GetDesc(&mut desc) };
            if (desc.Width, desc.Height) != self.input {
                return Err(error(format!(
                    "the desktop image is {}x{}, not the {}x{} its display mode and rotation give",
                    desc.Width, desc.Height, self.input.0, self.input.1
                )));
            }
        }
        let sample = take().ok_or_else(|| error("capture thread armed without a sample"))?;
        let Self {
            capture,
            converter,
            shared,
            direct,
            copied,
            ..
        } = self;
        if *direct && shared.copy.load(Ordering::Acquire) {
            *direct = false;
            converter.forget_inputs();
        }
        let (device, context) = (capture.device(), capture.device_context());
        shared.work(device, context, false, true, || {
            if *direct {
                if let Err(failure) = converter.convert(&frame, &sample) {
                    *direct = false;
                    shared.direct.store(false, Ordering::Release);
                    converter.forget_inputs();
                    tracing::info!(
                        target: "pb_mapper_desktop::session",
                        event = "desktop_capture_copied",
                        error = %failure,
                    );
                    convert_copy(device, context, copied, converter, &frame, &sample)?;
                }
            } else {
                convert_copy(device, context, copied, converter, &frame, &sample)?;
            }
            Ok(())
        })?;
        if present != 0 {
            self.last_present = present;
        }
        let at = if std::mem::take(&mut self.first) {
            None
        } else {
            present_instant(present)
        };
        Ok(Some(Picture {
            sample,
            fresh: Fresh {
                at,
                presents: info.AccumulatedFrames.max(1),
            },
        }))
    }
}

/// Copy `frame` out of the duplication into `copied`, made on `device` as
/// first needed, then convert the copy into `sample`: for a video
/// processor that refuses the duplication's own surface.
fn convert_copy(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    copied: &mut Option<ID3D11Texture2D>,
    converter: &mut Converter,
    frame: &ID3D11Texture2D,
    sample: &Sample,
) -> Result<()> {
    if copied.is_none() {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: an out parameter the call fills.
        unsafe { frame.GetDesc(&mut desc) };
        desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
        desc.MiscFlags = 0;
        desc.CPUAccessFlags = 0;
        desc.Usage = D3D11_USAGE_DEFAULT;
        let mut texture = None;
        // SAFETY: a description and an out parameter that outlive the call.
        unsafe {
            device
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .map_err(error)?;
        }
        *copied = texture;
    }
    let copied = copied
        .as_ref()
        .ok_or_else(|| error("missing capture texture"))?;
    // SAFETY: two textures of the same size and format on this device.
    unsafe { context.CopyResource(copied, frame) };
    converter.convert(copied, sample)
}

impl PresentSource for DxgiSource {
    type Target = Sample;
    type Picture = Picture;
    const WAIT: Duration = ACQUIRE_WAIT;

    /// Thread C's desktop first, before any COM or D3D call:
    /// `SetThreadDesktop` refuses a thread that has windows or hooks on its
    /// desktop, and a fresh thread has none. Then per-monitor DPI
    /// awareness, which thread C captured under, and the multithreaded
    /// apartment, which Media Foundation's samples live in.
    fn start(&mut self) -> Result<()> {
        join_desktop(self.thread_c)?;
        // SAFETY: a plain call on this thread; the previous context it
        // returns is not needed, as the thread ends with the source.
        let _ = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        // SAFETY: balanced by `Apartment`'s drop, on this thread.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(error)?;
        self.apartment = Some(Apartment);
        Ok(())
    }

    /// Wait at most `wait` for a frame, and release it at once: a frame
    /// held until the next acquire keeps DXGI from folding the next update
    /// into a frame meanwhile. A new picture is converted while DXGI holds
    /// it, dated by its present, and counts the presents DXGI folded into
    /// it.
    fn next(
        &mut self,
        wait: Duration,
        take: impl FnOnce() -> Option<Sample>,
    ) -> Result<Option<Picture>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        let timeout = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
        self.shared.acquires.fetch_add(1, Ordering::Relaxed);
        // SAFETY: out parameters that outlive the call.
        match unsafe {
            self.capture
                .duplication()
                .AcquireNextFrame(timeout, &mut info, &mut resource)
        } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) => return Err(error(e)),
        }
        if self.shared.first_acquire.get().is_none() {
            let acquired = FirstAcquire {
                last_present_time: info.LastPresentTime,
                accumulated_frames: info.AccumulatedFrames,
                resource: resource.is_some(),
            };
            let _ = self.shared.first_acquire.set(acquired);
            tracing::debug!(
                target: "pb_mapper_desktop::session",
                event = "windows_capture_first_acquire",
                last_present_time = acquired.last_present_time,
                accumulated_frames = acquired.accumulated_frames,
                resource = acquired.resource,
            );
        }
        let converted = self.convert_frame(&info, resource, take);
        // SAFETY: the frame acquired above is released once.
        let released = unsafe { self.capture.duplication().ReleaseFrame() }.map_err(error);
        converted.and_then(|converted| released.map(|()| converted))
    }

    /// The display showed `older`, which thread C held back and never
    /// sent, before `newer`: its presents count for `newer`, and its sample
    /// goes back to the pool.
    fn fold(older: Picture, newer: &mut Picture) {
        newer.fresh.presents = newer.fresh.presents.saturating_add(older.fresh.presents);
    }
}

/// Put this thread on the desktop of the thread `thread_c` names, unless
/// it is there already.
fn join_desktop(thread_c: u32) -> Result<()> {
    // SAFETY: plain calls on thread ids; a handle `GetThreadDesktop`
    // returns needs no closing, and thread C keeps its desktop for as long
    // as this thread lives, since it drops the encoder, which joins this
    // thread, before it binds another.
    unsafe {
        let wanted = GetThreadDesktop(thread_c).map_err(error)?;
        let current = GetThreadDesktop(GetCurrentThreadId()).map_err(error)?;
        if wanted != current {
            SetThreadDesktop(wanted).map_err(error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        Win32::{
            Foundation::GENERIC_ALL,
            System::StationsAndDesktops::{CloseDesktop, CreateDesktopW, DESKTOP_CONTROL_FLAGS},
        },
        core::{PCWSTR, w},
    };

    /// The capture thread lands on thread C's desktop: here a desktop of
    /// its own that the test thread, standing in for thread C, is bound
    /// to, as the privileged worker's thread C is bound to the input
    /// desktop. Skipped where this process may not make or enter one.
    #[test]
    fn the_capture_thread_joins_thread_cs_desktop() {
        // SAFETY: plain calls; the desktop is closed below, after both
        // threads have left it.
        let created = unsafe {
            CreateDesktopW(
                w!("pb-mapper-capture-thread-test"),
                PCWSTR::null(),
                None,
                DESKTOP_CONTROL_FLAGS(0),
                GENERIC_ALL.0,
                None,
            )
        };
        let desktop = match created {
            Ok(desktop) => desktop,
            Err(failure) => {
                println!("skipped: no desktop of its own: {failure}");
                return;
            }
        };
        // SAFETY: as above.
        let original = unsafe { GetThreadDesktop(GetCurrentThreadId()) }.unwrap();
        assert_ne!(original, desktop);
        // SAFETY: as above; a test thread has no windows or hooks.
        if let Err(failure) = unsafe { SetThreadDesktop(desktop) } {
            println!("skipped: cannot enter it: {failure}");
            // SAFETY: as above.
            unsafe { CloseDesktop(desktop) }.unwrap();
            return;
        }
        // SAFETY: as above.
        let thread_c = unsafe { GetCurrentThreadId() };
        let joined = std::thread::spawn(move || {
            join_desktop(thread_c)?;
            // SAFETY: as above.
            unsafe { GetThreadDesktop(GetCurrentThreadId()) }
                .map(|joined| joined.0 as isize)
                .map_err(error)
        })
        .join()
        .unwrap();
        // SAFETY: as above.
        unsafe {
            SetThreadDesktop(original).unwrap();
            CloseDesktop(desktop).unwrap();
        }
        assert_eq!(joined.unwrap(), desktop.0 as isize);
        // A thread already on thread C's desktop stays there.
        std::thread::spawn(move || join_desktop(thread_c))
            .join()
            .unwrap()
            .unwrap();
    }
}
