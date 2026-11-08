package com.pb_mapper.ui

import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build

/**
 * The `decoders` channel method's answer: for H.264 and H.265, the largest of
 * the viewer's resolution bounds a hardware decoder takes at 60 fps, or null,
 * and the display's fastest refresh. It reads the codec list and creates no
 * codec: a bad driver can hang there.
 */
internal object DecoderProbe {
    // Resolution::bounds in lib.rs, largest first.
    private val BOUNDS = listOf(3840 to 2160, 2560 to 1440, 1920 to 1080, 1280 to 720)

    fun decoders(displayMaxHz: Double): Map<String, Any?> {
        val hardware = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
            .filter { !it.isEncoder && hardware(it) }
        return mapOf(
            "h264" to largest(hardware, MediaFormat.MIMETYPE_VIDEO_AVC),
            "h265" to largest(hardware, MediaFormat.MIMETYPE_VIDEO_HEVC),
            "display_max_hz" to displayMaxHz,
        )
    }

    private fun largest(decoders: List<MediaCodecInfo>, mime: String): Map<String, Int>? {
        val video = decoders
            .filter { info -> info.supportedTypes.any { it.equals(mime, ignoreCase = true) } }
            .mapNotNull { info ->
                try {
                    info.getCapabilitiesForType(mime).videoCapabilities
                } catch (_: IllegalArgumentException) {
                    null
                }
            }
        val (width, height) = BOUNDS.firstOrNull { (width, height) ->
            video.any { it.areSizeAndRateSupported(width, height, 60.0) }
        } ?: return null
        return mapOf("max_width" to width, "max_height" to height)
    }

    /** Whether a decoder runs on hardware; also what the overlay reports of it. */
    fun hardware(info: MediaCodecInfo): Boolean =
        if (Build.VERSION.SDK_INT >= 29) {
            info.isHardwareAccelerated
        } else {
            // Before API 29, the platform's software decoders are named.
            !info.name.startsWith("OMX.google.") && !info.name.startsWith("c2.android.")
        }
}
