//! The Android feeder's way into `desktop_video`: the same registry, ids and
//! return codes, as the static natives of `DesktopVideoNative`. The design is
//! pull-only, so there is no `JNI_OnLoad`, no cached `JavaVM` and no Android
//! context. A JNI failure clears its pending exception and returns `INVALID`,
//! so Kotlin always sees a code, never a throw.
#![allow(non_snake_case)]

use crate::desktop_video::{self as video, INVALID, NEED_CAPACITY, PbDesktopVideoMetaV1, UNIT};
use jni::{
    JNIEnv,
    objects::{JByteBuffer, JClass, JLongArray, JString},
    sys::{jboolean, jint, jlong},
};
use pb_mapper::desktop::Pull;

/// `next` fills `meta[0..10]`: generation, frame id, seq, pts, age, width,
/// height, len, flags, 0.
const META_LEN: i32 = 10;
const META_INDEX_LEN: i32 = 7;

/// Runs a JNI body; a JNI error or a panic becomes `INVALID`, with any
/// exception it left pending cleared.
fn call(env: &mut JNIEnv, body: impl FnOnce(&mut JNIEnv) -> jni::errors::Result<jint>) -> jint {
    let code = video::guard(INVALID, || body(env).unwrap_or(INVALID));
    if env.exception_check().unwrap_or(true) {
        let _ = env.exception_clear();
    }
    code
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_pb_1mapper_ui_DesktopVideoNative_instanceToken(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    video::instance_token() as jlong
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_pb_1mapper_ui_DesktopVideoNative_attach(
    _env: JNIEnv,
    _class: JClass,
    source: jlong,
) -> jlong {
    match u64::try_from(source) {
        Ok(source) => video::guard(INVALID.into(), || video::attach(source)),
        Err(_) => INVALID.into(),
    }
}

/// Copies the next unit into the direct `buffer` (its whole capacity) and
/// fills `meta`. On `NEED_CAPACITY` only `meta[7]` is set, to the size needed,
/// and the unit stays queued. The feeder thread only.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_pb_1mapper_ui_DesktopVideoNative_next(
    mut env: JNIEnv,
    _class: JClass,
    consumer: jlong,
    timeout_ms: jint,
    buffer: JByteBuffer,
    meta: JLongArray,
) -> jint {
    call(&mut env, |env| {
        let (Ok(consumer), Ok(timeout_ms)) = (u64::try_from(consumer), u32::try_from(timeout_ms))
        else {
            return Ok(INVALID);
        };
        // Checked before the pull, so a unit is never popped and then lost.
        if env.get_array_length(&meta)? < META_LEN {
            return Ok(INVALID);
        }
        // Both fail for a buffer that is not direct.
        let capacity = env.get_direct_buffer_capacity(&buffer)?;
        let address = env.get_direct_buffer_address(&buffer)?;
        Ok(match video::next(consumer, timeout_ms, capacity) {
            Ok(Pull::Unit(unit)) => {
                // SAFETY: the unit is at most `capacity` bytes, and the direct
                // buffer owns `capacity` bytes at `address` while Kotlin holds it.
                unsafe {
                    std::ptr::copy_nonoverlapping(unit.bytes.as_ptr(), address, unit.bytes.len());
                }
                let meta_of = PbDesktopVideoMetaV1::of(&unit);
                let values = [
                    meta_of.generation,
                    meta_of.frame_id,
                    meta_of.seq,
                    meta_of.pts_us,
                    meta_of.age_us,
                    meta_of.width.into(),
                    meta_of.height.into(),
                    meta_of.len.into(),
                    meta_of.flags.into(),
                    0,
                ]
                .map(|value| value as jlong);
                env.set_long_array_region(&meta, 0, &values)?;
                UNIT
            }
            Ok(Pull::NeedCapacity(len)) => {
                let len = i64::try_from(len).unwrap_or(i64::MAX);
                env.set_long_array_region(&meta, META_INDEX_LEN, &[len])?;
                NEED_CAPACITY
            }
            Ok(pull) => video::code(&pull),
            Err(code) => code,
        })
    })
}

/// The codec thread presented `(generation, frame_id)`: 1 accepted, 0
/// rejected, or `CLOSED`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_pb_1mapper_ui_DesktopVideoNative_presented(
    _env: JNIEnv,
    _class: JClass,
    consumer: jlong,
    generation: jlong,
    frame_id: jlong,
) -> jint {
    match (
        u64::try_from(consumer),
        u64::try_from(generation),
        u64::try_from(frame_id),
    ) {
        (Ok(consumer), Ok(generation), Ok(frame_id)) => {
            video::guard(INVALID, || video::presented(consumer, generation, frame_id))
        }
        _ => INVALID,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_pb_1mapper_ui_DesktopVideoNative_keyframe(
    _env: JNIEnv,
    _class: JClass,
    consumer: jlong,
    reason: jint,
) -> jint {
    match (u64::try_from(consumer), u32::try_from(reason)) {
        (Ok(consumer), Ok(reason)) => video::guard(INVALID, || video::keyframe(consumer, reason)),
        _ => INVALID,
    }
}

/// The decoder's name (64 characters are kept) and whether it is hardware.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_pb_1mapper_ui_DesktopVideoNative_report(
    mut env: JNIEnv,
    _class: JClass,
    consumer: jlong,
    decoder: JString,
    hardware: jboolean,
) -> jint {
    call(&mut env, |env| {
        let Ok(consumer) = u64::try_from(consumer) else {
            return Ok(INVALID);
        };
        let decoder: String = env.get_string(&decoder)?.into();
        Ok(video::report(consumer, &decoder, hardware != 0))
    })
}

/// Idempotent; wakes a `next` blocked on this consumer.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_pb_1mapper_ui_DesktopVideoNative_detach(
    _env: JNIEnv,
    _class: JClass,
    consumer: jlong,
) {
    if let Ok(consumer) = u64::try_from(consumer) {
        video::guard((), || video::detach(consumer));
    }
}
