package com.pb_mapper.ui

import java.nio.ByteBuffer

/** The Rust video source as the feeder and decoder see it; JVM tests fake it. */
internal interface VideoSourceApi {
    fun attach(source: Long): Long
    fun next(consumer: Long, timeoutMs: Int, buffer: ByteBuffer, meta: LongArray): Int
    fun presented(consumer: Long, generation: Long, frameId: Long): Int
    fun keyframe(consumer: Long, reason: Int): Int
    fun report(consumer: Long, decoder: String, hardware: Boolean): Int
    fun detach(consumer: Long)
}

/**
 * The JNI exports of `pb_mapper_ffi` (desktop_jni.rs). Dart dlopen()s the
 * same soname first; `instanceToken` proves both reach one library instance.
 *
 * Every constant is `const`, so reading one never runs `init` and never loads
 * the library: the JVM tests use them without it.
 */
internal object DesktopVideoNative {
    init {
        System.loadLibrary("pb_mapper_ffi")
    }

    // The return codes every video export and JNI function shares.
    const val UNIT = 1
    const val TIMEOUT = 0
    const val CLOSED = -1
    const val NEED_CAPACITY = -2
    const val PAUSED = -3
    const val NOT_ATTACHED = -4
    const val INVALID = -5
    const val ALREADY_ATTACHED = -6

    // `next` fills `meta` at these indices.
    const val META_GENERATION = 0
    const val META_FRAME_ID = 1
    const val META_SEQ = 2
    const val META_PTS_US = 3
    const val META_AGE_US = 4
    const val META_WIDTH = 5
    const val META_HEIGHT = 6
    const val META_LEN = 7
    const val META_FLAGS = 8
    const val META_SIZE = 10

    const val FLAG_KEYFRAME = 1L
    const val FLAG_DISCONTINUITY = 2L
    const val FLAG_JPEG = 4L
    const val FLAG_DUPLICATE = 8L

    const val REASON_DECODER_ERROR = 0
    const val REASON_GAP = 1
    const val REASON_STALL = 3

    @JvmStatic external fun instanceToken(): Long
    @JvmStatic external fun attach(source: Long): Long
    @JvmStatic external fun next(consumer: Long, timeoutMs: Int, buffer: ByteBuffer, meta: LongArray): Int
    @JvmStatic external fun presented(consumer: Long, generation: Long, frameId: Long): Int
    @JvmStatic external fun keyframe(consumer: Long, reason: Int): Int
    @JvmStatic external fun report(consumer: Long, decoder: String, hardware: Boolean): Int
    @JvmStatic external fun detach(consumer: Long)

    // An object cannot implement an interface with @JvmStatic overrides, and
    // the JNI symbols need the static methods; this adapter delegates.
    val api: VideoSourceApi = object : VideoSourceApi {
        override fun attach(source: Long) = DesktopVideoNative.attach(source)
        override fun next(consumer: Long, timeoutMs: Int, buffer: ByteBuffer, meta: LongArray) =
            DesktopVideoNative.next(consumer, timeoutMs, buffer, meta)
        override fun presented(consumer: Long, generation: Long, frameId: Long) =
            DesktopVideoNative.presented(consumer, generation, frameId)
        override fun keyframe(consumer: Long, reason: Int) = DesktopVideoNative.keyframe(consumer, reason)
        override fun report(consumer: Long, decoder: String, hardware: Boolean) =
            DesktopVideoNative.report(consumer, decoder, hardware)
        override fun detach(consumer: Long) = DesktopVideoNative.detach(consumer)
    }
}
