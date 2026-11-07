package com.pb_mapper.ui

import com.pb_mapper.ui.DecoderRules.Step
import com.pb_mapper.ui.DesktopVideoNative.FLAG_DISCONTINUITY
import com.pb_mapper.ui.DesktopVideoNative.FLAG_KEYFRAME
import java.nio.ByteBuffer
import org.junit.Assert.*
import org.junit.Test

class DecoderRulesTest {
    private val feed = FeedQueue(BufferPool())

    private fun admit(frameId: Long, width: Int = 1920, height: Int = 1080, flags: Long = FLAG_KEYFRAME): FeedUnit {
        val unit = FeedUnit(ByteBuffer.allocate(16), 16, 1, frameId, width, height, flags, 0)
        assertEquals(FeedQueue.Admission.ENQUEUED, feed.offer(unit))
        return unit
    }

    @Test
    fun aSameSizeDiscontinuityFeedsTheRunningCodec() {
        val first = admit(1)
        val configured = FrameSize(1920, 1080)
        assertEquals(Step.FEED, DecoderRules.step(first, feed.revision, configured))

        // The resume or recovery IDR starts a new revision; the codec configured
        // in the old one takes it, and only older units are thrown away.
        val resumed = admit(2, flags = FLAG_KEYFRAME or FLAG_DISCONTINUITY)
        assertNotEquals(first.revision, resumed.revision)
        assertEquals(Step.FEED, DecoderRules.step(resumed, feed.revision, configured))
        assertEquals(Step.DISCARD, DecoderRules.step(first, feed.revision, configured))
    }

    @Test
    fun aSizeChangeOrAMissingCodecConfigures() {
        val unit = admit(1, width = 2560, height = 1440)
        assertEquals(Step.CONFIGURE, DecoderRules.step(unit, feed.revision, FrameSize(1920, 1080)))
        assertEquals(Step.CONFIGURE, DecoderRules.step(unit, feed.revision, null))
    }
}
