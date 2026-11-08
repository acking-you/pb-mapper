package com.pb_mapper.ui

import com.pb_mapper.ui.DecoderRules.Step
import com.pb_mapper.ui.DesktopVideoNative.FLAG_DISCONTINUITY
import com.pb_mapper.ui.DesktopVideoNative.FLAG_H265
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
        val configured = VideoFormat(1920, 1080, hevc = false)
        assertEquals(Step.FEED, DecoderRules.step(first, feed.revision, configured))

        // The resume or recovery IDR starts a new revision; the codec configured
        // in the old one takes it, and only older units are thrown away.
        val resumed = admit(2, flags = FLAG_KEYFRAME or FLAG_DISCONTINUITY)
        assertNotEquals(first.revision, resumed.revision)
        assertEquals(Step.FEED, DecoderRules.step(resumed, feed.revision, configured))
        assertEquals(Step.DISCARD, DecoderRules.step(first, feed.revision, configured))
    }

    @Test
    fun aSizeOrCodecChangeOrAMissingCodecConfigures() {
        val unit = admit(1, width = 2560, height = 1440)
        assertEquals(Step.CONFIGURE, DecoderRules.step(unit, feed.revision, VideoFormat(1920, 1080, hevc = false)))
        assertEquals(Step.CONFIGURE, DecoderRules.step(unit, feed.revision, null))
        // The next session's first IDR, the same size in another codec.
        val h265 = admit(2, width = 2560, height = 1440, flags = FLAG_KEYFRAME or FLAG_H265)
        assertTrue(h265.hevc)
        assertEquals(Step.CONFIGURE, DecoderRules.step(h265, feed.revision, VideoFormat(2560, 1440, hevc = false)))
        assertEquals(Step.FEED, DecoderRules.step(h265, feed.revision, VideoFormat(2560, 1440, hevc = true)))
    }

    @Test
    fun aKeyFramesParameterSetsConfigureTheDecoder() {
        fun unit(vararg nals: ByteArray) = nals.fold(ByteArray(0)) { all, nal -> all + START + nal }
        fun config(bytes: ByteArray, hevc: Boolean) = DecoderRules.codecConfig(ByteBuffer.wrap(bytes), bytes.size, hevc)
        val sps = bytes(0x67, 0x42, 0xc0, 0x28)
        val pps = bytes(0x68, 0xce, 0x3c, 0x80)
        val avc = config(unit(sps, pps, bytes(0x65, 0x88, 0x84)), hevc = false)
        assertArrayEquals(START + sps, avc["csd-0"])
        assertArrayEquals(START + pps, avc["csd-1"])

        // H.265's three go together in csd-0, in order, and a key frame
        // without one of them configures nothing.
        val vps = bytes(0x40, 0x01, 0x0c, 0x01)
        val hevcSps = bytes(0x42, 0x01, 0x01, 0x01)
        val hevcPps = bytes(0x44, 0x01, 0xc1, 0xa5)
        val sei = bytes(0x4e, 0x01, 0x05, 0xff)
        val idr = bytes(0x26, 0x01, 0xaf, 0x10)
        val hevc = config(unit(vps, hevcSps, hevcPps, sei, idr), hevc = true)
        assertEquals(setOf("csd-0"), hevc.keys)
        assertArrayEquals(START + vps + START + hevcSps + START + hevcPps, hevc["csd-0"])
        assertThrows(IllegalArgumentException::class.java) { config(unit(vps, hevcSps, idr), hevc = true) }
    }

    private companion object {
        val START = bytes(0, 0, 0, 1)
        fun bytes(vararg values: Int) = ByteArray(values.size) { values[it].toByte() }
    }
}
