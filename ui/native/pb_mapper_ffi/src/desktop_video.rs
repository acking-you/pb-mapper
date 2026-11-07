//! Desktop video for native consumers. The Windows runner's decoder thread and
//! the Android feeder (through `desktop_jni.rs`) pull encoded units straight
//! from a viewer's [`VideoSink`]; video never passes through Dart.
//!
//! Ids, never pointers, cross the boundary. Sources and consumers share one
//! counter and no id is ever reused, so a late call with a stale id finds
//! nothing and reads as [`CLOSED`]: it can never reach freed memory or a
//! replaced session. Every export is a leaf wrapped in `catch_unwind`. Only
//! `next` blocks, on the sink's `Condvar` with the registry lock released and
//! never on tokio, so it must not run on a UI thread.
//!
//! The return codes are one space, shared with the JNI functions.
use pb_mapper::desktop::{
    AttachError, KeyframeReason, Pull, VideoCodec, VideoConsumer, VideoSink, VideoUnit,
};
use std::{
    collections::HashMap,
    ffi::c_char,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, LazyLock, atomic::AtomicU8},
    time::Duration,
};

/// A unit was copied into the buffer and its metadata filled.
pub(crate) const UNIT: i32 = 1;
pub(crate) const TIMEOUT: i32 = 0;
/// The source or consumer is closed or unknown: the consumer must exit.
pub(crate) const CLOSED: i32 = -1;
/// `len` holds the size needed; the unit stays queued for a larger buffer.
pub(crate) const NEED_CAPACITY: i32 = -2;
/// The viewer is paused: suspend stall checks and call again.
pub(crate) const PAUSED: i32 = -3;
/// The id names a live source, not one of its attached consumers.
pub(crate) const NOT_ATTACHED: i32 = -4;
/// A bad argument, or a caught panic.
pub(crate) const INVALID: i32 = -5;
pub(crate) const ALREADY_ATTACHED: i32 = -6;

/// The longest `next` waits, whatever the caller asks for.
const MAX_WAIT: Duration = Duration::from_secs(1);
/// The longest decoder name `report` reads, in bytes.
const MAX_DECODER_NAME: usize = 64;

const FLAG_KEYFRAME: u32 = 1;
const FLAG_DISCONTINUITY: u32 = 1 << 1;
const FLAG_JPEG: u32 = 1 << 2;
const FLAG_DUPLICATE: u32 = 1 << 3;

/// What `next` says about the unit it copied.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PbDesktopVideoMetaV1 {
    pub generation: u64,
    pub frame_id: u64,
    /// The transport sequence, for trace correlation.
    pub seq: u64,
    pub pts_us: u64,
    /// Time since the unit was received, measured at the pop, so no clock
    /// crosses languages.
    pub age_us: u64,
    pub width: u32,
    pub height: u32,
    /// Bytes written, or on `NEED_CAPACITY` the bytes needed.
    pub len: u32,
    /// Bit 0 keyframe, 1 discontinuity, 2 JPEG (else H.264), 3 duplicate.
    pub flags: u32,
}
const _: () = assert!(std::mem::size_of::<PbDesktopVideoMetaV1>() == 56);

impl PbDesktopVideoMetaV1 {
    pub(crate) fn of(unit: &VideoUnit) -> Self {
        let flag = |set: bool, flag: u32| if set { flag } else { 0 };
        Self {
            generation: unit.generation,
            frame_id: unit.frame_id,
            seq: unit.seq,
            pts_us: unit.pts_us,
            age_us: u64::try_from(unit.received_at.elapsed().as_micros()).unwrap_or(u64::MAX),
            width: unit.width,
            height: unit.height,
            len: u32::try_from(unit.bytes.len()).unwrap_or(u32::MAX),
            flags: flag(unit.keyframe, FLAG_KEYFRAME)
                | flag(unit.discontinuity, FLAG_DISCONTINUITY)
                | flag(unit.codec == VideoCodec::Jpeg, FLAG_JPEG)
                | flag(unit.duplicate, FLAG_DUPLICATE),
        }
    }

    fn needing(len: usize) -> Self {
        Self {
            len: u32::try_from(len).unwrap_or(u32::MAX),
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct Registry {
    next_id: u64,
    sources: HashMap<u64, Arc<VideoSink>>,
    consumers: HashMap<u64, (u64, Arc<VideoConsumer>)>,
    #[cfg(feature = "desktop-test-source")]
    players: HashMap<u64, test_source::Player>,
}
impl Registry {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn consumer(&self, id: u64) -> Result<Arc<VideoConsumer>, i32> {
        match self.consumers.get(&id) {
            Some((_, consumer)) => Ok(consumer.clone()),
            None if self.sources.contains_key(&id) => Err(NOT_ATTACHED),
            None => Err(CLOSED),
        }
    }
}

static REGISTRY: LazyLock<parking_lot::Mutex<Registry>> = LazyLock::new(Default::default);

/// Publishes a sink to native consumers under a new id.
#[cfg_attr(
    not(any(test, feature = "desktop-test-source")),
    allow(dead_code, reason = "no viewer registers its sink yet")
)]
pub(crate) fn add_source(sink: Arc<VideoSink>) -> u64 {
    let mut registry = REGISTRY.lock();
    let id = registry.id();
    registry.sources.insert(id, sink);
    id
}

/// Retires a source and closes its sink: a `next` blocked on it returns
/// [`CLOSED`] at once, and so does every later call naming the source or one
/// of its consumers. Unknown ids are ignored.
#[cfg_attr(
    not(any(test, feature = "desktop-test-source")),
    allow(dead_code, reason = "no viewer registers its sink yet")
)]
pub(crate) fn remove_source(id: u64) {
    let (sink, consumers) = {
        let mut registry = REGISTRY.lock();
        let consumers: Vec<_> = registry
            .consumers
            .extract_if(|_, (source, _)| *source == id)
            .collect();
        (registry.sources.remove(&id), consumers)
    };
    // Outside the lock: the last reference may free queued video.
    if let Some(sink) = sink {
        sink.close();
    }
    drop(consumers);
}

pub(crate) fn attach(source: u64) -> i64 {
    let mut registry = REGISTRY.lock();
    let Some(sink) = registry.sources.get(&source).cloned() else {
        return CLOSED.into();
    };
    match sink.attach() {
        Ok(consumer) => {
            let id = registry.id();
            let Ok(handle) = i64::try_from(id) else {
                return INVALID.into();
            };
            registry.consumers.insert(id, (source, Arc::new(consumer)));
            handle
        }
        Err(AttachError::Closed) => CLOSED.into(),
        Err(AttachError::AlreadyAttached) => ALREADY_ATTACHED.into(),
    }
}

/// Waits up to `timeout_ms` (at most a second) for the next unit no longer
/// than `capacity`. A longer head stays queued behind `NeedCapacity`.
pub(crate) fn next(consumer: u64, timeout_ms: u32, capacity: usize) -> Result<Pull, i32> {
    // The lock is released before the wait: only the consumer's Arc is held.
    let consumer = REGISTRY.lock().consumer(consumer)?;
    let timeout = Duration::from_millis(timeout_ms.into()).min(MAX_WAIT);
    Ok(consumer.next(timeout, capacity))
}

/// The return code of a pull that carries no unit.
pub(crate) fn code(pull: &Pull) -> i32 {
    match pull {
        Pull::Unit(_) => UNIT,
        Pull::NeedCapacity(_) => NEED_CAPACITY,
        Pull::Timeout => TIMEOUT,
        Pull::Paused => PAUSED,
        Pull::Closed => CLOSED,
    }
}

/// Runs `body` on the consumer with the registry lock already released.
fn with_consumer(id: u64, body: impl FnOnce(&VideoConsumer) -> i32) -> i32 {
    let consumer = REGISTRY.lock().consumer(id);
    consumer.map_or_else(|code| code, |consumer| body(&consumer))
}

pub(crate) fn presented(consumer: u64, generation: u64, frame_id: u64) -> i32 {
    with_consumer(consumer, |consumer| {
        consumer.presented(generation, frame_id).into()
    })
}

pub(crate) fn keyframe(consumer: u64, reason: u32) -> i32 {
    let reason = match reason {
        0 => KeyframeReason::DecoderError,
        1 => KeyframeReason::Gap,
        2 => KeyframeReason::Backlog,
        3 => KeyframeReason::Stall,
        4 => KeyframeReason::User,
        _ => return INVALID,
    };
    with_consumer(consumer, |consumer| consumer.keyframe(reason).into())
}

pub(crate) fn report(consumer: u64, decoder: &str, hardware: bool) -> i32 {
    with_consumer(consumer, |consumer| {
        consumer.report(decoder, hardware);
        0
    })
}

pub(crate) fn detach(consumer: u64) {
    let removed = REGISTRY.lock().consumers.remove(&consumer);
    // Wakes a `next` blocked on another thread, which still holds a reference.
    if let Some((_, consumer)) = removed {
        consumer.detach();
    }
}

/// The address of a static. A second copy of this library has another one,
/// which is how native code proves it reached the copy Dart loaded.
pub(crate) fn instance_token() -> u64 {
    static INSTANCE: AtomicU8 = AtomicU8::new(0);
    std::ptr::from_ref(&INSTANCE).addr() as u64
}

/// Runs an export's body; a panic becomes `fallback` instead of unwinding
/// into C or the JVM.
pub(crate) fn guard<T>(fallback: T, body: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or(fallback)
}

/// `> 0`: the consumer id. Otherwise `CLOSED` or `ALREADY_ATTACHED`: a source
/// has exactly one consumer.
#[unsafe(no_mangle)]
pub extern "C" fn pb_mapper_desktop_video_attach_v1(source: u64) -> i64 {
    guard(INVALID.into(), || attach(source))
}

/// Copies the next unit into `dst` and fills `meta`. Blocks up to
/// `timeout_ms`; never call it on a UI thread.
///
/// # Safety
/// `dst` must be valid for `capacity` bytes of writes (it may be null only
/// when `capacity` is 0), and `meta` must point to a writable
/// `PbDesktopVideoMetaV1`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_video_next_v1(
    consumer: u64,
    timeout_ms: u32,
    dst: *mut u8,
    capacity: u32,
    meta: *mut PbDesktopVideoMetaV1,
) -> i32 {
    guard(INVALID, || {
        if meta.is_null() || (dst.is_null() && capacity != 0) {
            return INVALID;
        }
        let (code, value) = match next(consumer, timeout_ms, capacity as usize) {
            Ok(Pull::Unit(unit)) => {
                if !unit.bytes.is_empty() {
                    // SAFETY: `next` only returns a unit of at most `capacity`
                    // bytes, and the caller provides that many at `dst`.
                    unsafe {
                        std::ptr::copy_nonoverlapping(unit.bytes.as_ptr(), dst, unit.bytes.len());
                    }
                }
                (UNIT, PbDesktopVideoMetaV1::of(&unit))
            }
            Ok(Pull::NeedCapacity(len)) => (NEED_CAPACITY, PbDesktopVideoMetaV1::needing(len)),
            Ok(pull) => return code(&pull),
            Err(code) => return code,
        };
        // SAFETY: checked non-null; the caller provides a writable struct.
        unsafe { meta.write(value) };
        code
    })
}

/// The consumer showed `(generation, frame_id)`. 1 accepted, 0 rejected, or
/// `CLOSED`. Non-blocking.
#[unsafe(no_mangle)]
pub extern "C" fn pb_mapper_desktop_video_presented_v1(
    consumer: u64,
    generation: u64,
    frame_id: u64,
) -> i32 {
    guard(INVALID, || presented(consumer, generation, frame_id))
}

/// The decoder needs a keyframe: 0 decoder error, 1 gap, 2 backlog, 3 stall,
/// 4 user. 1 queued, 0 limited or paused, or `CLOSED`. Non-blocking.
#[unsafe(no_mangle)]
pub extern "C" fn pb_mapper_desktop_video_keyframe_v1(consumer: u64, reason: u32) -> i32 {
    guard(INVALID, || keyframe(consumer, reason))
}

/// Names the decoder in use, for the overlay; bit 0 of `flags` is hardware.
/// 0, or `CLOSED`.
///
/// # Safety
/// `decoder_utf8` must be null or a NUL-terminated string; at most 64 bytes
/// before the NUL are read.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_video_report_v1(
    consumer: u64,
    decoder_utf8: *const c_char,
    flags: u32,
) -> i32 {
    guard(INVALID, || {
        if decoder_utf8.is_null() {
            return INVALID;
        }
        let name = decoder_utf8.cast::<u8>();
        // SAFETY: the string is NUL-terminated, and the scan stops at the NUL
        // or after the longest name, whichever comes first.
        let Some(len) = (0..=MAX_DECODER_NAME).find(|&i| unsafe { *name.add(i) } == 0) else {
            return INVALID;
        };
        // SAFETY: the `len` bytes before the NUL were just read.
        let Ok(name) = std::str::from_utf8(unsafe { std::slice::from_raw_parts(name, len) }) else {
            return INVALID;
        };
        report(consumer, name, flags & 1 != 0)
    })
}

/// Idempotent. Wakes a `next` blocked on this consumer, which returns
/// `CLOSED`; the source then takes a new consumer.
#[unsafe(no_mangle)]
pub extern "C" fn pb_mapper_desktop_video_detach_v1(consumer: u64) {
    guard((), || detach(consumer));
}

/// The same value from every caller that reached this copy of the library.
#[unsafe(no_mangle)]
pub extern "C" fn pb_mapper_desktop_instance_token_v1() -> u64 {
    instance_token()
}

/// Plays a `video_bench --dump` clip (`u32` big-endian length, then an Annex B
/// access unit, repeated) into a new source, looped at `fps` (1..=120) until
/// stopped: generation 1, frame ids 1, 2, 3 …, a keyframe wherever a unit
/// holds an IDR slice, the size from the latest SPS. The clip's first unit
/// must be a keyframe with its SPS; every loop, and every resume, restarts at
/// it as a discontinuity. Returns the source id, or 0 for a bad path, clip or
/// `fps`.
///
/// # Safety
/// `path_utf8` must be null or a NUL-terminated string.
#[cfg(feature = "desktop-test-source")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_video_test_source_v1(
    path_utf8: *const c_char,
    fps: u32,
) -> u64 {
    guard(0, || {
        if path_utf8.is_null() || !(1..=120).contains(&fps) {
            return 0;
        }
        // SAFETY: checked non-null; the caller passes a NUL-terminated string.
        let Ok(path) = unsafe { std::ffi::CStr::from_ptr(path_utf8) }.to_str() else {
            return 0;
        };
        let Some(clip) = test_source::load(std::path::Path::new(path)) else {
            return 0;
        };
        let sink = VideoSink::detached_for_test();
        let Some(player) = test_source::Player::start(sink.clone(), clip, fps) else {
            return 0;
        };
        let id = add_source(sink);
        REGISTRY.lock().players.insert(id, player);
        id
    })
}

/// Pauses (`paused != 0`) or resumes a test source, so a consumer's `PAUSED`
/// handling runs too. 0, or `CLOSED` when `source` is not a running test
/// source.
#[cfg(feature = "desktop-test-source")]
#[unsafe(no_mangle)]
pub extern "C" fn pb_mapper_desktop_video_test_source_pause_v1(source: u64, paused: u32) -> i32 {
    guard(INVALID, || {
        let registry = REGISTRY.lock();
        match (registry.players.get(&source), registry.sources.get(&source)) {
            (Some(player), Some(sink)) => {
                player.pause(paused != 0);
                sink.set_paused_for_test(paused != 0);
                0
            }
            _ => CLOSED,
        }
    })
}

/// Stops a test source and closes its sink. Idempotent; ignores any id that
/// is not a test source.
#[cfg(feature = "desktop-test-source")]
#[unsafe(no_mangle)]
pub extern "C" fn pb_mapper_desktop_video_test_source_stop_v1(source: u64) {
    guard((), || {
        let player = REGISTRY.lock().players.remove(&source);
        if let Some(player) = player {
            remove_source(source);
            player.stop();
        }
    });
}

#[cfg(feature = "desktop-test-source")]
mod test_source {
    //! The clip player behind `pb_mapper_desktop_video_test_source_v1`.
    use bytes::Bytes;
    use pb_mapper::desktop::{VideoCodec, VideoSink, VideoUnit};
    use std::{
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread::JoinHandle,
        time::{Duration, Instant},
    };

    const MAX_CLIP: u64 = 256 * 1024 * 1024;
    /// The wire's largest payload.
    const MAX_UNIT: usize = 4 * 1024 * 1024;

    pub(super) struct Unit {
        bytes: Bytes,
        keyframe: bool,
        width: u32,
        height: u32,
    }

    pub(super) struct Player {
        /// Dropping it ends the loop at once.
        stop: mpsc::Sender<()>,
        paused: Arc<AtomicBool>,
        thread: JoinHandle<()>,
    }
    impl Player {
        pub(super) fn start(sink: Arc<VideoSink>, clip: Vec<Unit>, fps: u32) -> Option<Self> {
            let (stop, stopped) = mpsc::channel();
            let paused = Arc::new(AtomicBool::new(false));
            let playing = paused.clone();
            let thread = std::thread::Builder::new()
                .name("pb-desktop-test-source".into())
                .spawn(move || play(&sink, &clip, fps, &playing, &stopped))
                .ok()?;
            Some(Self {
                stop,
                paused,
                thread,
            })
        }

        pub(super) fn pause(&self, paused: bool) {
            self.paused.store(paused, Ordering::Release);
        }

        pub(super) fn stop(self) {
            drop(self.stop);
            let _ = self.thread.join();
        }
    }

    /// Plays like a host: nothing while the viewer is paused, then the clip
    /// again from its first IDR.
    fn play(
        sink: &VideoSink,
        clip: &[Unit],
        fps: u32,
        paused: &AtomicBool,
        stopped: &mpsc::Receiver<()>,
    ) {
        let interval = Duration::from_secs(1) / fps;
        let started = Instant::now();
        let mut deadline = started;
        let mut frame_id = 0;
        let mut position = 0;
        loop {
            if paused.load(Ordering::Acquire) {
                position = 0;
            } else if let Some(unit) = clip.get(position) {
                frame_id += 1;
                sink.push_for_test(VideoUnit {
                    seq: frame_id,
                    generation: 1,
                    frame_id,
                    codec: VideoCodec::H264,
                    width: unit.width,
                    height: unit.height,
                    keyframe: unit.keyframe,
                    duplicate: false,
                    // The first IDR, again after a loop or a pause, as after
                    // a drop.
                    discontinuity: position == 0,
                    pts_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                    bytes: unit.bytes.clone(),
                    received_at: Instant::now(),
                });
                position = (position + 1) % clip.len();
            }
            deadline += interval;
            let now = Instant::now();
            // Behind schedule (a suspended device), carry on without a burst.
            deadline = deadline.max(now);
            if !matches!(
                stopped.recv_timeout(deadline - now),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                return;
            }
        }
    }

    pub(super) fn load(path: &Path) -> Option<Vec<Unit>> {
        if std::fs::metadata(path).ok()?.len() > MAX_CLIP {
            return None;
        }
        let data = Bytes::from(std::fs::read(path).ok()?);
        let mut units = Vec::new();
        let mut size = None;
        let mut offset = 0;
        while offset < data.len() {
            let len = data.get(offset..offset + 4)?;
            let len = u32::from_be_bytes(len.try_into().ok()?) as usize;
            let start = offset + 4;
            let end = start.checked_add(len)?;
            if len == 0 || len > MAX_UNIT || end > data.len() {
                return None;
            }
            let bytes = data.slice(start..end);
            let nals = nal_units(&bytes);
            if nals.is_empty() {
                return None;
            }
            let mut keyframe = false;
            for nal in nals {
                match nal[0] & 0x1f {
                    5 => keyframe = true,
                    7 => size = Some(sps_size(nal)?),
                    _ => {}
                }
            }
            if units.is_empty() && !keyframe {
                return None;
            }
            let (width, height) = size?;
            units.push(Unit {
                bytes,
                keyframe,
                width,
                height,
            });
            offset = end;
        }
        (!units.is_empty()).then_some(units)
    }

    /// The NAL units of an Annex B access unit, without their start codes.
    fn nal_units(bytes: &[u8]) -> Vec<&[u8]> {
        let mut starts = Vec::new();
        let mut position = 0;
        while position + 3 <= bytes.len() {
            if bytes[position..position + 3] == [0, 0, 1] {
                position += 3;
                starts.push(position);
            } else {
                position += 1;
            }
        }
        starts
            .iter()
            .enumerate()
            .map(|(index, &start)| {
                let end = starts.get(index + 1).map_or(bytes.len(), |&next| next - 3);
                &bytes[start..end.max(start)]
            })
            .filter(|nal| !nal.is_empty())
            .collect()
    }

    /// The picture size an H.264 SPS declares, after its cropping.
    pub(super) fn sps_size(nal: &[u8]) -> Option<(u32, u32)> {
        let rbsp = unescape(nal.get(1..)?);
        let mut bits = Bits {
            data: &rbsp,
            position: 0,
        };
        let profile = bits.read(8)?;
        bits.read(16)?; // constraint flags and level
        bits.ue()?; // seq_parameter_set_id
        let mut chroma = 1;
        let mut separate_planes = false;
        if matches!(
            profile,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            chroma = bits.ue()?;
            if chroma == 3 {
                separate_planes = bits.bit()? == 1;
            }
            bits.ue()?; // bit_depth_luma_minus8
            bits.ue()?; // bit_depth_chroma_minus8
            bits.bit()?; // qpprime_y_zero_transform_bypass_flag
            if bits.bit()? == 1 {
                for list in 0..if chroma == 3 { 12 } else { 8 } {
                    if bits.bit()? == 1 {
                        bits.skip_scaling_list(if list < 6 { 16 } else { 64 })?;
                    }
                }
            }
        }
        bits.ue()?; // log2_max_frame_num_minus4
        match bits.ue()? {
            0 => {
                bits.ue()?; // log2_max_pic_order_cnt_lsb_minus4
            }
            1 => {
                bits.bit()?;
                bits.se()?;
                bits.se()?;
                for _ in 0..bits.ue()?.min(255) {
                    bits.se()?;
                }
            }
            _ => {}
        }
        bits.ue()?; // max_num_ref_frames
        bits.bit()?; // gaps_in_frame_num_value_allowed_flag
        let width_mbs = bits.ue()?.checked_add(1)?;
        let height_units = bits.ue()?.checked_add(1)?;
        let frame_mbs_only = bits.bit()?;
        if frame_mbs_only == 0 {
            bits.bit()?; // mb_adaptive_frame_field_flag
        }
        bits.bit()?; // direct_8x8_inference_flag
        let mut crop = [0; 4];
        if bits.bit()? == 1 {
            for edge in &mut crop {
                *edge = bits.ue()?;
            }
        }
        let [left, right, top, bottom] = crop;
        let fields = 2 - frame_mbs_only;
        let (unit_x, unit_y) = if chroma == 0 || separate_planes {
            (1, fields)
        } else {
            (
                if chroma == 3 { 1 } else { 2 },
                if chroma == 1 { 2 } else { 1 } * fields,
            )
        };
        let width = width_mbs
            .checked_mul(16)?
            .checked_sub(left.checked_add(right)?.checked_mul(unit_x)?)?;
        let height = height_units
            .checked_mul(16 * fields)?
            .checked_sub(top.checked_add(bottom)?.checked_mul(unit_y)?)?;
        (width > 0 && height > 0).then_some((width, height))
    }

    /// Removes the emulation prevention byte from each `00 00 03`.
    fn unescape(bytes: &[u8]) -> Vec<u8> {
        let mut rbsp = Vec::with_capacity(bytes.len());
        let mut zeros = 0;
        for &byte in bytes {
            if zeros >= 2 && byte == 3 {
                zeros = 0;
                continue;
            }
            zeros = if byte == 0 { zeros + 1 } else { 0 };
            rbsp.push(byte);
        }
        rbsp
    }

    struct Bits<'a> {
        data: &'a [u8],
        position: usize,
    }
    impl Bits<'_> {
        fn bit(&mut self) -> Option<u32> {
            let byte = *self.data.get(self.position / 8)?;
            let bit = byte >> (7 - self.position % 8) & 1;
            self.position += 1;
            Some(bit.into())
        }

        fn read(&mut self, count: u32) -> Option<u32> {
            (0..count).try_fold(0, |value, _| Some(value << 1 | self.bit()?))
        }

        /// Exp-Golomb, unsigned.
        fn ue(&mut self) -> Option<u32> {
            let mut zeros = 0;
            while self.bit()? == 0 {
                zeros += 1;
                if zeros > 31 {
                    return None;
                }
            }
            ((1u32 << zeros) - 1).checked_add(self.read(zeros)?)
        }

        /// Exp-Golomb, signed.
        fn se(&mut self) -> Option<i64> {
            let code = i64::from(self.ue()?);
            Some(if code % 2 == 1 {
                (code + 1) / 2
            } else {
                -(code / 2)
            })
        }

        fn skip_scaling_list(&mut self, size: usize) -> Option<()> {
            let (mut last, mut next) = (8, 8);
            for _ in 0..size {
                if next != 0 {
                    next = (last + self.se()?).rem_euclid(256);
                }
                if next != 0 {
                    last = next;
                }
            }
            Some(())
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn unit(frame_id: u64, len: usize) -> VideoUnit {
        VideoUnit {
            seq: frame_id,
            generation: 1,
            frame_id,
            codec: VideoCodec::H264,
            width: 16,
            height: 16,
            keyframe: true,
            duplicate: false,
            discontinuity: true,
            pts_us: 0,
            bytes: bytes::Bytes::from(vec![frame_id as u8; len]),
            received_at: Instant::now(),
        }
    }

    /// `next` on another thread, with how long it blocked after `trigger` ran.
    fn blocked_next(consumer: u64, trigger: impl FnOnce()) -> (i32, Duration) {
        let pulling = std::thread::spawn(move || {
            let mut meta = PbDesktopVideoMetaV1::default();
            let mut buffer = [0u8; 16];
            let code = unsafe {
                pb_mapper_desktop_video_next_v1(consumer, 1000, buffer.as_mut_ptr(), 16, &mut meta)
            };
            (code, Instant::now())
        });
        std::thread::sleep(Duration::from_millis(50));
        let triggered = Instant::now();
        trigger();
        let (code, returned) = pulling.join().unwrap();
        (code, returned.saturating_duration_since(triggered))
    }

    #[test]
    fn video_consumer_is_exclusive_and_stale_ids_are_harmless() {
        let sink = VideoSink::detached_for_test();
        let source = add_source(sink.clone());
        let consumer = pb_mapper_desktop_video_attach_v1(source);
        assert!(consumer > 0);
        assert_eq!(
            pb_mapper_desktop_video_attach_v1(source),
            i64::from(ALREADY_ATTACHED)
        );
        let consumer = consumer as u64;
        let mut meta = PbDesktopVideoMetaV1::default();
        // A source id is not a consumer.
        assert_eq!(
            unsafe {
                pb_mapper_desktop_video_next_v1(source, 0, std::ptr::null_mut(), 0, &mut meta)
            },
            NOT_ATTACHED
        );

        // A unit larger than the buffer stays queued until a larger one comes.
        sink.push_for_test(unit(1, 100));
        let mut small = [0u8; 10];
        assert_eq!(
            unsafe {
                pb_mapper_desktop_video_next_v1(consumer, 0, small.as_mut_ptr(), 10, &mut meta)
            },
            NEED_CAPACITY
        );
        assert_eq!(meta.len, 100);
        let mut buffer = vec![0u8; 100];
        assert_eq!(
            unsafe {
                pb_mapper_desktop_video_next_v1(consumer, 0, buffer.as_mut_ptr(), 100, &mut meta)
            },
            UNIT
        );
        assert_eq!((meta.generation, meta.frame_id, meta.len), (1, 1, 100));
        assert_eq!(meta.flags, FLAG_KEYFRAME | FLAG_DISCONTINUITY);
        assert!(buffer.iter().all(|&byte| byte == 1));
        assert_eq!(pb_mapper_desktop_video_keyframe_v1(consumer, 9), INVALID);
        assert_eq!(
            unsafe {
                pb_mapper_desktop_video_next_v1(
                    consumer,
                    0,
                    buffer.as_mut_ptr(),
                    100,
                    std::ptr::null_mut(),
                )
            },
            INVALID
        );

        // Detaching wakes a blocked pull, and the source takes a new consumer.
        let (code, waited) = blocked_next(consumer, || pb_mapper_desktop_video_detach_v1(consumer));
        assert_eq!(code, CLOSED);
        assert!(waited < Duration::from_millis(100), "{waited:?}");
        pb_mapper_desktop_video_detach_v1(consumer);
        let consumer = pb_mapper_desktop_video_attach_v1(source);
        assert!(consumer > 0);
        let consumer = consumer as u64;

        // Disconnecting closes the source under a blocked pull.
        let (code, waited) = blocked_next(consumer, || remove_source(source));
        assert_eq!(code, CLOSED);
        assert!(waited < Duration::from_millis(100), "{waited:?}");

        // Every id of the closed source now reads CLOSED, and nothing panics.
        let name = c"decoder";
        assert_eq!(pb_mapper_desktop_video_attach_v1(source), i64::from(CLOSED));
        assert_eq!(
            unsafe {
                pb_mapper_desktop_video_next_v1(consumer, 0, buffer.as_mut_ptr(), 100, &mut meta)
            },
            CLOSED
        );
        assert_eq!(pb_mapper_desktop_video_presented_v1(consumer, 1, 1), CLOSED);
        assert_eq!(pb_mapper_desktop_video_keyframe_v1(consumer, 1), CLOSED);
        assert_eq!(
            unsafe { pb_mapper_desktop_video_report_v1(consumer, name.as_ptr(), 1) },
            CLOSED
        );
        pb_mapper_desktop_video_detach_v1(consumer);
        pb_mapper_desktop_video_detach_v1(consumer);
        remove_source(source);
        assert_ne!(pb_mapper_desktop_instance_token_v1(), 0);
    }

    #[cfg(feature = "desktop-test-source")]
    #[test]
    fn test_source_loops_a_dumped_clip() {
        // Baseline 1920x1088 macroblocks, cropped by 8 rows to 1080.
        const SPS: [u8; 10] = [0x67, 0x42, 0xc0, 0x28, 0xda, 0x01, 0xe0, 0x08, 0x9f, 0x95];
        let record = |nals: &[&[u8]]| {
            let unit: Vec<u8> = nals
                .iter()
                .flat_map(|nal| [&[0, 0, 0, 1][..], nal].concat())
                .collect();
            [&(unit.len() as u32).to_be_bytes()[..], &unit].concat()
        };
        let keyframe = record(&[&SPS, &[0x68, 0xce, 0x3c, 0x80], &[0x65, 0x88, 0x84]]);
        let predicted = record(&[&[0x41, 0x9a, 0x02]]);
        let directory = std::env::temp_dir().join(format!("pb-video-clip-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let start = |name: &str, clip: &[u8], fps: u32| {
            let path = directory.join(name);
            std::fs::write(&path, clip).unwrap();
            let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
            unsafe { pb_mapper_desktop_video_test_source_v1(path.as_ptr(), fps) }
        };
        assert_eq!(start("bad-fps.h264", &keyframe, 0), 0);
        assert_eq!(start("no-keyframe.h264", &predicted, 30), 0);
        assert_eq!(
            start("truncated.h264", &keyframe[..keyframe.len() - 1], 30),
            0
        );

        let source = start("clip.h264", &[keyframe, predicted].concat(), 120);
        assert!(source > 0);
        let consumer = pb_mapper_desktop_video_attach_v1(source) as u64;
        let mut meta = PbDesktopVideoMetaV1::default();
        let mut buffer = vec![0u8; 64];
        let mut pull = |timeout_ms| {
            let code = unsafe {
                pb_mapper_desktop_video_next_v1(
                    consumer,
                    timeout_ms,
                    buffer.as_mut_ptr(),
                    64,
                    &mut meta,
                )
            };
            (code, meta)
        };
        let pulled: Vec<_> = (0..3).map(|_| pull(1000)).collect();
        for (index, (code, meta)) in pulled.iter().enumerate() {
            assert_eq!(*code, UNIT);
            assert_eq!((meta.generation, meta.frame_id), (1, index as u64 + 1));
            assert_eq!((meta.width, meta.height), (1920, 1080));
        }
        // The loop restarts at the first IDR, as a discontinuity.
        let keyframes: Vec<_> = pulled.iter().map(|(_, meta)| meta.flags).collect();
        let both = FLAG_KEYFRAME | FLAG_DISCONTINUITY;
        assert_eq!(keyframes, [both, 0, both]);

        assert_eq!(pb_mapper_desktop_video_test_source_pause_v1(source, 1), 0);
        assert_eq!(pull(20).0, PAUSED);
        assert_eq!(pb_mapper_desktop_video_test_source_pause_v1(source, 0), 0);
        pb_mapper_desktop_video_test_source_stop_v1(source);
        pb_mapper_desktop_video_test_source_stop_v1(source);
        assert_eq!(pull(0).0, CLOSED);
        assert_eq!(
            pb_mapper_desktop_video_test_source_pause_v1(source, 1),
            CLOSED
        );
        pb_mapper_desktop_video_detach_v1(consumer);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
