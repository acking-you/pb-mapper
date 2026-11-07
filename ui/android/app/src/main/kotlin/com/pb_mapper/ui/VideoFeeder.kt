package com.pb_mapper.ui

import android.os.Process
import com.pb_mapper.ui.DesktopVideoNative.CLOSED
import com.pb_mapper.ui.DesktopVideoNative.NEED_CAPACITY
import com.pb_mapper.ui.DesktopVideoNative.PAUSED
import com.pb_mapper.ui.DesktopVideoNative.TIMEOUT
import com.pb_mapper.ui.DesktopVideoNative.UNIT
import java.nio.ByteBuffer
import java.util.ArrayDeque
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/** Where the feeder delivers; the decoder in the app, a fake in the JVM tests. */
internal interface FeedTarget {
    /** True once fewer than [FEED_QUEUE_MAX] units wait for the decoder. */
    fun awaitRoom(timeoutMs: Long): Boolean

    /** The only enqueue point; a unit it does not enqueue goes back to the pool. */
    fun offer(unit: FeedUnit)
    fun setPaused(paused: Boolean)
    fun failed(message: String)
}

internal const val FEED_QUEUE_MAX = 2

/** One access unit in a pooled buffer, as `next` described it in `meta`. */
internal class FeedUnit(
    val buffer: ByteBuffer,
    val len: Int,
    val generation: Long,
    val frameId: Long,
    val width: Int,
    val height: Int,
    val flags: Long,
    /** System.nanoTime() of the network receipt, from the age Rust measured at pop. */
    val receivedNs: Long,
) {
    /** Set under the queue's lock when the unit is enqueued. */
    var revision = 0L
        internal set
    val keyframe get() = flags and DesktopVideoNative.FLAG_KEYFRAME != 0L
    val discontinuity get() = flags and DesktopVideoNative.FLAG_DISCONTINUITY != 0L

    companion object {
        fun from(buffer: ByteBuffer, meta: LongArray) = FeedUnit(
            buffer,
            meta[DesktopVideoNative.META_LEN].toInt(),
            meta[DesktopVideoNative.META_GENERATION],
            meta[DesktopVideoNative.META_FRAME_ID],
            meta[DesktopVideoNative.META_WIDTH].toInt(),
            meta[DesktopVideoNative.META_HEIGHT].toInt(),
            meta[DesktopVideoNative.META_FLAGS],
            System.nanoTime() - meta[DesktopVideoNative.META_AGE_US] * 1000,
        )
    }
}

/**
 * Units waiting for the decoder, and the rules that admit them. Every path
 * that does not enqueue a unit, and every clear, returns its buffer to the
 * pool and signals room, so no drop can strand the feeder.
 *
 * A queued unit belongs to the queue: its buffer is read only under the lock
 * ([read]) or after [poll] hands it over, never after a clear returned it.
 */
internal class FeedQueue(private val pool: BufferPool) {
    enum class Admission { ENQUEUED, DROPPED, GAP }

    private val lock = ReentrantLock()
    private val room = lock.newCondition()
    private val queue = ArrayDeque<FeedUnit>(FEED_QUEUE_MAX)
    private val revisions = AtomicLong(1)
    private var acceptedGeneration = -1L
    private var lastAccepted = 0L
    private var awaitingKey = true
    private var closed = false
    val dropped = AtomicLong()

    /** Bumped by every discard; pictures of an older revision are never shown. */
    val revision: Long get() = revisions.get()
    val size: Int get() = lock.withLock { queue.size }

    fun awaitRoom(timeoutMs: Long): Boolean = lock.withLock {
        if (queue.size < FEED_QUEUE_MAX) return true
        room.await(timeoutMs, TimeUnit.MILLISECONDS)
        queue.size < FEED_QUEUE_MAX
    }

    fun offer(unit: FeedUnit): Admission = lock.withLock {
        if (closed || unit.generation < acceptedGeneration) return drop(unit, Admission.DROPPED)
        // A discontinuity restarts the decoder at this IDR: queued units and
        // decoded pictures of the old revision are superseded.
        if (unit.generation != acceptedGeneration || unit.discontinuity) {
            restart()
            acceptedGeneration = unit.generation
            lastAccepted = 0
        }
        if (unit.frameId <= lastAccepted) return drop(unit, Admission.DROPPED)
        lastAccepted = unit.frameId
        if (awaitingKey && !unit.keyframe) return drop(unit, Admission.GAP)
        awaitingKey = false
        unit.revision = revisions.get()
        queue.addLast(unit)
        Admission.ENQUEUED
    }

    /** The next unit for the decoder, still owned by the queue. */
    fun head(): FeedUnit? = lock.withLock { queue.peekFirst() }

    /** Runs [block] over [unit]'s bytes while it is still the head; null once a clear took it. */
    fun <T> read(unit: FeedUnit, block: (FeedUnit) -> T): T? = lock.withLock {
        if (queue.peekFirst() === unit) block(unit) else null
    }

    /** Hands [unit] to the caller, who must [release] it; false once a clear took it. */
    fun poll(unit: FeedUnit): Boolean = lock.withLock {
        if (queue.peekFirst() !== unit) return false
        queue.pollFirst()
        room.signalAll()
        true
    }

    /** Returns a polled unit's buffer once the decoder has copied it. */
    fun release(unit: FeedUnit) {
        pool.give(unit.buffer)
        lock.withLock { room.signalAll() }
    }

    /** The decoder failed: wait for the next keyframe in a new revision. */
    fun discard() = lock.withLock { restart() }

    /** A new source: forget the old session's generation and frame ids too. */
    fun reset() = lock.withLock {
        restart()
        acceptedGeneration = -1
        lastAccepted = 0
    }

    fun close() = lock.withLock {
        closed = true
        restart()
    }

    // Caller holds lock.
    private fun restart() {
        revisions.incrementAndGet()
        awaitingKey = true
        while (true) pool.give((queue.pollFirst() ?: break).buffer)
        room.signalAll()
    }

    // Caller holds lock.
    private fun drop(unit: FeedUnit, admission: Admission): Admission {
        pool.give(unit.buffer)
        dropped.incrementAndGet()
        room.signalAll()
        return admission
    }
}

/**
 * Pulls access units from Rust on its own thread, never the main thread.
 * It waits for room before each pull, so Rust's sink stays the only place a
 * backlog can build and be dropped to the next keyframe.
 */
internal class VideoFeeder(
    private val api: VideoSourceApi,
    private val consumer: Long,
    private val target: FeedTarget,
    private val pool: BufferPool,
    // android.os.Process throws "not mocked" in JVM unit tests; they pass {}.
    private val raisePriority: () -> Unit = { Process.setThreadPriority(Process.THREAD_PRIORITY_DISPLAY) },
) : Thread("pb-desktop-feed") {
    @Volatile var stopped = false
        private set

    override fun run() {
        try {
            raisePriority()
            val meta = LongArray(DesktopVideoNative.META_SIZE)
            while (!stopped) {
                if (!target.awaitRoom(PULL_TIMEOUT_MS.toLong())) continue
                // Three buffers cover two queued units and one in hand.
                val buf = pool.take() ?: continue
                when (val r = api.next(consumer, PULL_TIMEOUT_MS, buf, meta)) {
                    UNIT -> {
                        target.setPaused(false)
                        target.offer(FeedUnit.from(buf, meta))
                    }
                    TIMEOUT -> pool.give(buf)
                    PAUSED -> {
                        pool.give(buf)
                        target.setPaused(true)
                    }
                    // The unit stays queued in Rust; the grown buffer is the next one taken.
                    NEED_CAPACITY -> pool.give(pool.grow(buf, meta[DesktopVideoNative.META_LEN].toInt()))
                    CLOSED -> {
                        pool.give(buf)
                        break
                    }
                    else -> {
                        pool.give(buf)
                        if (!stopped) target.failed("Video feed error $r")
                        break
                    }
                }
            }
        } catch (e: Exception) {
            // An uncaught exception on this thread would end the process.
            target.failed(e.message ?: "Video feed failed")
        } finally {
            api.detach(consumer)
        }
    }

    /** Ends the thread at once: detach wakes a blocked pull. */
    fun close() {
        stopped = true
        api.detach(consumer)
        join()
    }

    companion object {
        const val PULL_TIMEOUT_MS = 50
    }
}
