package com.pb_mapper.ui

import org.junit.Assert.*
import org.junit.Test

class VideoPresentationQueueTest {
    @Test
    fun decodedPicturesAreImmediatelyEligibleAndBurstsDoNotReplayOlderPictures() {
        val queue = VideoPresentationQueue()
        queue.enqueue(0, 1, 1, 1, 100_000_000)
        assertTrue(queue.take(1, false)!!.render)

        // A late network burst must show the newest result of remote input,
        // without scheduling the intervening pictures on a playback timeline.
        queue.enqueue(1, 2, 1, 2, 101_000_000)
        queue.enqueue(2, 3, 1, 3, 102_000_000)
        assertFalse(queue.take(1, false)!!.render)
        val newest = queue.take(1, false)!!
        assertTrue(newest.render)
        assertEquals(3L, newest.frameId)
    }

    @Test
    fun pausedSurfaceDiscardsOnlySupersededDecodedPicturesAndRetainsNewestUntilConsumed() {
        val queue = VideoPresentationQueue()
        queue.enqueue(0, 1, 1, 1, 0)
        assertTrue(queue.take(1, false)!!.render)
        queue.enqueue(1, 2, 1, 2, 16_000_000)
        queue.enqueue(2, 3, 1, 3, 32_000_000)
        queue.enqueue(3, 4, 1, 4, 48_000_000)

        val second = queue.take(1, true)!!
        assertEquals(2L, second.frameId)
        assertFalse(second.render)
        val third = queue.take(1, true)!!
        assertEquals(3L, third.frameId)
        assertFalse(third.render)
        assertNull(queue.take(1, true))
        assertEquals(1, queue.size)

        val newest = queue.take(1, false)!!
        assertTrue(newest.render)
        assertEquals(4L, newest.frameId)
        assertNull(queue.take(1, false))
    }

    @Test
    fun resetRetiresOldRevisionEvenWithNoSurfaceCredit() {
        val queue = VideoPresentationQueue()
        queue.enqueue(0, 1, 1, 1, 0)
        queue.enqueue(1, 2, 1, 2, 1_000_000)
        assertFalse(queue.take(2, true)!!.render)
        assertFalse(queue.take(2, true)!!.render)
        assertEquals(0, queue.size)

        queue.clear()
        queue.enqueue(0, 3, 2, 1, 2_000_000)
        val first = queue.take(2, false)!!
        assertTrue(first.render)
        assertEquals(3L, first.ptsUs)
        assertEquals(0, queue.size)
    }
}
