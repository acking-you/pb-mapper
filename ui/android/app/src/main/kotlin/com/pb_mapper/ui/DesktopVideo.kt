package com.pb_mapper.ui

import android.graphics.Bitmap
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import android.util.Log
import android.view.PixelCopy
import android.view.Surface
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel
import io.flutter.view.TextureRegistry
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.util.ArrayDeque
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * Native H.264 into a Flutter texture. Access units come from Rust on a
 * feeder thread and never cross Dart; presentation receipts and keyframe
 * requests go straight back to Rust. The channel only creates, binds,
 * snapshots and tears down textures.
 */
@Suppress("DEPRECATION")
class DesktopVideo(messenger: BinaryMessenger, private val textures: TextureRegistry) {
    private val main = Handler(Looper.getMainLooper())
    private val channel = MethodChannel(messenger, "pb_mapper/desktop_video")
    private val viewers = ConcurrentHashMap<Long, Viewer>()

    init {
        channel.setMethodCallHandler { call, result ->
            try {
                if (call.method == "create") {
                    checkInstance(call.argument<Number>("instance")?.toLong() ?: error("Missing video library instance"))
                    check(viewers.size < 2) { "Desktop texture limit reached" }
                    val viewer = Viewer(
                        textures.createSurfaceTexture(),
                        DesktopVideoNative.api,
                        call.argument<Boolean>("decoderHints") != false,
                    )
                    viewers[viewer.id] = viewer
                    result.success(viewer.id)
                } else {
                    val id = call.argument<Number>("textureId")?.toLong() ?: error("Missing desktop texture")
                    val viewer = viewers[id] ?: error("Unknown desktop texture")
                    when (call.method) {
                        "attach" -> {
                            viewer.attach(call.argument<Number>("source")?.toLong() ?: error("Missing video source"))
                            result.success(null)
                        }
                        "snapshot" -> {
                            val path = call.argument<String>("path")?.takeIf { it.isNotEmpty() } ?: error("Missing snapshot path")
                            val maxWidth = call.argument<Number>("maxWidth")?.toInt() ?: SNAPSHOT_WIDTH
                            require(maxWidth > 0) { "Invalid snapshot width" }
                            viewer.snapshot(path, maxWidth) { result.success(it) }
                        }
                        "stats" -> viewer.stats(result, call.argument<Boolean>("timings") != false)
                        "dispose" -> { viewers.remove(id); viewer.dispose(); result.success(null) }
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

    // D9: Dart's dlopen and this System.loadLibrary must reach one library,
    // or the feeder would pull from a registry Dart never filled. Fails closed.
    private fun checkInstance(dart: Long) {
        val kotlin = try {
            DesktopVideoNative.instanceToken()
        } catch (e: LinkageError) {
            error("pb_mapper_ffi is not loaded: ${e.message}")
        }
        Log.d(TAG, "Video library instance: Dart 0x${java.lang.Long.toHexString(dart)}, Kotlin 0x${java.lang.Long.toHexString(kotlin)}")
        check(kotlin == dart) { "native video library loaded twice" }
    }

    private class PendingFrame(
        val generation: Long,
        val frameId: Long,
        val revision: Long,
        val receivedNs: Long,
        val queuedNs: Long,
    ) {
        var decodedNs = 0L
        var releasedNs = 0L
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
        val readyToRelease = Timing()
        val receiveToPresent = Timing()
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
            "ready_to_release" to readyToRelease.snapshot(),
            "receive_to_present" to receiveToPresent.snapshot(),
            "surface_consume" to surfaceConsume.snapshot(),
            "consume_dispatch" to consumeDispatch.snapshot(),
            "pump_dispatch" to pumpDispatch.snapshot(),
            "pump_work" to pumpWork.snapshot(),
            "peak_pending" to peakPending,
            "peak_outputs" to peakOutputs,
        )
    }

    private inner class Viewer(
        private val entry: TextureRegistry.SurfaceTextureEntry,
        private val api: VideoSourceApi,
        decoderHints: Boolean,
    ) : FeedTarget {
        val id = entry.id()
        val submitted = AtomicLong()
        val submittedKeyframes = AtomicLong()
        val decoded = AtomicLong()
        val presented = AtomicLong()
        val dropped = AtomicLong()
        val configurations = AtomicLong()
        val recoveries = AtomicLong()
        @Volatile private var lastRecovery = ""
        @Volatile var decoderName = ""
        private val pool = BufferPool()
        private val feed = FeedQueue(pool)
        // Main thread only; the feeder of the attached source.
        private var feeder: VideoFeeder? = null
        @Volatile private var consumer = 0L
        @Volatile private var paused = false
        @Volatile private var resumedAtMs = 0L
        private val thread = HandlerThread("pb-desktop-video", Process.THREAD_PRIORITY_DISPLAY).apply { start() }
        private val worker = Handler(thread.looper)
        private val surface = Surface(entry.surfaceTexture())
        private val lock = Any()
        private val pending = linkedMapOf<Long, PendingFrame>()
        private val inputs = ArrayDeque<Int>()
        private val outputs = VideoPresentationQueue()
        private var surfacePtsUs: Long? = null
        private var surfaceReleasedAtMs = 0L
        private var waitingOutputs = 0
        private var lastProgress = 0L
        private var codec: MediaCodec? = null
        // KEY_LOW_LATENCY goes into configure while this holds. On AOSP's OMX
        // path a decoder that rejects it fails configure (ACodec::setLowLatency),
        // so configure retries the same decoder without it and keeps it off.
        private var lowLatency = decoderHints && Build.VERSION.SDK_INT >= 30
        private var active: FrameSize? = null
        // The size of the picture last released to the surface, which is what
        // a snapshot reads. It outlives the codec that drew it.
        private var lastPicture: FrameSize? = null
        private var lastPresentedGeneration = -1L
        private var lastPresentedFrameId = 0L
        private var pts = 0L
        private var pumpPosted = false
        private var pumpDueMs = 0L
        private val pumpTask = Runnable {
            synchronized(lock) {
                diagnostics?.pumpDispatch?.add((SystemClock.uptimeMillis() - pumpDueMs) * 1_000_000)
                pumpPosted = false
            }
            pump()
        }
        private val consumedTimestamp = AtomicLong()
        private val consumedAtNs = AtomicLong()
        private val consumePosted = AtomicBoolean()
        private val consumeTask = Runnable { consumeSurface() }
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

        /** Binds a new source. The texture and its last picture stay until the new session's IDR replaces them. */
        fun attach(source: Long) {
            stopFeeder()
            feed.reset()
            paused = false
            resumedAtMs = SystemClock.elapsedRealtime()
            val next = api.attach(source)
            check(next > 0) { "Video source unavailable ($next)" }
            consumer = next
            feeder = VideoFeeder(api, next, this, pool).apply { start() }
        }

        private fun stopFeeder() {
            feeder?.close()
            feeder = null
        }

        // FeedTarget, on the feeder thread.
        override fun awaitRoom(timeoutMs: Long) = feed.awaitRoom(timeoutMs)

        override fun offer(unit: FeedUnit) {
            when (feed.offer(unit)) {
                FeedQueue.Admission.ENQUEUED -> {
                    submitted.incrementAndGet()
                    if (unit.keyframe) submittedKeyframes.incrementAndGet()
                    schedulePump(0)
                }
                FeedQueue.Admission.GAP -> api.keyframe(consumer, DesktopVideoNative.REASON_GAP)
                FeedQueue.Admission.DROPPED -> Unit
            }
        }

        override fun setPaused(paused: Boolean) {
            if (this.paused == paused) return
            // A hidden texture is not consumed and a paused source sends
            // nothing: the stall clocks restart when the video does.
            if (!paused) resumedAtMs = SystemClock.elapsedRealtime()
            this.paused = paused
        }

        override fun failed(message: String) {
            Log.w(TAG, message)
            lastRecovery = message
        }

        private fun consumeSurface() {
            while (!closed) {
                val timestamp = consumedTimestamp.get()
                // A surface has one presentation credit. Only consumption
                // of that exact timestamp may release the next output.
                if (timestamp == surfacePtsUs) {
                    surfacePtsUs = null
                    val frame = pending.remove(timestamp)
                    if (frame != null && frame.revision == feed.revision && !closed) {
                        diagnostics?.let { metrics ->
                            val consumedNs = consumedAtNs.get()
                            if (consumedNs != 0L) {
                                metrics.consumeDispatch.add(System.nanoTime() - consumedNs)
                                if (frame.releasedNs != 0L) metrics.surfaceConsume.add(consumedNs - frame.releasedNs)
                                metrics.receiveToPresent.add(consumedNs - frame.receivedNs)
                            }
                        }
                        lastPresentedGeneration = frame.generation
                        lastPresentedFrameId = frame.frameId
                        presented.incrementAndGet()
                        // Consumed by the compositor: the receipt Rust validates
                        // and forwards for input authorisation.
                        api.presented(consumer, frame.generation, frame.frameId)
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

        fun stats(reply: MethodChannel.Result, timings: Boolean) {
            worker.post {
                if (timings && diagnostics == null) diagnostics = Diagnostics()
                if (!timings) diagnostics = null
                val values = mapOf(
                    "submitted" to submitted.get(), "submittedKeyframes" to submittedKeyframes.get(),
                    "decoded" to decoded.get(), "presented" to presented.get(),
                    "lastPresentedGeneration" to lastPresentedGeneration,
                    "lastPresentedFrameId" to lastPresentedFrameId,
                    "dropped" to dropped.get() + feed.dropped.get(), "decoder" to decoderName,
                    "configurations" to configurations.get(), "lowLatency" to lowLatency,
                    "recoveries" to recoveries.get(), "lastRecovery" to lastRecovery,
                    "queued" to feed.size,
                    "decoding" to waitingOutputs, "outputs" to outputs.size,
                    "surfacePending" to (surfacePtsUs != null),
                    "timings" to diagnostics?.snapshot(),
                )
                main.post { reply.success(values) }
            }
        }

        /**
         * D23: the picture last released to the texture, as a JPEG at most
         * [maxWidth] wide, written beside [path] and renamed over it. Replies
         * on the main thread. It replies false with no picture, when PixelCopy
         * cannot read the surface (R6), or once [SNAPSHOT_TIMEOUT_MS] pass, and
         * a late picture never replaces [path]. Dispose never waits for it.
         */
        fun snapshot(path: String, maxWidth: Int, reply: (Boolean) -> Unit) {
            val settled = AtomicBoolean()
            val expire = Runnable { if (settled.compareAndSet(false, true)) reply(false) }
            fun settle(ok: Boolean) {
                if (!settled.compareAndSet(false, true)) return
                main.removeCallbacks(expire)
                main.post { reply(ok) }
            }
            main.postDelayed(expire, SNAPSHOT_TIMEOUT_MS)
            val posted = worker.post {
                val size = lastPicture
                if (closed || size == null) return@post settle(false)
                val width = minOf(maxWidth, size.width)
                val height = ((size.height.toLong() * width + size.width / 2) / size.width).toInt().coerceAtLeast(1)
                val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
                val temporary = File("$path.tmp")
                try {
                    // Scales the surface's last queued buffer into the bitmap.
                    PixelCopy.request(surface, bitmap, { copied ->
                        val written = copied == PixelCopy.SUCCESS && writeJpeg(bitmap, temporary)
                        bitmap.recycle()
                        // Claim the reply before the rename: past the deadline
                        // Dart already has false, and the old picture stays.
                        if (written && settled.compareAndSet(false, true)) {
                            main.removeCallbacks(expire)
                            val moved = temporary.renameTo(File(path))
                            if (!moved) temporary.delete()
                            main.post { reply(moved) }
                        } else {
                            temporary.delete()
                            settle(false)
                        }
                    }, worker)
                } catch (e: IllegalArgumentException) {
                    Log.w(TAG, "Cannot snapshot the desktop video surface", e)
                    bitmap.recycle()
                    settle(false)
                }
            }
            // The worker has quit, and the texture is gone with it.
            if (!posted) settle(false)
        }

        fun dispose() {
            stopFeeder()
            closed = true
            feed.close()
            entry.setOnFrameConsumedListener(null)
            worker.post {
                releaseCodec()
                surface.release()
                main.post { entry.release() }
                thread.quitSafely()
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
            try { old?.stop() } catch (_: Exception) {}
            try { old?.release() } catch (_: Exception) {}
        }

        private fun fail(message: String, ask: () -> Unit) {
            releaseCodec()
            if (closed) return
            // The next keyframe configures a new codec in a new revision.
            feed.discard()
            recoveries.incrementAndGet()
            lastRecovery = message
            // Rust rate-limits requests. A paused source asks for nothing:
            // its resume already starts with an IDR.
            if (!paused) ask()
        }

        private fun stalled(message: String) =
            fail(message) { api.keyframe(consumer, DesktopVideoNative.REASON_STALL) }

        private fun configure(unit: FeedUnit, parameterSets: Map<Int, ByteArray>) {
            // Async flush leaves callbacks carrying invalid buffer indices.
            // Recreate on a size change so old callbacks can be rejected by
            // codec identity without ever returning an obsolete buffer.
            releaseCodec()
            entry.surfaceTexture().setDefaultBufferSize(unit.width, unit.height)
            var next = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
            decoderName = next.name
            try {
                start(next, unit, parameterSets, lowLatency)
            } catch (e: Exception) {
                try { next.release() } catch (_: Exception) {}
                if (!lowLatency) throw e
                Log.w(TAG, "$decoderName rejected the low-latency hint", e)
                next = MediaCodec.createByCodecName(decoderName)
                try {
                    start(next, unit, parameterSets, false)
                } catch (retry: Exception) {
                    try { next.release() } catch (_: Exception) {}
                    throw retry
                }
                lowLatency = false
            }
            // Callbacks reach this worker only after configure returns.
            codec = next
            active = FrameSize(unit.width, unit.height)
            configurations.incrementAndGet()
            api.report(consumer, next.name, hardware(next))
        }

        private fun start(c: MediaCodec, unit: FeedUnit, parameterSets: Map<Int, ByteArray>, hinted: Boolean) {
            val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, unit.width, unit.height)
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, MAX_BYTES)
            format.setInteger(MediaFormat.KEY_MAX_WIDTH, unit.width)
            format.setInteger(MediaFormat.KEY_MAX_HEIGHT, unit.height)
            if (hinted) format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            // We own dropping before the surface and keep exactly one released
            // frame in flight. A driver silently dropping that sole frame would
            // strand the consumption credit and look like a frozen renderer.
            if (Build.VERSION.SDK_INT >= 29) format.setInteger(MediaFormat.KEY_ALLOW_FRAME_DROP, 0)
            parameterSets.forEach { (type, bytes) -> format.setByteBuffer(if (type == 7) "csd-0" else "csd-1", ByteBuffer.wrap(bytes)) }
            c.setCallback(callback, worker)
            c.configure(format, surface, null, 0)
            c.start()
        }

        private val callback = object : MediaCodec.Callback() {
            override fun onInputBufferAvailable(c: MediaCodec, index: Int) {
                if (closed || c !== codec) return
                inputs.addLast(index)
                pump()
            }

            override fun onOutputBufferAvailable(c: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
                if (closed || c !== codec) return
                try {
                    val frame = pending[info.presentationTimeUs]
                    lastProgress = SystemClock.elapsedRealtime()
                    if (frame != null) waitingOutputs = (waitingOutputs - 1).coerceAtLeast(0)
                    if (frame != null && frame.revision == feed.revision && info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG == 0) {
                        diagnostics?.let { metrics ->
                            frame.decodedNs = System.nanoTime()
                            if (frame.queuedNs != 0L) metrics.decodeCallback.add(frame.decodedNs - frame.queuedNs)
                            metrics.peakOutputs = maxOf(metrics.peakOutputs, outputs.size + 1)
                        }
                        outputs.enqueue(index, info.presentationTimeUs, frame.revision, frame.frameId, System.nanoTime())
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

        private fun renderLatest(c: MediaCodec) {
            while (true) {
                val output = outputs.take(feed.revision, surfacePtsUs != null) ?: return
                val frame = pending[output.ptsUs]
                // Release the newest decoded picture as soon as the surface is
                // available. The unique decoder timestamp still identifies its
                // actual Flutter consumption receipt; queuing is not display.
                if (output.render && frame != null && frame.revision == feed.revision) {
                    surfacePtsUs = output.ptsUs
                    surfaceReleasedAtMs = SystemClock.elapsedRealtime()
                    lastPicture = active
                    diagnostics?.let { metrics ->
                        frame.releasedNs = System.nanoTime()
                        metrics.readyToRelease.add(frame.releasedNs - output.readyNs)
                    }
                    c.releaseOutputBuffer(output.index, true)
                    return
                }
                c.releaseOutputBuffer(output.index, false)
                pending.remove(output.ptsUs)
                dropped.incrementAndGet()
            }
        }

        // Rust logs the text, counts it toward giving up, and ends the
        // session's message with it.
        private fun codecFailed(error: Exception) {
            Log.w(TAG, "Native H.264 decoder failed", error)
            val message = error.toString()
            fail(message) { api.failed(consumer, message) }
        }

        private fun pump() {
            if (closed) return
            val metrics = diagnostics
            val startedNs = if (metrics != null) System.nanoTime() else 0
            try {
                codec?.let { renderLatest(it) }
                for (attempt in 0 until 4) {
                    val unit = feed.head() ?: break
                    when (DecoderRules.step(unit, feed.revision, active)) {
                        DecoderRules.Step.DISCARD -> {
                            if (feed.poll(unit)) feed.release(unit)
                            continue
                        }
                        DecoderRules.Step.CONFIGURE -> {
                            check(unit.keyframe) { "Decoder requires a key frame" }
                            val sets = feed.read(unit) { parameterSets(it.buffer, it.len) } ?: continue
                            configure(unit, sets)
                        }
                        DecoderRules.Step.FEED -> Unit
                    }
                    // Every queued access unit can yield an asynchronous output.
                    // Reserve one of the four output slots before submitting it,
                    // rather than accumulating callbacks beyond the queue bound.
                    if (pending.size >= 16 || waitingOutputs + outputs.size >= 4) break
                    val c = codec ?: return
                    val index = inputs.peekFirst() ?: break
                    if (!feed.poll(unit)) continue
                    inputs.removeFirst()
                    try {
                        val buffer = c.getInputBuffer(index) ?: error("Decoder input unavailable")
                        require(buffer.capacity() >= unit.len) { "Decoder input is too small" }
                        buffer.span(0, buffer.capacity())
                        unit.buffer.span(0, unit.len)
                        buffer.put(unit.buffer)
                        pts += 16667 // Unique decoder timestamp; never replay a network timestamp.
                        val queuedNs = if (metrics != null) System.nanoTime() else 0
                        pending[pts] = PendingFrame(unit.generation, unit.frameId, unit.revision, unit.receivedNs, queuedNs)
                        c.queueInputBuffer(index, 0, unit.len, pts, 0)
                        metrics?.let {
                            it.inputWait.add(queuedNs - unit.receivedNs)
                            it.peakPending = maxOf(it.peakPending, pending.size)
                        }
                        if (waitingOutputs == 0) lastProgress = SystemClock.elapsedRealtime()
                        waitingOutputs++
                    } finally {
                        // The pooled buffer returns once MediaCodec copied it,
                        // and the feeder may pull the next unit.
                        feed.release(unit)
                    }
                }
                // A paused source sends nothing and its texture is hidden, so
                // neither clock means a stall; both restart on resume.
                if (paused) return
                val nowMs = SystemClock.elapsedRealtime()
                val resumed = resumedAtMs
                if (surfacePtsUs != null && nowMs - maxOf(surfaceReleasedAtMs, resumed) > 2000) {
                    stalled("Native video texture was not consumed")
                    return
                }
                if (waitingOutputs > 0 && nowMs - maxOf(lastProgress, resumed) > 2000) {
                    stalled("Native video decoder stalled")
                    return
                }
                // Input/output/consumption callbacks wake us as work arrives.
                // Timers detect stalls only. The renderer never schedules old
                // pictures for later playback or polls an idle decoder.
                var delayMs = Long.MAX_VALUE
                if (surfacePtsUs != null) delayMs = minOf(delayMs, 2001 - (nowMs - maxOf(surfaceReleasedAtMs, resumed)))
                if (waitingOutputs > 0) delayMs = minOf(delayMs, 2001 - (nowMs - maxOf(lastProgress, resumed)))
                if (delayMs != Long.MAX_VALUE) schedulePump(delayMs.coerceAtLeast(1))
            } catch (error: Exception) {
                codecFailed(error)
            } finally {
                metrics?.pumpWork?.add(System.nanoTime() - startedNs)
            }
        }
    }

    companion object {
        private const val TAG = "PbDesktopVideo"
        private const val MAX_BYTES = 4 * 1024 * 1024
        private const val SNAPSHOT_WIDTH = 320
        private const val SNAPSHOT_QUALITY = 80
        private const val SNAPSHOT_TIMEOUT_MS = 300L

        // JPEG q80, flushed to disk before the caller renames it into place.
        private fun writeJpeg(bitmap: Bitmap, file: File): Boolean = try {
            file.parentFile?.mkdirs()
            FileOutputStream(file).use { out ->
                bitmap.compress(Bitmap.CompressFormat.JPEG, SNAPSHOT_QUALITY, out).also { compressed ->
                    if (compressed) {
                        out.flush()
                        out.fd.sync()
                    }
                }
            }
        } catch (e: IOException) {
            Log.w(TAG, "Cannot write the desktop snapshot", e)
            false
        }

        private fun hardware(codec: MediaCodec): Boolean =
            if (Build.VERSION.SDK_INT >= 29) {
                !codec.codecInfo.isSoftwareOnly
            } else {
                // Before API 29, the platform's software decoders are named.
                !codec.name.startsWith("OMX.google.") && !codec.name.startsWith("c2.android.")
            }

        // A key frame leads with its SPS (7) and PPS (8); the scan stops once
        // both are found. They stay owned by the decoder for its lifetime.
        private fun parameterSets(buffer: ByteBuffer, len: Int): Map<Int, ByteArray> {
            fun zero(at: Int) = buffer.get(at) == 0.toByte()
            fun startCode(at: Int) = when {
                zero(at) && zero(at + 1) && buffer.get(at + 2) == 1.toByte() -> 3
                at + 3 < len && zero(at) && zero(at + 1) && zero(at + 2) && buffer.get(at + 3) == 1.toByte() -> 4
                else -> 0
            }
            val result = mutableMapOf<Int, ByteArray>()
            var nal = -1
            fun finish(end: Int) {
                if (nal < 0 || nal >= end) return
                val type = buffer.get(nal).toInt() and 31
                if (type != 7 && type != 8) return
                val bytes = ByteArray(4 + end - nal)
                bytes[3] = 1
                val view = buffer.duplicate()
                view.span(nal, end)
                view.get(bytes, 4, end - nal)
                result[type] = bytes
            }
            var i = 0
            while (i + 3 < len && result.size < 2) {
                val count = startCode(i)
                if (count == 0) { i++; continue }
                finish(i)
                i += count
                nal = i
            }
            if (result.size < 2) finish(len)
            require(result.size == 2) { "Key frame must include H.264 SPS and PPS" }
            return result
        }
    }
}

internal data class FrameSize(val width: Int, val height: Int)

/** The codec thread's decisions about the head unit, pure so the JVM tests hold them. */
internal object DecoderRules {
    enum class Step { DISCARD, CONFIGURE, FEED }

    /**
     * A unit of an older revision is discarded. Otherwise a codec is configured
     * only when there is none ([configured] null, also after a failure released
     * it) or the size changed. A discontinuity bumps the revision but keeps the
     * size: its IDR goes to the running codec, whose in-band SPS/PPS resets the
     * references (R18).
     */
    fun step(unit: FeedUnit, revision: Long, configured: FrameSize?): Step = when {
        unit.revision != revision -> Step.DISCARD
        configured == null || configured.width != unit.width || configured.height != unit.height -> Step.CONFIGURE
        else -> Step.FEED
    }
}
