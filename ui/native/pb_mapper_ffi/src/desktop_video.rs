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
/// The longest failure text `failed` reads, in bytes.
const MAX_FAILURE_MESSAGE: usize = 512;

const FLAG_KEYFRAME: u32 = 1;
const FLAG_DISCONTINUITY: u32 = 1 << 1;
const FLAG_JPEG: u32 = 1 << 2;
const FLAG_DUPLICATE: u32 = 1 << 3;
const FLAG_H265: u32 = 1 << 4;

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
    /// Bit 0 keyframe, 1 discontinuity, 2 JPEG, 3 duplicate, 4 H.265. The
    /// codec is bit 2 or bit 4; a unit with neither is H.264.
    pub flags: u32,
}
// The layout of perf 7.5, field by field, as the Windows runner asserts it
// too; desktop_ffi_test.dart pins Dart's mirror to the same offsets.
const _: () = {
    use std::mem::offset_of;
    type M = PbDesktopVideoMetaV1;
    assert!(size_of::<M>() == 56 && align_of::<M>() == 8);
    assert!(offset_of!(M, generation) == 0);
    assert!(offset_of!(M, frame_id) == 8);
    assert!(offset_of!(M, seq) == 16);
    assert!(offset_of!(M, pts_us) == 24);
    assert!(offset_of!(M, age_us) == 32);
    assert!(offset_of!(M, width) == 40);
    assert!(offset_of!(M, height) == 44);
    assert!(offset_of!(M, len) == 48);
    assert!(offset_of!(M, flags) == 52);
};

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
                | flag(unit.duplicate, FLAG_DUPLICATE)
                | flag(unit.codec == VideoCodec::H265, FLAG_H265),
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
pub(crate) fn add_source(sink: Arc<VideoSink>) -> u64 {
    let mut registry = REGISTRY.lock();
    let id = registry.id();
    registry.sources.insert(id, sink);
    id
}

/// Retires a source and closes its sink: a `next` blocked on it returns
/// [`CLOSED`] at once, and so does every later call naming the source or one
/// of its consumers. Unknown ids are ignored.
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
fn next(consumer: u64, timeout_ms: u32, capacity: usize) -> Result<Pull, i32> {
    // The lock is released before the wait: only the consumer's Arc is held.
    let consumer = REGISTRY.lock().consumer(consumer)?;
    let timeout = Duration::from_millis(timeout_ms.into()).min(MAX_WAIT);
    Ok(consumer.next(timeout, capacity))
}

/// What a pull left the caller, for the C and the JNI `next` alike.
pub(crate) enum Pulled {
    /// A unit was copied into the buffer: its metadata, its age measured at
    /// the pop.
    Unit(PbDesktopVideoMetaV1),
    /// The head unit needs this many bytes; it stays queued.
    NeedCapacity(usize),
    /// No unit: the return code.
    Code(i32),
}

/// `next`, with the unit it pulls copied to `dst`.
///
/// # Safety
/// `dst` must be valid for `capacity` bytes of writes; it may be null only
/// when `capacity` is 0.
pub(crate) unsafe fn pull_into(
    consumer: u64,
    timeout_ms: u32,
    dst: *mut u8,
    capacity: usize,
) -> Pulled {
    match next(consumer, timeout_ms, capacity) {
        Ok(Pull::Unit(unit)) => {
            if !unit.bytes.is_empty() {
                // SAFETY: `next` only returns a unit of at most `capacity`
                // bytes, and the caller provides that many at `dst`.
                unsafe {
                    std::ptr::copy_nonoverlapping(unit.bytes.as_ptr(), dst, unit.bytes.len());
                }
            }
            Pulled::Unit(PbDesktopVideoMetaV1::of(&unit))
        }
        Ok(Pull::NeedCapacity(len)) => Pulled::NeedCapacity(len),
        Ok(Pull::Timeout) => Pulled::Code(TIMEOUT),
        Ok(Pull::Paused) => Pulled::Code(PAUSED),
        Ok(Pull::Closed) => Pulled::Code(CLOSED),
        Err(code) => Pulled::Code(code),
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
        1 => KeyframeReason::Gap,
        2 => KeyframeReason::Backlog,
        3 => KeyframeReason::Stall,
        4 => KeyframeReason::User,
        _ => return INVALID,
    };
    with_consumer(consumer, |consumer| consumer.keyframe(reason).into())
}

pub(crate) fn failed(consumer: u64, message: &str) -> i32 {
    with_consumer(consumer, |consumer| consumer.failed(message).into())
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
        // SAFETY: the caller provides `capacity` bytes at `dst`, which is
        // null only when `capacity` is 0, as just checked.
        let pulled = unsafe { pull_into(consumer, timeout_ms, dst, capacity as usize) };
        let (code, value) = match pulled {
            Pulled::Unit(value) => (UNIT, value),
            Pulled::NeedCapacity(len) => (NEED_CAPACITY, PbDesktopVideoMetaV1::needing(len)),
            Pulled::Code(code) => return code,
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

/// The decoder needs a keyframe: 1 gap, 2 backlog, 3 stall, 4 user; a decoder
/// error goes through `failed`. 1 queued, 0 limited or paused, or `CLOSED`.
/// Non-blocking.
#[unsafe(no_mangle)]
pub extern "C" fn pb_mapper_desktop_video_keyframe_v1(consumer: u64, reason: u32) -> i32 {
    guard(INVALID, || keyframe(consumer, reason))
}

/// The decoder failed, and why: it asks for a keyframe, counts toward giving
/// up on the video, and the text is logged and ends the session's message.
/// 1 queued, 0 limited or paused, or `CLOSED`. Non-blocking.
///
/// # Safety
/// `message_utf8` must be null or a NUL-terminated string; at most 512 bytes
/// before the NUL are read. Null or invalid UTF-8 still counts, so a failure
/// is never lost to its text.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pb_mapper_desktop_video_failed_v1(
    consumer: u64,
    message_utf8: *const c_char,
) -> i32 {
    guard(INVALID, || {
        let message: &[u8] = if message_utf8.is_null() {
            &[]
        } else {
            let text = message_utf8.cast::<u8>();
            // SAFETY: the string is NUL-terminated, and the scan stops at the
            // NUL or after the longest message, whichever comes first.
            let len = (0..MAX_FAILURE_MESSAGE)
                .find(|&i| unsafe { *text.add(i) } == 0)
                .unwrap_or(MAX_FAILURE_MESSAGE);
            // SAFETY: the `len` bytes at `text` were just read.
            unsafe { std::slice::from_raw_parts(text, len) }
        };
        failed(consumer, &String::from_utf8_lossy(message))
    })
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
/// holds an IDR slice (an IRAP picture in H.265), the size from the latest
/// SPS. The clip is H.265 when its first unit holds a VPS, otherwise H.264.
/// Its first unit must be a keyframe with its SPS; every loop, and every
/// resume, restarts at it. The sink marks discontinuities as it does in a session, so the first
/// unit after a resume carries one. Returns the source id, or 0 for a bad
/// path, clip or `fps`.
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
mod test_source;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// How soon a blocked `next` returns once it is woken: longer than a
    /// loaded machine may take to run the woken thread, and well short of
    /// the 1 s it waits when nothing wakes it, when it returns `CLOSED` too.
    const WOKEN: Duration = Duration::from_millis(500);

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
        // A failure is taken without its text; no host streams, so 0.
        assert_eq!(
            unsafe { pb_mapper_desktop_video_failed_v1(consumer, std::ptr::null()) },
            0
        );
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
        assert!(waited < WOKEN, "{waited:?}");
        pb_mapper_desktop_video_detach_v1(consumer);
        let consumer = pb_mapper_desktop_video_attach_v1(source);
        assert!(consumer > 0);
        let consumer = consumer as u64;

        // Disconnecting closes the source under a blocked pull.
        let (code, waited) = blocked_next(consumer, || remove_source(source));
        assert_eq!(code, CLOSED);
        assert!(waited < WOKEN, "{waited:?}");

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
            unsafe { pb_mapper_desktop_video_failed_v1(consumer, name.as_ptr()) },
            CLOSED
        );
        assert_eq!(
            unsafe { pb_mapper_desktop_video_report_v1(consumer, name.as_ptr(), 1) },
            CLOSED
        );
        pb_mapper_desktop_video_detach_v1(consumer);
        pb_mapper_desktop_video_detach_v1(consumer);
        remove_source(source);
        assert_ne!(pb_mapper_desktop_instance_token_v1(), 0);
    }
}
