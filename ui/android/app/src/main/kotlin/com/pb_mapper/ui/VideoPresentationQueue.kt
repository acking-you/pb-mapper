package com.pb_mapper.ui

import java.util.ArrayDeque

/** Worker-owned decoded pictures; compressed H.264 reference frames are never dropped here. */
internal class VideoPresentationQueue {
    class Output(
        val index: Int,
        val ptsUs: Long,
        val revision: Long,
        val frameId: Long,
        val dueNs: Long,
    ) {
        var render = false
            internal set
    }

    private val outputs = ArrayDeque<Output>()
    private var lastSourcePtsUs: Long? = null
    private var lastScheduledNs = 0L
    val size: Int get() = outputs.size
    val nextDeadlineNs: Long? get() = outputs.peekFirst()?.dueNs

    fun enqueue(index: Int, ptsUs: Long, revision: Long, frameId: Long, sourcePtsUs: Long, nowNs: Long) {
        val previous = lastSourcePtsUs
        val step = if (previous == null) 0 else sourcePtsUs - previous
        // Preserve capture cadence across short TCP bursts, without adding a
        // startup buffer. A discontinuity re-anchors instead of growing latency.
        val candidate = lastScheduledNs + step.coerceIn(0, 80_000) * 1000
        val due = if (previous == null || step !in 1L..80_000L ||
            candidate < nowNs - 25_000_000 || candidate > nowNs + 80_000_000) {
            nowNs
        } else candidate
        lastSourcePtsUs = sourcePtsUs
        lastScheduledNs = due
        outputs.addLast(Output(index, ptsUs, revision, frameId, due))
    }

    /** Return one buffer to release, or keep the newest due picture until the surface has credit. */
    fun take(nowNs: Long, revision: Long, surfaceBusy: Boolean): Output? {
        val output = outputs.pollFirst() ?: return null
        val next = outputs.peekFirst()
        val current = output.revision == revision
        val superseded = current && next != null && next.revision == revision &&
            next.frameId > output.frameId && next.dueNs <= nowNs
        if (current && !superseded && (surfaceBusy || output.dueNs > nowNs)) {
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
        lastSourcePtsUs = null
        lastScheduledNs = 0
    }
}
