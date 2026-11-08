package io.flutter.embedding.engine.renderer

/**
 * Asks the engine for one more frame, the request a SurfaceTexture's
 * frame-available callback makes. Main thread only, while the engine is
 * attached.
 *
 * Flutter folds every request made before a frame begins into that frame, and
 * the frame's updateTexImage latches one buffer, the oldest. A second picture
 * released before that frame began therefore waits in the queue until
 * something else asks for a frame. The desktop decoder asks here when a
 * consumption leaves a picture queued. FlutterRenderer has no public way to
 * ask, so this file sits in its package to reach the package-private method
 * its texture entries use: a Flutter upgrade that changes it fails the build,
 * never a running app.
 */
internal fun FlutterRenderer.requestTextureFrame() = scheduleEngineFrame()
