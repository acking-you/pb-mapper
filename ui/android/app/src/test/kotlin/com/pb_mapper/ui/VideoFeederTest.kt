package com.pb_mapper.ui

import com.pb_mapper.ui.DesktopVideoNative.CLOSED
import com.pb_mapper.ui.DesktopVideoNative.FLAG_KEYFRAME
import com.pb_mapper.ui.DesktopVideoNative.NEED_CAPACITY
import com.pb_mapper.ui.DesktopVideoNative.PAUSED
import com.pb_mapper.ui.DesktopVideoNative.UNIT
import java.nio.ByteBuffer
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Test

class VideoFeederTest {
    /** A unit the fake source hands out, or a bare return code. */
    private class Step(val code: Int, val generation: Long = 1, val frameId: Long = 0, val key: Boolean = false, val len: Int = 16)

    /** Plays [steps] in order, then reports CLOSED. Records what the feeder did. */
    private class FakeSource(vararg steps: Step) : VideoSourceApi {
        val steps = ConcurrentLinkedQueue(steps.toList())
        val pulls = AtomicInteger()
        val capacities = ConcurrentLinkedQueue<Int>()
        val detached = CountDownLatch(1)

        override fun attach(source: Long) = 7L
        override fun next(consumer: Long, timeoutMs: Int, buffer: ByteBuffer, meta: LongArray): Int {
            pulls.incrementAndGet()
            capacities.add(buffer.capacity())
            val step = steps.peek() ?: return CLOSED
            if (step.code == UNIT && step.len > buffer.capacity()) {
                // Rust keeps the unit queued and says how much room it needs.
                meta[DesktopVideoNative.META_LEN] = step.len.toLong()
                return NEED_CAPACITY
            }
            steps.poll()
            if (step.code != UNIT) return step.code
            meta[DesktopVideoNative.META_GENERATION] = step.generation
            meta[DesktopVideoNative.META_FRAME_ID] = step.frameId
            meta[DesktopVideoNative.META_WIDTH] = 1920
            meta[DesktopVideoNative.META_HEIGHT] = 1080
            meta[DesktopVideoNative.META_LEN] = step.len.toLong()
            meta[DesktopVideoNative.META_FLAGS] = if (step.key) FLAG_KEYFRAME else 0
            return UNIT
        }
        override fun presented(consumer: Long, generation: Long, frameId: Long) = 1
        override fun keyframe(consumer: Long, reason: Int) = 1
        override fun failed(consumer: Long, message: String) = 1
        override fun report(consumer: Long, decoder: String, hardware: Boolean) = 0
        override fun detach(consumer: Long) = detached.countDown()
    }

    /** The real admission rules, with nothing draining them, like a stalled decoder. */
    private class QueueTarget(val feed: FeedQueue) : FeedTarget {
        val gaps = AtomicInteger()
        val pausedChanges = ConcurrentLinkedQueue<Boolean>()
        val failures = ConcurrentLinkedQueue<String>()
        val ends = ConcurrentLinkedQueue<VideoFeeder>()
        override fun awaitRoom(timeoutMs: Long) = feed.awaitRoom(timeoutMs)
        override fun offer(unit: FeedUnit) {
            if (feed.offer(unit) == FeedQueue.Admission.GAP) gaps.incrementAndGet()
        }
        override fun setPaused(paused: Boolean) { pausedChanges.add(paused) }
        override fun failed(message: String) { failures.add(message) }
        override fun ended(feeder: VideoFeeder) { ends.add(feeder) }
    }

    private fun run(source: FakeSource, target: FeedTarget, pool: BufferPool): VideoFeeder =
        VideoFeeder(source, 7, target, pool) {}.apply { start() }

    private fun eventually(condition: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (!condition()) {
            assertTrue("timed out", System.nanoTime() < deadline)
            Thread.sleep(5)
        }
    }

    @Test
    fun neverPullsWhileTwoWaitAndDropsReturnRoom() {
        val pool = BufferPool()
        val target = QueueTarget(FeedQueue(pool))
        // Every drop path, more times over than the pool has buffers: a drop
        // that kept its buffer or its room would strand the feeder before the
        // two units that fill the queue.
        val source = FakeSource(
            Step(UNIT, generation = 1, frameId = 5), // awaiting a key
            Step(UNIT, generation = 2, frameId = 1, key = true),
            Step(UNIT, generation = 1, frameId = 9, key = true), // older generation
            Step(UNIT, generation = 2, frameId = 1, key = true), // duplicate
            Step(UNIT, generation = 3, frameId = 1), // awaiting a key again
            Step(UNIT, generation = 2, frameId = 7), // older generation
            Step(UNIT, generation = 3, frameId = 2), // still awaiting a key
            Step(UNIT, generation = 3, frameId = 3, key = true),
            Step(UNIT, generation = 3, frameId = 4),
            Step(UNIT, generation = 3, frameId = 5),
        )
        val feeder = run(source, target, pool)

        eventually { target.feed.size == FEED_QUEUE_MAX }
        assertEquals(3, target.gaps.get())
        assertEquals(6L, target.feed.dropped.get())
        // Room never came back: the feeder waits rather than pulling the last unit.
        Thread.sleep(200)
        assertEquals(9, source.pulls.get())
        assertEquals(1, source.steps.size)

        // The decoder takes one: exactly one more pull follows.
        val head = target.feed.head()!!
        assertTrue(target.feed.poll(head))
        target.feed.release(head)
        eventually { source.steps.isEmpty() }
        feeder.close()
        assertEquals(0, target.failures.size)
        // Two queued units hold the only buffers missing from the pool.
        assertEquals(1, pool.available)
        target.feed.close()
        assertEquals(3, pool.available)
    }

    @Test
    fun needCapacityGrowsTheBufferAndTheSameUnitFollows() {
        val pool = BufferPool()
        val target = QueueTarget(FeedQueue(pool))
        val big = 3 shl 20
        val source = FakeSource(Step(UNIT, frameId = 1, key = true, len = big))
        run(source, target, pool)

        assertTrue(source.detached.await(5, TimeUnit.SECONDS))
        val head = target.feed.head()!!
        assertEquals(1L, head.frameId)
        assertEquals(big, head.len)
        assertEquals(4 shl 20, head.buffer.capacity())
        assertEquals(listOf(1 shl 20, 4 shl 20), source.capacities.take(2))
    }

    @Test
    fun pausedSetsPausedAndTheNextUnitClearsIt() {
        val pool = BufferPool()
        val target = QueueTarget(FeedQueue(pool))
        val source = FakeSource(Step(PAUSED), Step(PAUSED), Step(UNIT, frameId = 1, key = true))
        run(source, target, pool)

        assertTrue(source.detached.await(5, TimeUnit.SECONDS))
        assertEquals(listOf(true, true, false), target.pausedChanges.toList())
        assertEquals(1, target.feed.size)
    }

    @Test
    fun closedEndsTheThreadDetachesAndTellsTheTarget() {
        val pool = BufferPool()
        val target = QueueTarget(FeedQueue(pool))
        val source = FakeSource(Step(CLOSED), Step(UNIT, frameId = 1, key = true))
        val feeder = run(source, target, pool)

        feeder.join(5_000)
        assertFalse(feeder.isAlive)
        assertEquals(0L, source.detached.count)
        // Nothing else tells the texture its source is gone: without this, it
        // keeps holding the display's fastest refresh.
        assertEquals(listOf(feeder), target.ends.toList())
        assertEquals(1, source.pulls.get())
        assertEquals(0, target.feed.size)
        assertEquals(0, target.failures.size)
        assertEquals(3, pool.available)
    }
}
