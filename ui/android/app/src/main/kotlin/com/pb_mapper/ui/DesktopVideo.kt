package com.pb_mapper.ui

import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.Surface
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel
import io.flutter.view.TextureRegistry
import java.nio.ByteBuffer
import java.util.ArrayDeque
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicBoolean

/** Only compressed access units cross the channel; decoding stays off the UI thread. */
@Suppress("DEPRECATION")
class DesktopVideo(messenger: BinaryMessenger, private val textures: TextureRegistry) {
    private val main = Handler(Looper.getMainLooper())
    private val channel = MethodChannel(messenger, "pb_mapper/desktop_video")
    private val viewers = mutableMapOf<Long, Viewer>()

    init {
        channel.setMethodCallHandler { call, result ->
            try {
                if (call.method == "create") {
                    check(viewers.size < 2) { "Desktop texture limit reached" }
                    val viewer = Viewer(textures.createSurfaceTexture())
                    viewers[viewer.id] = viewer
                    result.success(viewer.id)
                } else {
                    val id = call.argument<Number>("textureId")?.toLong()
                    val viewer = viewers[id] ?: error("Unknown desktop texture")
                    when (call.method) {
                        "submit" -> {
                            val bytes = call.argument<ByteArray>("bytes") ?: error("Missing video")
                            val width = call.argument<Number>("width")?.toInt() ?: 0
                            val height = call.argument<Number>("height")?.toInt() ?: 0
                            require(bytes.isNotEmpty() && bytes.size <= MAX_BYTES && width in 2..7680 && height in 2..4320 && width.toLong() * height <= 16_777_216) { "Invalid video bounds" }
                            viewer.submit(Packet(bytes, call.argument<Number>("generation")!!.toLong(), call.argument<Number>("frameId")!!.toLong(), call.argument<Number>("ptsUs")!!.toLong(), width, height, call.argument<Boolean>("keyframe") == true, viewer.revision.get()), result)
                        }
                        "reset" -> { viewer.reset(); result.success(null) }
                        "dispose" -> { viewers.remove(id); viewer.dispose(); result.success(null) }
                        "stats" -> viewer.stats(result, call.argument<Boolean>("timings") != false)
                        else -> result.notImplemented()
                    }
                }
            } catch (e: Exception) {
                result.error("desktop_video", e.message ?: "Video operation failed", null)
            }
        }
    }

    fun dispose() {
        channel.setMethodCallHandler(null)
        viewers.values.forEach { it.dispose() }
        viewers.clear()
    }

    private data class Packet(val bytes: ByteArray, val generation: Long, val frameId: Long, val sourcePtsUs: Long, val width: Int, val height: Int, val keyframe: Boolean, val revision: Long)
    private data class PendingFrame(val packet: Packet, val queuedNs: Long, var decodedNs: Long = 0, var releasedNs: Long = 0)
    private data class DecodedOutput(val index: Int, val ptsUs: Long, val dueNs: Long)
    private inner class Submission(val packet: Packet, private val reply: MethodChannel.Result, val receivedNs: Long) {
        private val completed = AtomicBoolean()
        fun complete() {
            if (completed.compareAndSet(false, true)) main.post { reply.success(null) }
        }
    }

    // Allocated only by an explicit stats request. Worker-owned accumulators
    // have constant space and never emit per-frame logs.
    private class Timing {
        private var count = 0L
        private var totalNs = 0L
        private var maxNs = 0L
        fun add(ns: Long) {
            val value = ns.coerceAtLeast(0)
            count++
            totalNs += value
            maxNs = maxOf(maxNs, value)
        }
        fun snapshot() = mapOf("samples" to count, "mean_ms" to if (count == 0L) 0.0 else totalNs.toDouble() / count / 1_000_000, "max_ms" to maxNs / 1_000_000.0)
    }

    private class Diagnostics {
        val startedNs = System.nanoTime()
        val inputWait = Timing()
        val decodeCallback = Timing()
        val pacingWait = Timing()
        val readyToRelease = Timing()
        val surfaceConsume = Timing()
        val consumeDispatch = Timing()
        val pumpDispatch = Timing()
        val pumpWork = Timing()
        var peakPending = 0
        var peakOutputs = 0
        fun snapshot() = mapOf(
            "elapsed_ms" to (System.nanoTime() - startedNs) / 1_000_000,
            "input_wait" to inputWait.snapshot(),
            "decode_callback" to decodeCallback.snapshot(),
            "pacing_wait" to pacingWait.snapshot(),
            "ready_to_release" to readyToRelease.snapshot(),
            "surface_consume" to surfaceConsume.snapshot(),
            "consume_dispatch" to consumeDispatch.snapshot(),
            "pump_dispatch" to pumpDispatch.snapshot(),
            "pump_work" to pumpWork.snapshot(),
            "peak_pending" to peakPending,
            "peak_outputs" to peakOutputs,
        )
    }

    private inner class Viewer(private val entry: TextureRegistry.SurfaceTextureEntry) {
        val id = entry.id()
        val revision = AtomicLong(1)
        val submitted = AtomicLong()
        val submittedKeyframes = AtomicLong()
        val decoded = AtomicLong()
        val presented = AtomicLong()
        val dropped = AtomicLong()
        @Volatile var decoderName = ""
        private val thread = HandlerThread("pb-desktop-video").apply { start() }
        private val worker = Handler(thread.looper)
        private val surface = Surface(entry.surfaceTexture())
        private val lock = Any()
        private val queue = ArrayDeque<Submission>()
        private val pending = linkedMapOf<Long, PendingFrame>()
        private val inputs = ArrayDeque<Int>()
        private val outputs = ArrayDeque<DecodedOutput>()
        private var surfacePtsUs: Long? = null
        private var surfaceReleasedAtMs = 0L
        private var lastSourcePtsUs: Long? = null
        private var lastScheduledNs = 0L
        private var waitingOutputs = 0
        private var lastProgress = 0L
        private var codec: MediaCodec? = null
        private var active: Packet? = null
        private var pts = 0L
        private var awaitingKey = true
        private var pumpPosted = false
        private var pumpDueMs = 0L
        private val pumpTask = Runnable {
            synchronized(lock) {
                diagnostics?.pumpDispatch?.add((SystemClock.uptimeMillis() - pumpDueMs) * 1_000_000)
                pumpPosted = false
            }
            pump()
        }
        private var lastAccepted = 0L
        private var acceptedGeneration = -1L
        private val consumedTimestamp = AtomicLong()
        private val consumedAtNs = AtomicLong()
        private val consumePosted = AtomicBoolean()
        private val consumeTask = Runnable { consumeSurface() }
        private val pendingEvents = mutableMapOf<Boolean, Pair<Packet, String>>()
        private var eventPosted = false
        @Volatile private var closed = false
        @Volatile private var diagnostics: Diagnostics? = null

        init {
            // Flutter calls this after consuming SurfaceTexture, not merely after
            // MediaCodec queues output. Its timestamp identifies the actual frame.
            entry.setOnFrameConsumedListener {
                if (closed) return@setOnFrameConsumedListener
                val timestamp = entry.surfaceTexture().timestamp / 1000
                // Flutter may repaint the same texture. It returns one credit
                // only when updateTexImage has consumed a different timestamp.
                if (timestamp == consumedTimestamp.get()) return@setOnFrameConsumedListener
                consumedAtNs.set(if (diagnostics != null) System.nanoTime() else 0)
                consumedTimestamp.set(timestamp)
                if (consumePosted.compareAndSet(false, true)) worker.post(consumeTask)
            }
        }

        private fun consumeSurface() {
            while (!closed) {
                val timestamp = consumedTimestamp.get()
                // A surface has one presentation credit. Only consumption
                // of that exact timestamp may release the next output.
                if (timestamp == surfacePtsUs) {
                    surfacePtsUs = null
                    val frame = pending.remove(timestamp)
                    val packet = frame?.packet
                    if (packet != null && packet.revision == revision.get() && !closed) {
                        diagnostics?.let { metrics ->
                            val consumedNs = consumedAtNs.get()
                            if (consumedNs != 0L) {
                                metrics.consumeDispatch.add(System.nanoTime() - consumedNs)
                                if (frame.releasedNs != 0L) metrics.surfaceConsume.add(consumedNs - frame.releasedNs)
                            }
                        }
                        presented.incrementAndGet()
                        event(packet, true, false)
                    }
                }
                // Clear the scheduled flag only after processing the snapshot,
                // then recheck: a concurrent callback must never lose a credit.
                consumePosted.set(false)
                if (consumedTimestamp.get() == timestamp || !consumePosted.compareAndSet(false, true)) {
                    // We already own the codec worker: do not add another
                    // message-loop turn to the next surface release.
                    pump()
                    return
                }
            }
            consumePosted.set(false)
        }

        fun submit(packet: Packet, reply: MethodChannel.Result) {
            var retry = false
            var queued = false
            synchronized(lock) {
                if (closed || packet.generation < acceptedGeneration) { reply.success(null); return }
                if (packet.generation != acceptedGeneration) {
                    revision.incrementAndGet()
                    clearQueue()
                    acceptedGeneration = packet.generation
                    lastAccepted = 0
                    awaitingKey = true
                }
                if (packet.frameId <= lastAccepted) { reply.success(null); return }
                lastAccepted = packet.frameId
                if (queue.size >= 4) {
                    dropped.addAndGet(queue.size.toLong())
                    clearQueue()
                    revision.incrementAndGet()
                    awaitingKey = true
                    retry = true
                }
                if (awaitingKey && !packet.keyframe) {
                    dropped.incrementAndGet()
                    retry = true
                } else {
                    awaitingKey = false
                    queue.addLast(Submission(packet.copy(revision = revision.get()), reply, if (diagnostics != null) System.nanoTime() else 0))
                    queued = true
                    submitted.incrementAndGet()
                    if (packet.keyframe) submittedKeyframes.incrementAndGet()
                }
                schedulePump(0)
            }
            if (!queued) reply.success(null)
            if (retry) event(packet.copy(revision = revision.get()), false, true)
        }

        fun stats(reply: MethodChannel.Result, timings: Boolean) {
            worker.post {
                if (timings && diagnostics == null) diagnostics = Diagnostics()
                if (!timings) diagnostics = null
                val values = mapOf(
                    "submitted" to submitted.get(), "submittedKeyframes" to submittedKeyframes.get(),
                    "decoded" to decoded.get(), "presented" to presented.get(),
                    "dropped" to dropped.get(), "decoder" to decoderName,
                    "queued" to synchronized(lock) { queue.size },
                    "decoding" to waitingOutputs, "outputs" to outputs.size,
                    "surfacePending" to (surfacePtsUs != null),
                    "timings" to diagnostics?.snapshot(),
                )
                main.post { reply.success(values) }
            }
        }

        // Caller holds lock. Each retired input returns its producer credit.
        private fun clearQueue() {
            queue.forEach { it.complete() }
            queue.clear()
        }

        fun reset() {
            synchronized(lock) {
                revision.incrementAndGet()
                clearQueue()
                awaitingKey = true
                acceptedGeneration = -1
                lastAccepted = 0
            }
            worker.post { releaseCodec() }
        }

        fun dispose() {
            closed = true
            revision.incrementAndGet()
            synchronized(lock) { clearQueue() }
            entry.setOnFrameConsumedListener(null)
            worker.post {
                releaseCodec()
                surface.release()
                main.post { entry.release() }
                thread.quitSafely()
            }
        }

        private fun event(packet: Packet, consumed: Boolean, key: Boolean, message: String = "") {
            check(consumed != key)
            synchronized(lock) {
                pendingEvents[key] = Pair(packet.copy(bytes = ByteArray(0)), message)
                if (eventPosted) return
                eventPosted = true
            }
            main.post {
                val ready = synchronized(lock) {
                    val values = pendingEvents.toMap()
                    pendingEvents.clear()
                    eventPosted = false
                    values
                }
                ready.forEach { (needsKey, value) ->
                    val frame = value.first
                    if (!closed && frame.revision == revision.get()) channel.invokeMethod("event", mapOf("textureId" to id, "generation" to frame.generation, "frameId" to frame.frameId, "width" to frame.width, "height" to frame.height, "presented" to !needsKey, "needKeyframe" to needsKey, "message" to value.second))
                }
            }
        }

        private fun releaseCodec() {
            val old = codec
            codec = null
            active = null
            waitingOutputs = 0
            pending.clear()
            inputs.clear()
            outputs.clear()
            surfacePtsUs = null
            lastSourcePtsUs = null
            try { old?.stop() } catch (_: Exception) {}
            try { old?.release() } catch (_: Exception) {}
        }

        private fun fail(packet: Packet, reason: String) {
            releaseCodec()
            synchronized(lock) {
                if (closed || packet.revision != revision.get()) return
                clearQueue()
                awaitingKey = true
                revision.incrementAndGet()
            }
            event(packet.copy(revision = revision.get()), false, true, reason)
        }

        private fun configure(packet: Packet) {
            // Async flush leaves callbacks carrying invalid buffer indices.
            // Recreate on revision/size changes so old callbacks can be rejected
            // by codec identity without ever returning an obsolete buffer.
            releaseCodec()
            check(packet.keyframe) { "Decoder requires a key frame" }
            entry.surfaceTexture().setDefaultBufferSize(packet.width, packet.height)
            val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, packet.width, packet.height)
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, MAX_BYTES)
            format.setInteger(MediaFormat.KEY_MAX_WIDTH, packet.width)
            format.setInteger(MediaFormat.KEY_MAX_HEIGHT, packet.height)
            if (Build.VERSION.SDK_INT >= 30) format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            parameterSets(packet.bytes).forEach { (type, bytes) -> format.setByteBuffer(if (type == 7) "csd-0" else "csd-1", ByteBuffer.wrap(bytes)) }
            val next = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
            codec = next
            active = packet
            decoderName = next.name
            next.setCallback(object : MediaCodec.Callback() {
                override fun onInputBufferAvailable(c: MediaCodec, index: Int) {
                    if (closed || c !== codec) return
                    inputs.addLast(index)
                    pump()
                }

                override fun onOutputBufferAvailable(c: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
                    if (closed || c !== codec) return
                    try {
                        val frame = pending[info.presentationTimeUs]
                        val render = frame != null && frame.packet.revision == revision.get() && info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG == 0
                        lastProgress = SystemClock.elapsedRealtime()
                        if (frame != null) waitingOutputs = (waitingOutputs - 1).coerceAtLeast(0)
                        if (render) {
                            val output = DecodedOutput(index, info.presentationTimeUs, presentationTime(frame!!.packet))
                            diagnostics?.let { metrics ->
                                frame.decodedNs = System.nanoTime()
                                if (frame.queuedNs != 0L) metrics.decodeCallback.add(frame.decodedNs - frame.queuedNs)
                                metrics.peakOutputs = maxOf(metrics.peakOutputs, outputs.size + 1)
                            }
                            outputs.addLast(output)
                            decoded.incrementAndGet()
                        } else {
                            c.releaseOutputBuffer(index, false)
                            pending.remove(info.presentationTimeUs)
                        }
                        pump()
                    } catch (error: Exception) {
                        codecFailed(error)
                    }
                }

                override fun onOutputFormatChanged(c: MediaCodec, format: MediaFormat) {}

                override fun onError(c: MediaCodec, error: MediaCodec.CodecException) {
                    if (!closed && c === codec) codecFailed(error)
                }
            }, worker)
            next.configure(format, surface, null, 0)
            next.start()
        }

        private fun schedulePump(delay: Long) {
            synchronized(lock) {
                if (closed) return
                val due = SystemClock.uptimeMillis() + delay
                if (pumpPosted && due >= pumpDueMs) return
                if (pumpPosted) worker.removeCallbacks(pumpTask)
                pumpPosted = true
                pumpDueMs = due
                worker.postDelayed(pumpTask, delay)
            }
        }

        private fun presentationTime(packet: Packet): Long {
            val now = System.nanoTime()
            val previous = lastSourcePtsUs
            val step = if (previous == null) 0 else packet.sourcePtsUs - previous
            // Preserve capture cadence across short TCP bursts. Timestamp jumps
            // re-anchor instead of allowing unbounded future or stale playback.
            val candidate = lastScheduledNs + step.coerceIn(0, 80_000) * 1000
            val due = if (previous == null || step !in 1L..80_000L ||
                candidate < now - 25_000_000 || candidate > now + 80_000_000) {
                now + 25_000_000
            } else candidate
            lastSourcePtsUs = packet.sourcePtsUs
            lastScheduledNs = due
            return due
        }

        private fun renderDue(c: MediaCodec) {
            while (outputs.isNotEmpty()) {
                val output = outputs.peekFirst() ?: return
                val frame = pending[output.ptsUs]
                val current = frame != null && frame.packet.revision == revision.get()
                if (current && (surfacePtsUs != null || output.dueNs > System.nanoTime())) return
                outputs.removeFirst()
                // SurfaceTexture does not guarantee SurfaceView's timestamp
                // scheduling. Release here at the deadline, keeping the unique
                // decoder timestamp for the actual Flutter consumption receipt.
                if (current) {
                    surfacePtsUs = output.ptsUs
                    surfaceReleasedAtMs = SystemClock.elapsedRealtime()
                    diagnostics?.let { metrics ->
                        frame!!.releasedNs = System.nanoTime()
                        if (frame.decodedNs != 0L) {
                            metrics.pacingWait.add(minOf(frame.releasedNs, output.dueNs) - frame.decodedNs)
                            // Ready time excludes the intentional capture pacer;
                            // the remainder waits for surface credit or worker.
                            metrics.readyToRelease.add(frame.releasedNs - maxOf(frame.decodedNs, output.dueNs))
                        }
                    }
                    c.releaseOutputBuffer(output.index, true)
                    return
                }
                c.releaseOutputBuffer(output.index, false)
                pending.remove(output.ptsUs)
            }
        }

        private fun codecFailed(error: Exception) {
            Log.w("PbDesktopVideo", "Native H.264 decoder failed", error)
            active?.let { fail(it, "Native H.264 decoding unavailable") }
        }

        private fun pump() {
            if (closed) return
            val metrics = diagnostics
            val startedNs = if (metrics != null) System.nanoTime() else 0
            var processing: Packet? = active
            try {
                codec?.let { renderDue(it) }
                for (attempt in 0 until 4) {
                    val input = synchronized(lock) { queue.peekFirst() } ?: break
                    val packet = input.packet
                    processing = packet
                    if (packet.revision != revision.get()) {
                        synchronized(lock) { if (queue.peekFirst() === input) queue.pollFirst() }
                        input.complete()
                        continue
                    }
                    val current = active
                    if (current == null || current.revision != packet.revision || current.width != packet.width || current.height != packet.height) {
                        configure(packet)
                    }
                    // Every queued access unit can yield an asynchronous output.
                    // Reserve one of the four output slots before submitting it,
                    // rather than accumulating callbacks beyond the queue bound.
                    if (pending.size >= 16 || waitingOutputs + outputs.size >= 4) break
                    val c = codec ?: return
                    val index = inputs.peekFirst() ?: break
                    val taken = synchronized(lock) {
                        if (queue.peekFirst() === input && packet.revision == revision.get()) {
                            queue.pollFirst()
                            true
                        } else false
                    }
                    if (!taken) { input.complete(); continue }
                    inputs.removeFirst()
                    try {
                        val buffer = c.getInputBuffer(index) ?: error("Decoder input unavailable")
                        require(buffer.capacity() >= packet.bytes.size) { "Decoder input is too small" }
                        buffer.clear()
                        buffer.put(packet.bytes)
                        pts += 16667 // Unique decoder timestamp; never replay a network timestamp.
                        val queuedNs = if (metrics != null) System.nanoTime() else 0
                        pending[pts] = PendingFrame(packet.copy(bytes = ByteArray(0)), queuedNs)
                        c.queueInputBuffer(index, 0, packet.bytes.size, pts, 0)
                        metrics?.let {
                            if (input.receivedNs != 0L) it.inputWait.add(queuedNs - input.receivedNs)
                            it.peakPending = maxOf(it.peakPending, pending.size)
                        }
                        if (waitingOutputs == 0) lastProgress = SystemClock.elapsedRealtime()
                        waitingOutputs++
                    } finally {
                        // Return producer credit only once MediaCodec accepted
                        // this input (or the failed input has been retired).
                        input.complete()
                    }
                }
                val nowMs = SystemClock.elapsedRealtime()
                if (surfacePtsUs != null && nowMs - surfaceReleasedAtMs > 2000) {
                    processing?.let { fail(it, "Native video texture was not consumed") }
                    return
                }
                if (waitingOutputs > 0 && nowMs - lastProgress > 2000) {
                    processing?.let { fail(it, "Native video decoder stalled") }
                    return
                }
                // Input/output/consumption callbacks wake us as work arrives.
                // Timers are only for a real presentation deadline or a stall;
                // there is no periodic JNI dequeue or idle two-ms polling.
                var delayMs = Long.MAX_VALUE
                if (surfacePtsUs != null) delayMs = minOf(delayMs, 2001 - (nowMs - surfaceReleasedAtMs))
                if (waitingOutputs > 0) delayMs = minOf(delayMs, 2001 - (nowMs - lastProgress))
                if (surfacePtsUs == null && outputs.isNotEmpty()) {
                    delayMs = minOf(delayMs, (outputs.peekFirst()!!.dueNs - System.nanoTime() + 999_999) / 1_000_000)
                }
                if (delayMs != Long.MAX_VALUE) schedulePump(delayMs.coerceAtLeast(1))
            } catch (error: Exception) {
                Log.w("PbDesktopVideo", "Native H.264 decoder failed", error)
                processing?.let { fail(it, "Native H.264 decoding unavailable") }
            } finally {
                metrics?.pumpWork?.add(System.nanoTime() - startedNs)
            }
        }
    }

    companion object {
        private const val MAX_BYTES = 4 * 1024 * 1024

        // Parameter sets remain owned by the decoder for its lifetime.
        private fun parameterSets(bytes: ByteArray): Map<Int, ByteArray> {
            val starts = mutableListOf<Pair<Int, Int>>()
            var i = 0
            while (i + 3 < bytes.size) {
                val count = if (bytes[i] == 0.toByte() && bytes[i + 1] == 0.toByte() && bytes[i + 2] == 1.toByte()) 3 else if (bytes[i] == 0.toByte() && bytes[i + 1] == 0.toByte() && bytes[i + 2] == 0.toByte() && bytes[i + 3] == 1.toByte()) 4 else 0
                if (count > 0) { starts.add(Pair(i, count)); i += count } else i++
            }
            val result = mutableMapOf<Int, ByteArray>()
            starts.forEachIndexed { index, (start, count) ->
                if (start + count < bytes.size) {
                    val type = bytes[start + count].toInt() and 31
                    if (type == 7 || type == 8) {
                        val end = if (index + 1 < starts.size) starts[index + 1].first else bytes.size
                        result[type] = byteArrayOf(0, 0, 0, 1) + bytes.copyOfRange(start + count, end)
                    }
                }
            }
            require(result.size == 2) { "Key frame must include H.264 SPS and PPS" }
            return result
        }
    }
}
