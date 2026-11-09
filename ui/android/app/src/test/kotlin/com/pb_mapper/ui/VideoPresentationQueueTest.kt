package com.pb_mapper.ui

import com.pb_mapper.ui.VideoPresentationQueue.Companion.CREDITS
import java.util.ArrayDeque
import kotlin.random.Random
import org.junit.Assert.*
import org.junit.Test

class VideoPresentationQueueTest {
    @Test
    fun jitteredPicturesShowInOrderAndSupersedeOnlyWithBothCreditsInUse() {
        val queue = VideoPresentationQueue()
        // Decoded pictures 11.7 to 30 ms apart. A released picture must reach
        // Flutter's scheduler 8 ms before a vsync, and that frame's raster
        // latches the oldest, 10 ms after the vsync: consumption comes more
        // than a vsync after release, the loop that held one credit to 30 fps.
        // The raster stalls once.
        val vsyncUs = 16_667L
        val announceUs = 8_000L
        val rasterUs = 10_000L
        val random = Random(51)
        val arrivals = generateSequence(0L) { it + 11_700 + random.nextInt(18_301) }
            .takeWhile { it < 3_000_000 }.toList()
        val stall = 1_500_000L until 1_600_000L
        // SurfaceTexture's FIFO queue: pts and release time.
        val surface = ArrayDeque<LongArray>()
        val shown = mutableListOf<Long>()
        val supersededAtUs = mutableListOf<Long>()

        fun release(nowUs: Long) {
            while (true) {
                val output = queue.take(1) ?: return
                if (output.render) {
                    queue.rendered(output.ptsUs, nowUs / 1000)
                    surface.addLast(longArrayOf(output.ptsUs, nowUs))
                } else {
                    // Never while a credit is free.
                    assertEquals(CREDITS, queue.credits)
                    supersededAtUs.add(nowUs)
                }
            }
        }

        var next = 0
        var vsync = 0L
        var resumed = 0
        while (vsync <= arrivals.last() + 10 * vsyncUs) {
            val latchUs = vsync + rasterUs
            while (next < arrivals.size && arrivals[next] < latchUs) {
                val frameId = next + 1L
                queue.enqueue(next, frameId * 1000, 1, frameId, 0)
                release(arrivals[next])
                next++
            }
            val head = surface.peekFirst()
            if (vsync !in stall && head != null && head[1] + announceUs <= vsync) {
                if (resumed == 0 && vsync > stall.last) resumed = next
                surface.removeFirst()
                assertTrue(queue.consumed(head[0]) { fail("the surface drops nothing") })
                shown.add(head[0] / 1000)
                release(latchUs)
            }
            vsync += vsyncUs
        }

        // Consumed in decode order, each once, and every picture accounted for.
        assertEquals(shown.distinct().sorted(), shown)
        assertEquals(arrivals.size, shown.size + supersededAtUs.size)
        // While the raster keeps up, every jittered picture is shown; the
        // stall superseded some, and kept the newest for the frame after it.
        assertTrue(supersededAtUs.all { it >= stall.first })
        assertTrue(supersededAtUs.isNotEmpty())
        assertTrue(resumed.toLong() in shown)
        assertEquals(0, queue.credits)
        assertEquals(0, queue.size)
    }

    @Test
    fun aPictureTheSurfaceDroppedReturnsItsCreditWithTheNextConsumption() {
        val queue = VideoPresentationQueue()
        queue.rendered(1_000, 0)
        queue.rendered(2_000, 0)
        // A repaint, or a picture of a released codec, returns no credit.
        assertFalse(queue.consumed(500) { fail("nothing is skipped") })
        assertEquals(2, queue.credits)

        // FIFO: once the newer picture is latched, the older never will be.
        val skipped = mutableListOf<Long>()
        assertTrue(queue.consumed(2_000) { skipped.add(it) })
        assertEquals(listOf(1_000L), skipped)
        assertEquals(0, queue.credits)
    }

    @Test
    fun aNewRevisionRetiresOldPicturesEvenWithBothCreditsInUse() {
        val queue = VideoPresentationQueue()
        queue.rendered(1_000, 0)
        queue.rendered(2_000, 0)
        queue.enqueue(0, 3_000, 1, 3, 0)
        queue.enqueue(1, 4_000, 1, 4, 0)
        assertFalse(queue.take(2)!!.render)
        assertFalse(queue.take(2)!!.render)
        assertEquals(0, queue.size)
        assertNull(queue.take(2))
    }
}
