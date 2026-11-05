package com.pb_mapper.ui

import java.util.ArrayDeque

/** Worker-owned decoded pictures; compressed H.264 reference frames are never dropped here. */
internal class VideoPresentationQueue {
    class Output(
        val index: Int,
        val ptsUs: Long,
        val revision: Long,
        val frameId: Long,
        val readyNs: Long,
    ) {
        var render = false
            internal set
    }

    private val outputs = ArrayDeque<Output>()
    val size: Int get() = outputs.size

    fun enqueue(index: Int, ptsUs: Long, revision: Long, frameId: Long, nowNs: Long) {
        // This is an interactive desktop, not a media timeline. A decoded
        // picture is immediately eligible; source pacing after a network burst
        // would make us play old input results while newer pictures are ready.
        outputs.addLast(Output(index, ptsUs, revision, frameId, nowNs))
    }

    /** Return one buffer to release, or keep the newest picture until the surface has credit. */
    fun take(revision: Long, surfaceBusy: Boolean): Output? {
        val output = outputs.pollFirst() ?: return null
        val next = outputs.peekFirst()
        val current = output.revision == revision
        val superseded = current && next != null && next.revision == revision &&
            next.frameId > output.frameId
        if (current && !superseded && surfaceBusy) {
            outputs.addFirst(output)
            return null
        }
        // Skipping decoded pictures keeps the codec's predictive references.
        // After a render pause, replaying old pictures only increases latency.
        output.render = current && !superseded
        return output
    }

    fun clear() {
        outputs.clear()
    }
}
