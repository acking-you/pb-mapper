package com.pb_mapper.ui

import java.util.ArrayDeque

/**
 * Worker-owned decoded pictures and the surface's presentation credits;
 * compressed reference frames, H.264 or H.265, are never dropped here.
 *
 * Up to [CREDITS] pictures may be released to the SurfaceTexture and not yet
 * consumed. Its queue is FIFO: each updateTexImage latches the oldest, so a
 * picture's credit returns when Flutter consumes its timestamp or a later one.
 */
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

    private class Released(val ptsUs: Long, val atMs: Long)

    private val outputs = ArrayDeque<Output>()
    private val released = ArrayDeque<Released>(CREDITS)

    /** Decoded pictures waiting for a credit. */
    val size: Int get() = outputs.size

    /** Pictures released to the surface and not yet consumed: the credits in use. */
    val credits: Int get() = released.size

    /** When the oldest picture still holding a credit was released; 0 with none. */
    val oldestReleaseMs: Long get() = released.peekFirst()?.atMs ?: 0

    fun enqueue(index: Int, ptsUs: Long, revision: Long, frameId: Long, nowNs: Long) {
        // This is an interactive desktop, not a media timeline. A decoded
        // picture is immediately eligible; source pacing after a network burst
        // would make us play old input results while newer pictures are ready.
        outputs.addLast(Output(index, ptsUs, revision, frameId, nowNs))
    }

    /**
     * One buffer to release, or null to keep the head until a credit frees.
     * While a credit is free, pictures go to the surface in decode order, so
     * a jittered pair shows on consecutive frames. Only with both credits in
     * use does a newer decoded picture supersede a waiting one.
     */
    fun take(revision: Long): Output? {
        val output = outputs.pollFirst() ?: return null
        val next = outputs.peekFirst()
        val current = output.revision == revision
        val busy = released.size >= CREDITS
        val superseded = current && busy && next != null && next.revision == revision &&
            next.frameId > output.frameId
        if (current && !superseded && busy) {
            outputs.addFirst(output)
            return null
        }
        // Skipping decoded pictures keeps the codec's predictive references.
        // After a render pause, replaying old pictures only increases latency.
        output.render = current && !superseded
        return output
    }

    /** The caller released [ptsUs] to the surface; it holds a credit until consumed. */
    fun rendered(ptsUs: Long, nowMs: Long) {
        check(released.size < CREDITS) { "No surface credit" }
        released.addLast(Released(ptsUs, nowMs))
    }

    /**
     * Flutter latched [ptsUs]: its credit returns. False when it holds none
     * (a repaint, or a picture of a released codec). Older pictures still
     * holding credits were never latched, so the surface dropped them: their
     * credits return too, each through [skipped], and none is stranded.
     */
    fun consumed(ptsUs: Long, skipped: (Long) -> Unit): Boolean {
        if (released.none { it.ptsUs == ptsUs }) return false
        while (true) {
            val oldest = released.pollFirst()?.ptsUs ?: return true
            if (oldest == ptsUs) return true
            skipped(oldest)
        }
    }

    fun clear() {
        outputs.clear()
        released.clear()
    }

    companion object {
        /** Released-but-unconsumed pictures: a double buffer behind Flutter's frame. */
        const val CREDITS = 2
    }
}
