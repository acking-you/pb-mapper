package com.pb_mapper.ui

import java.nio.Buffer
import java.nio.ByteBuffer
import java.util.ArrayDeque

/**
 * The direct buffers Rust copies access units into. Three cover two queued
 * units plus the one in hand, so the feeder never allocates per frame.
 * Shared by the feeder, the codec thread and the channel thread.
 */
internal class BufferPool(
    private val count: Int = 3,
    private val initial: Int = 1 shl 20,
    private val max: Int = 4 shl 20,
) {
    private val free = ArrayDeque<ByteBuffer>(count)
    private var created = 0

    /** A free buffer, or null while all of them are queued or in hand. */
    @Synchronized
    fun take(): ByteBuffer? {
        free.pollFirst()?.let { return it }
        if (created == count) return null
        created++
        return ByteBuffer.allocateDirect(initial)
    }

    /** Returns [b] to the front, so a buffer just grown serves the next pull. */
    @Synchronized
    fun give(b: ByteBuffer) {
        // A second return of one buffer would hand it to two writers at once.
        if (free.any { it === b }) return
        b.span(0, b.capacity())
        free.addFirst(b)
    }

    /** Replaces [b] with a buffer of at least [needed] bytes: the next power of two, at most [max]. */
    fun grow(b: ByteBuffer, needed: Int): ByteBuffer {
        require(needed in 1..max) { "Video unit of $needed bytes exceeds $max" }
        if (b.capacity() >= needed) return b
        val size = (Integer.highestOneBit(needed - 1) shl 1).coerceIn(needed, max)
        return ByteBuffer.allocateDirect(size)
    }

    val available: Int
        @Synchronized get() = free.size + (count - created)
}

/**
 * Selects bytes [start, end). Through [Buffer] on purpose: ByteBuffer's own
 * covariant clear/position/limit arrived with API 34, and a call compiled
 * against them would fail on older devices.
 */
internal fun Buffer.span(start: Int, end: Int) {
    clear()
    position(start)
    limit(end)
}
