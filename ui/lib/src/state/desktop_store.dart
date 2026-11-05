import 'dart:async';
import 'package:flutter/foundation.dart';
import '../ffi/desktop_api.dart';
import '../ffi/desktop_video.dart';

enum DesktopConnectionPhase {
  idle,
  connecting,
  live,
  delayed,
  waitingForDesktop,
  recoveringVideo,
  retryWaiting,
  reconnecting,
  identityMismatch,
}

/// Owns the session independently of navigation. Only the newest frame is kept,
/// and each input carries the geometry of the frame actually rendered by UI.
class DesktopStore extends ChangeNotifier {
  DesktopStore(this.api, {DateTime Function()? now, this.onRestartReady})
    : _now = now ?? DateTime.now,
      _presentSample = (now ?? DateTime.now)();
  final DateTime Function() _now;
  final DesktopApiClient api;
  final Future<void> Function()? onRestartReady;
  bool restarting = false;
  DesktopCapabilities capabilities = const DesktopCapabilities();
  DesktopTrust trust = const DesktopTrust();
  bool _trustRestored = false;
  Timer? _hostResumeTimer;
  int _hostResumeSerial = 0, _hostResumeAttempt = 0;
  DesktopSnapshot snapshot = const DesktopSnapshot();
  DesktopFrame? frame;
  DesktopVideoTexture? videoTexture;
  final videoPresentation = ValueNotifier<DesktopStatus?>(null);
  final cursorStatus = ValueNotifier<DesktopStatus?>(null);
  DesktopStatus? get videoPresented => videoPresentation.value;
  set videoPresented(DesktopStatus? status) => videoPresentation.value = status;
  StreamIterator<DesktopFrame>? _frames;
  StreamSubscription<DesktopVideoEvent>? _videoEvents;
  final _videoStatuses = <int, (DesktopStatus, DateTime)>{};
  int _videoGeneration = -1;
  int _mediaEpoch = 0;
  int get mediaEpoch => _mediaEpoch;
  bool _current(int epoch) => !_disposed && epoch == _mediaEpoch;
  Future<void>? _disconnecting;
  Future<void>? _feedback;
  Timer? _presentationTimer;
  DesktopStatus? _pendingPresentation;
  bool _pendingKeyframe = false, _mediaFailed = false;
  DateTime _presentSample;
  int _presentCount = 0;
  double _presentFps = 0;
  double get presentFps =>
      _videoPresentedAt != null &&
          _now().difference(_videoPresentedAt!) < const Duration(seconds: 2)
      ? _presentFps
      : 0;
  DateTime? _videoPresentedAt;
  DateTime _lastAck = DateTime.fromMillisecondsSinceEpoch(0);
  DateTime _lastMediaNotify = DateTime.fromMillisecondsSinceEpoch(0);
  String? invitation;
  String? error;
  (int, String)? _videoError;
  bool busy = false;
  bool _disposed = false;
  bool _polling = false;
  Timer? _timer;
  int _sequence = 0;
  DateTime? _frameAt;
  int _inFlight = 0;
  int _inputEpoch = 0;
  Future<void> _inputTail = Future.value();
  _PendingMove? _pendingMove;
  bool _endingControl = false;
  bool _releasing = false;
  (String, bool)? _target;
  int _sessionSerial = 0;
  int? _attemptSerial;
  bool get _attemptRunning => _attemptSerial == _sessionSerial;
  bool _controlWanted = false, _viewerForeground = true;
  bool _restoringControl = false;
  Timer? _retryTimer;
  DateTime? _retryAt, _recoveryStarted, _lastKeyframeRequest, _stableSince;
  DateTime? _controlRequestedAt;
  int reconnectAttempt = 0;
  String? recoveryReason;
  bool _identityMismatch = false;
  Duration? get pictureAge {
    final at = _videoPresentedAt ?? _frameAt;
    return at == null ? null : _now().difference(at);
  }

  Duration get retryIn => _retryAt == null
      ? Duration.zero
      : (_retryAt!.difference(_now()) > Duration.zero
            ? _retryAt!.difference(_now())
            : Duration.zero);
  DesktopConnectionPhase get connectionPhase {
    if (_identityMismatch) return DesktopConnectionPhase.identityMismatch;
    if (_retryTimer != null) return DesktopConnectionPhase.retryWaiting;
    if (_attemptRunning) {
      return reconnectAttempt == 0
          ? DesktopConnectionPhase.connecting
          : DesktopConnectionPhase.reconnecting;
    }
    if (snapshot.viewer?.state == 'capture_unavailable') {
      return DesktopConnectionPhase.waitingForDesktop;
    }
    if (!connected) return DesktopConnectionPhase.idle;
    if (fresh) {
      return snapshot.viewer!.frameAckMs > 500
          ? DesktopConnectionPhase.delayed
          : DesktopConnectionPhase.live;
    }
    return _recoveryStarted != null
        ? DesktopConnectionPhase.recoveringVideo
        : DesktopConnectionPhase.connecting;
  }

  bool get sharing =>
      snapshot.host != null || snapshot.background?.enabled == true;
  bool get connected => _target != null || snapshot.viewer != null;
  bool get fresh =>
      frame != null &&
      frame!.bytes.isNotEmpty &&
      _frameAt != null &&
      _now().difference(_frameAt!) < const Duration(seconds: 2) &&
      (frame!.status.codec != 'h264' ||
          (videoPresented?.generation == frame!.status.generation &&
              _videoPresentedAt != null &&
              _now().difference(_videoPresentedAt!) <
                  const Duration(seconds: 2))) &&
      snapshot.viewer?.streaming == true;
  bool get controlling =>
      fresh &&
      snapshot.viewer?.controlling == true &&
      !_endingControl &&
      !_releasing;

  Future<void> load() async {
    await run(() async {
      capabilities = await api.capabilities();
      trust = await api.trustStatus();
      await _loadBackground();
    });
    if (_disposed) return;
    _timer ??= Timer.periodic(const Duration(milliseconds: 250), (_) => poll());
    await poll();
  }

  Future<void> restoreOnStartup() async {
    if (_trustRestored || _disposed) return;
    _trustRestored = true;
    await _restoreStartupHost(_hostResumeSerial);
  }

  bool _hostResumeCurrent(int serial) =>
      !_disposed && serial == _hostResumeSerial;

  Future<void> _restoreStartupHost(int serial) async {
    if (!_hostResumeCurrent(serial)) return;
    try {
      // A restarting helper must not prevent discovering the durable handoff
      // that asks us to retry it. Read the local intent before service IPC.
      final restoredTrust = await api.trustStatus();
      if (!_hostResumeCurrent(serial)) return;
      trust = restoredTrust;
      if (trust.saved?.background == true &&
          trust.saved?.autoStart == true &&
          trust.matchesRelay) {
        await _resumePendingHost(serial);
        return;
      }
      await _loadBackground(hostSerial: serial);
      if (!_hostResumeCurrent(serial)) return;
      if (snapshot.background?.configured != true &&
          trust.saved?.autoStart == true &&
          trust.matchesRelay) {
        await restoreHost();
        if (_hostResumeCurrent(serial)) {
          _timer ??= Timer.periodic(
            const Duration(milliseconds: 250),
            (_) => poll(),
          );
        }
      }
      if (snapshot.background?.configured == true &&
          _hostResumeCurrent(serial)) {
        _timer ??= Timer.periodic(
          const Duration(milliseconds: 250),
          (_) => poll(),
        );
        await poll();
      }
    } catch (e) {
      if (!_hostResumeCurrent(serial)) return;
      error = e.toString();
      _scheduleHostResume(serial, _restoreStartupHost);
      _notify();
    }
  }

  Future<void> _loadBackground({int? hostSerial}) async {
    final serial = hostSerial ?? _hostResumeSerial;
    final background = await api.backgroundAction('status');
    if (!_hostResumeCurrent(serial)) return;
    snapshot = snapshot.withBackground(background);
    if (background.configured) invitation = background.invitation;
  }

  Future<void> backgroundAction(String action) async {
    await run(() async {
      if (action != 'status') _cancelHostResume();
      final serial = _hostResumeSerial;
      final background = await api.backgroundAction(action);
      if (!_hostResumeCurrent(serial)) return;
      snapshot = snapshot.withBackground(background);
      invitation = background.invitation;
    });
    await poll();
  }

  Future<void> _resumePendingHost(int serial) async {
    if (!_hostResumeCurrent(serial)) return;
    try {
      final restoredTrust = await api.trustStatus();
      if (!_hostResumeCurrent(serial)) return;
      trust = restoredTrust;
      if (trust.saved?.background != true ||
          trust.saved?.autoStart != true ||
          !trust.matchesRelay) {
        // A previous attempt may have configured the service successfully and
        // then lost its status reply. Discover that owner without re-enabling.
        await _restoreStartupHost(serial);
        return;
      }
      final restoredInvitation = await api.restoreHost();
      if (!_hostResumeCurrent(serial)) return;
      invitation = restoredInvitation;
      _hostResumeAttempt = 0;
      error = null;
      await _loadBackground(hostSerial: serial);
      if (!_hostResumeCurrent(serial)) return;
      final currentTrust = await api.trustStatus();
      if (!_hostResumeCurrent(serial)) return;
      trust = currentTrust;
      _timer ??= Timer.periodic(
        const Duration(milliseconds: 250),
        (_) => poll(),
      );
      await poll();
    } catch (e) {
      if (!_hostResumeCurrent(serial)) return;
      error = e.toString();
      _scheduleHostResume(serial, _resumePendingHost);
    }
    _notify();
  }

  void _scheduleHostResume(int serial, Future<void> Function(int) retry) {
    if (!_hostResumeCurrent(serial)) return;
    _hostResumeTimer?.cancel();
    _hostResumeTimer = Timer(
      Duration(seconds: (1 << _hostResumeAttempt.clamp(0, 4)).clamp(1, 15)),
      () {
        _hostResumeTimer = null;
        _hostResumeAttempt++;
        unawaited(retry(serial));
      },
    );
  }

  void _cancelHostResume() {
    _hostResumeSerial++;
    _hostResumeTimer?.cancel();
    _hostResumeTimer = null;
    _hostResumeAttempt = 0;
  }

  Future<void> restoreHost() async {
    await run(() async {
      final serial = _hostResumeSerial;
      final restoredInvitation = await api.restoreHost();
      if (!_hostResumeCurrent(serial)) return;
      invitation = restoredInvitation;
      final restoredTrust = await api.trustStatus();
      if (!_hostResumeCurrent(serial)) return;
      trust = restoredTrust;
    });
    await poll();
  }

  Future<void> revokeTrust() async {
    _cancelHostResume();
    final serial = _hostResumeSerial;
    try {
      // Native revocation serializes with pending setup and checks the actual
      // service owner. This snapshot can lag a completed background handoff.
      await api.revokeTrust();
      if (!_hostResumeCurrent(serial)) return;
      invitation = null;
      final revokedTrust = await api.trustStatus();
      if (!_hostResumeCurrent(serial)) return;
      trust = revokedTrust;
      snapshot = snapshot.withHost(null);
      error = null;
    } catch (e) {
      if (_hostResumeCurrent(serial)) error = e.toString();
    }
    _notify();
    await poll();
  }

  Future<void> poll() async {
    if (_polling || _disposed || _disconnecting != null) return;
    if (busy || _attemptRunning || _retryTimer != null) {
      _notify();
      return;
    }
    _polling = true;
    final epoch = _mediaEpoch;
    try {
      final status = await api.status();
      if (!_current(epoch)) return;
      snapshot = status;
      if (status.background?.configured == true) {
        invitation = status.background?.invitation;
      }
      if (snapshot.viewer == null || snapshot.viewer?.state == 'stopped') {
        _mediaFailed = snapshot.viewer != null;
        if (_target != null) {
          _scheduleReconnect(snapshot.viewer?.message ?? error);
          return;
        }
        if (_frames != null || videoTexture != null || frame != null) {
          await _clearVideo();
        }
        return;
      }
      if (connected && _frames == null && !_mediaFailed) {
        final latest = await api.frame(
          frame?.status.frameId ?? 0,
          frame?.status.generation ?? 0,
        );
        if (!_current(epoch)) return;
        if (latest != null) {
          await _acceptFrame(latest, epoch);
        }
      } else if (!connected) {
        frame = null;
      }
      if (snapshot.viewer?.controlling != true) _endingControl = false;
      await _maintainViewer();
    } catch (e) {
      if (_current(epoch)) {
        error = e.toString();
        if (_target != null) _scheduleReconnect(error);
      }
    } finally {
      _polling = false;
      _notify();
    }
  }

  Future<void> _maintainViewer() async {
    if (_target == null || !_viewerForeground) return;
    if (_mediaFailed) {
      _scheduleReconnect(error);
      return;
    }
    final status = snapshot.viewer;
    if (status?.state == 'capture_unavailable') {
      // The transport is alive. Wait for the host desktop instead of making a
      // network retry loop around a local Windows permission requirement.
      _recoveryStarted = null;
      _lastKeyframeRequest = null;
      _stableSince = null;
      return;
    }
    if (fresh) {
      _recoveryStarted = null;
      recoveryReason = null;
      _stableSince ??= _now();
      if (_now().difference(_stableSince!) >= const Duration(seconds: 3)) {
        reconnectAttempt = 0;
      }
      if (status?.controlAllowed != true) _controlWanted = false;
      if (_controlWanted &&
          status?.controlling != true &&
          !_restoringControl &&
          (_controlRequestedAt == null ||
              _now().difference(_controlRequestedAt!) >=
                  const Duration(seconds: 1))) {
        _restoringControl = true;
        _controlRequestedAt = _now();
        final serial = _sessionSerial;
        try {
          await api.control(true);
        } catch (e) {
          if (serial == _sessionSerial) recoveryReason = e.toString();
        } finally {
          _restoringControl = false;
        }
      }
      return;
    }
    _stableSince = null;
    _recoveryStarted ??= _now();
    final age = _now().difference(_recoveryStarted!);
    if (age >= const Duration(seconds: 3)) {
      _scheduleReconnect(error ?? status?.message);
    } else if (age >= const Duration(milliseconds: 700) &&
        (_lastKeyframeRequest == null ||
            _now().difference(_lastKeyframeRequest!) >=
                const Duration(seconds: 1))) {
      _lastKeyframeRequest = _now();
      _pendingKeyframe = true;
      _flushFeedback(_mediaEpoch);
    }
  }

  void _scheduleReconnect(String? reason) {
    // Certificate pin failure cannot be repaired by retrying a different
    // publisher. Keep the original invitation and require a deliberate new
    // connection after the host identity has been checked.
    if (reason?.contains('desktop identity changed') == true) {
      _identityMismatch = true;
      _retryTimer?.cancel();
      _retryTimer = null;
      _retryAt = null;
      _controlWanted = false;
      _endingControl = true;
      _inputEpoch++;
      recoveryReason = reason;
      _notify();
      return;
    }
    if (_disposed ||
        _identityMismatch ||
        !_viewerForeground ||
        _target == null ||
        _retryTimer != null ||
        _attemptRunning) {
      return;
    }
    reconnectAttempt++;
    final delay = Duration(
      seconds: (1 << (reconnectAttempt - 1).clamp(0, 4)).clamp(1, 15),
    );
    recoveryReason = reason?.isNotEmpty == true ? reason : null;
    _retryAt = _now().add(delay);
    _endingControl = true;
    _inputEpoch++;
    final serial = _sessionSerial;
    _retryTimer = Timer(delay, () {
      _retryTimer = null;
      _retryAt = null;
      if (!_disposed && serial == _sessionSerial && _target != null) {
        unawaited(_openViewer(serial));
      }
    });
    _notify();
  }

  Future<void> _acceptFrame(DesktopFrame next, int epoch) async {
    if (!_current(epoch)) return;
    cursorStatus.value = next.status;
    if (next.status.streaming &&
        next.bytes.isEmpty &&
        next.status.generation == frame?.status.generation) {
      // Transport liveness is neither a new image nor a decoder discontinuity.
      // In particular it must not refresh picture age or revoke the rendered
      // geometry while waiting for presentation credit.
      final previous = snapshot.viewer;
      snapshot = snapshot.withViewer(next.status);
      if (previous?.controlling != next.status.controlling ||
          previous?.controlAllowed != next.status.controlAllowed ||
          _now().difference(_lastMediaNotify).inMilliseconds >= 100) {
        _lastMediaNotify = _now();
        _notify();
      }
      return;
    }
    frame = next;
    snapshot = snapshot.withViewer(next.status);
    final state = next.status;
    if (!state.streaming || next.bytes.isEmpty) {
      videoPresented = null;
      _videoPresentedAt = null;
      _videoStatuses.clear();
      _videoGeneration = -1;
      _notify();
      // Status heartbeats carry no image. Keep the texture alive and reset the
      // decoder once when a real frame of the next generation arrives.
      return;
    }
    _frameAt = _now();
    if (state.codec != 'h264') {
      videoPresented = null;
      _videoPresentedAt = null;
      _videoStatuses.clear();
      final texture = videoTexture;
      if (_videoGeneration != -1 && texture != null) await texture.reset();
      _videoGeneration = -1;
      if (!_current(epoch)) return;
    }
    if (state.codec == 'h264' && next.bytes.isNotEmpty && state.streaming) {
      if (state.videoWidth < 2 || state.videoHeight < 2) {
        throw StateError('Invalid desktop video dimensions');
      }
      if (videoTexture == null) {
        final texture = await DesktopVideoTexture.create();
        if (!_current(epoch) || !connected) {
          await texture.dispose();
          return;
        }
        videoTexture = texture;
        _videoEvents = texture.events.listen((event) {
          if (_current(epoch) && identical(texture, videoTexture)) {
            _presentVideo(event, epoch);
          }
        });
      }
      final texture = videoTexture!;
      if (_videoGeneration != state.generation) {
        _videoGeneration = state.generation;
        videoPresented = null;
        _videoPresentedAt = null;
        _videoStatuses.clear();
        _notify();
        await texture.reset();
        if (!_current(epoch) || !identical(texture, videoTexture)) return;
      }
      _videoStatuses[state.frameId] = (state, _frameAt!);
      while (_videoStatuses.length > 256) {
        _videoStatuses.remove(_videoStatuses.keys.first);
      }
      await texture.submit(
        bytes: next.bytes,
        generation: state.generation,
        frameId: state.frameId,
        ptsUs: state.ptsUs,
        width: state.videoWidth,
        height: state.videoHeight,
        keyframe: state.keyframe,
      );
      if (!_current(epoch)) return;
    }
    if (next.status.codec != 'h264' ||
        _now().difference(_lastMediaNotify).inMilliseconds >= 100) {
      _lastMediaNotify = _now();
      _notify();
    }
  }

  void _presentVideo(DesktopVideoEvent event, int epoch) {
    if (!_current(epoch) || event.generation != _videoGeneration) return;
    final wasFresh = fresh;
    if (event.needKeyframe) {
      videoPresented = null;
      _videoPresentedAt = null;
      if (event.message.isNotEmpty) {
        error = event.message;
        _videoError = (event.generation, event.message);
      }
      _pendingKeyframe = true;
      _flushFeedback(epoch);
    }
    final received = _videoStatuses[event.frameId];
    final status = received?.$1;
    if (event.presented &&
        status != null &&
        status.generation == frame?.status.generation &&
        status.frameId > (videoPresented?.frameId ?? -1) &&
        snapshot.viewer?.streaming == true &&
        _now().difference(received!.$2) < const Duration(seconds: 2)) {
      videoPresented = status;
      _videoPresentedAt = received.$2;
      if (_videoError?.$1 == status.generation) {
        if (error == _videoError?.$2) error = null;
        _videoError = null;
      }
      _presentCount++;
      final elapsed = _now().difference(_presentSample).inMicroseconds;
      if (elapsed >= 1000000) {
        _presentFps = _presentCount * 1000000 / elapsed;
        _presentCount = 0;
        _presentSample = _now();
      }
      _pendingPresentation = status;
      _flushFeedback(epoch);
    }
    if (event.needKeyframe ||
        (!wasFresh && fresh) ||
        _now().difference(_lastMediaNotify).inMilliseconds >= 100) {
      _lastMediaNotify = _now();
      _notify();
    }
  }

  void _flushFeedback(int epoch) {
    if (_feedback != null || !_current(epoch)) return;
    final work = () async {
      while (_current(epoch) &&
          (_pendingKeyframe || _pendingPresentation != null)) {
        try {
          if (_pendingKeyframe) {
            _pendingKeyframe = false;
            await api.keyframe();
          } else if (_pendingPresentation case final status?) {
            // Keep cumulative feedback within two 60 Hz refreshes. Waiting
            // 100 ms can fill the bounded receiver window even on a fast link.
            final delay =
                const Duration(milliseconds: 33) - _now().difference(_lastAck);
            if (delay > Duration.zero) {
              // Always acknowledge the final displayed frame even if media
              // admission pauses the source before another event can arrive.
              _presentationTimer ??= Timer(delay, () {
                _presentationTimer = null;
                _flushFeedback(epoch);
              });
              break;
            }
            _presentationTimer?.cancel();
            _presentationTimer = null;
            _pendingPresentation = null;
            _lastAck = _now();
            await api.presented(status.generation, status.frameId);
          }
        } catch (_) {
          /* Session teardown invalidates these observations. */
        }
      }
    }();
    _feedback = work;
    unawaited(
      work.whenComplete(() {
        if (identical(_feedback, work)) _feedback = null;
      }),
    );
  }

  void acknowledgePresented(DesktopStatus status, int epoch) {
    if (!_current(epoch) ||
        !connected ||
        status.generation != frame?.status.generation ||
        snapshot.viewer?.streaming != true) {
      return;
    }
    _pendingPresentation = status;
    _flushFeedback(epoch);
  }

  Future<void> _consumeFrames(
    StreamIterator<DesktopFrame> frames,
    int epoch,
  ) async {
    try {
      while (await frames.moveNext()) {
        if (_disposed || epoch != _mediaEpoch) break;
        await _acceptFrame(frames.current, epoch);
      }
    } catch (e) {
      if (!_disposed && epoch == _mediaEpoch) {
        error = e.toString();
        frame = null;
        _notify();
      }
    } finally {
      await frames.cancel();
      if (_current(epoch) && identical(_frames, frames)) {
        _frames = null;
        _mediaFailed = true;
        frame = null;
        videoPresented = null;
        error ??= 'Desktop video stream ended';
        _notify();
      }
    }
  }

  Future<void> _clearVideo() async {
    _mediaEpoch++;
    cursorStatus.value = null;
    _presentationTimer?.cancel();
    _presentationTimer = null;
    final frames = _frames;
    _frames = null;
    final events = _videoEvents;
    _videoEvents = null;
    final texture = videoTexture;
    videoTexture = null;
    final feedback = _feedback;
    _feedback = null;
    _pendingPresentation = null;
    _pendingKeyframe = false;
    frame = null;
    _frameAt = null;
    videoPresented = null;
    _videoPresentedAt = null;
    _videoGeneration = -1;
    _videoStatuses.clear();
    _videoError = null;
    _presentFps = 0;
    _presentCount = 0;
    _presentSample = _now();
    _lastAck = DateTime.fromMillisecondsSinceEpoch(0);
    _lastMediaNotify = DateTime.fromMillisecondsSinceEpoch(0);
    final epoch = _mediaEpoch;
    try {
      await Future.wait([
        if (frames != null) frames.cancel(),
        if (events != null) events.cancel(),
        if (texture != null) texture.dispose(),
        if (feedback != null) feedback,
      ]);
    } catch (e) {
      if (_current(epoch)) error = e.toString();
    }
  }

  Future<void> run(Future<void> Function() operation) async {
    if (busy || _disposed) return;
    busy = true;
    error = null;
    _notify();
    try {
      await operation();
    } catch (e) {
      error = e.toString();
    } finally {
      busy = false;
      _notify();
    }
  }

  Future<void> startHost(
    String service,
    int display,
    bool control,
    bool relayOnly, {
    bool privileged = false,
    bool remember = false,
    bool autoStart = false,
    bool background = false,
  }) async {
    await run(() async {
      invitation = await api.startHost(
        service: service,
        displayId: display,
        allowControl: control,
        relayOnly: relayOnly,
        privileged: privileged,
        remember: remember,
        autoStart: autoStart,
        background: background,
      );
      trust = await api.trustStatus();
    });
    await poll();
  }

  Future<void> enableHost(String service, int display) async {
    await run(() async {
      _cancelHostResume();
      final serial = _hostResumeSerial;
      final DesktopHostEnable result;
      try {
        result = await api.enableHost(service, display);
      } catch (_) {
        // A failed IPC reply can leave durable setup waiting for the helper.
        // Resume that intent through HostRestore, which never installs or asks
        // for UAC. Cancelled installation has already retired this intent.
        try {
          final pending = await api.trustStatus();
          if (_hostResumeCurrent(serial) &&
              pending.saved?.background == true &&
              pending.saved?.autoStart == true &&
              pending.matchesRelay) {
            trust = pending;
            _scheduleHostResume(serial, _resumePendingHost);
          }
        } catch (_) {
          // Preserve the original setup error when local trust cannot be read.
        }
        rethrow;
      }
      if (!_hostResumeCurrent(serial)) return;
      if (result.restart) {
        final restart = onRestartReady;
        if (restart == null) {
          throw StateError('Application restart is unavailable');
        }
        restarting = true;
        _timer?.cancel();
        _notify();
        await restart();
        return;
      }
      invitation = result.invitation;
      if (result.background case final background?) {
        snapshot = snapshot.withBackground(background);
      }
      final enabledTrust = await api.trustStatus();
      if (!_hostResumeCurrent(serial)) return;
      trust = enabledTrust;
    });
    if (!restarting) await poll();
  }

  Future<void> stopHost() async {
    _cancelHostResume();
    final serial = _hostResumeSerial;
    // Local stop must work during an unrelated outgoing connection attempt.
    try {
      await api.stopHost();
      if (!_hostResumeCurrent(serial)) return;
      final stoppedTrust = await api.trustStatus();
      if (!_hostResumeCurrent(serial)) return;
      trust = stoppedTrust;
      invitation = null;
      snapshot = snapshot.withHost(null);
    } catch (e) {
      if (_hostResumeCurrent(serial)) error = e.toString();
    }
    _notify();
    await poll();
  }

  Future<void> helperAction(String action) async {
    await run(() async {
      if (await api.helperAction(action)) {
        final restart = onRestartReady;
        if (restart == null) {
          throw StateError('Application restart is unavailable');
        }
        restarting = true;
        _timer?.cancel();
        _notify();
        await restart();
        return;
      }
      capabilities = await api.capabilities();
      await _loadBackground();
    });
    if (!restarting) await poll();
  }

  Future<void> allowControl(bool allow) async {
    if (allow) {
      await run(() => api.allowControl(true));
    } else {
      try {
        await api.allowControl(false);
      } catch (e) {
        error = e.toString();
      }
      _notify();
    }
    try {
      trust = await api.trustStatus();
    } catch (e) {
      if (!_disposed) error = e.toString();
    }
    await poll();
  }

  Future<void> connect(String invite, bool relayOnly) async {
    await _disconnecting;
    if (_disposed || busy || _attemptRunning) return;
    _viewerForeground = true;
    _retryTimer?.cancel();
    _retryTimer = null;
    _retryAt = null;
    _target = (invite, relayOnly);
    _controlWanted = true;
    _identityMismatch = false;
    reconnectAttempt = 0;
    error = null;
    recoveryReason = null;
    final serial = ++_sessionSerial;
    _timer ??= Timer.periodic(const Duration(milliseconds: 250), (_) => poll());
    await _openViewer(serial);
    await poll();
  }

  Future<void> _openViewer(int serial) async {
    final target = _target;
    if (_disposed ||
        !_viewerForeground ||
        target == null ||
        serial != _sessionSerial ||
        _attemptRunning) {
      return;
    }
    _attemptSerial = serial;
    _notify();
    try {
      final needsDisconnect = snapshot.viewer != null || reconnectAttempt > 0;
      final cleanup = _clearVideo();
      final epoch = _mediaEpoch;
      if (needsDisconnect) await api.disconnect();
      await cleanup;
      if (!_current(epoch) || serial != _sessionSerial || !_viewerForeground) {
        return;
      }
      await api.connect(target.$1, relayOnly: target.$2);
      if (!_current(epoch) || serial != _sessionSerial) return;
      _sequence = 0;
      _inputEpoch++;
      frame = null;
      _endingControl = !_viewerForeground;
      _releasing = false;
      _mediaFailed = false;
      _recoveryStarted = _now();
      _lastKeyframeRequest = null;
      _controlRequestedAt = null;
      error = null;
      snapshot = snapshot.withViewer(const DesktopStatus(state: 'connecting'));
      final frames = StreamIterator(api.frames());
      _frames = frames;
      unawaited(_consumeFrames(frames, epoch));
    } catch (e) {
      if (!_disposed && serial == _sessionSerial) {
        error = e.toString();
        _mediaFailed = true;
      }
    } finally {
      if (_attemptSerial == serial) _attemptSerial = null;
      if (!_disposed && serial == _sessionSerial && _mediaFailed) {
        _scheduleReconnect(error);
      }
      _notify();
    }
  }

  Future<void> disconnect() {
    if (_disconnecting case final closing?) return closing;
    final closing = _disconnect();
    _disconnecting = closing;
    unawaited(
      closing.whenComplete(() {
        if (identical(_disconnecting, closing)) _disconnecting = null;
      }),
    );
    return closing;
  }

  Future<void> _disconnect() async {
    _target = null;
    _sessionSerial++;
    _retryTimer?.cancel();
    _retryTimer = null;
    _retryAt = null;
    _controlWanted = false;
    _identityMismatch = false;
    _recoveryStarted = null;
    recoveryReason = null;
    _endingControl = true;
    _inputEpoch++;
    final cleanup = _clearVideo();
    snapshot = snapshot.withViewer(null);
    final epoch = _mediaEpoch;
    _notify();
    try {
      await api.disconnect();
    } catch (e) {
      if (_current(epoch)) error = e.toString();
    }
    await Future.wait([cleanup, _inputTail]);
    _notify();
  }

  Future<void> control(bool enabled) async {
    if (_disposed) return;
    _controlWanted = enabled;
    _controlRequestedAt = _now();
    _endingControl = !enabled;
    _inputEpoch++;
    _notify();
    if (enabled) {
      await run(() => api.control(true));
    } else {
      // Focus loss and local end-control must not be skipped by an unrelated
      // in-flight operation or navigation.
      try {
        await api.control(false);
      } catch (e) {
        if (connected) error = e.toString();
      }
    }
  }

  Future<void> release() async {
    if (!connected || _releasing) return;
    _inputEpoch++;
    _releasing = true;
    final epoch = _mediaEpoch;
    try {
      await _inputTail;
      if (_current(epoch)) await api.release();
    } catch (_) {
      /* disconnect also releases */
    } finally {
      if (_current(epoch)) _releasing = false;
    }
  }

  void setViewerForeground(bool foreground) {
    if (_disposed || _viewerForeground == foreground) return;
    _viewerForeground = foreground;
    if (!foreground) {
      _retryTimer?.cancel();
      _retryTimer = null;
      _retryAt = null;
      // Suspend actual input while retaining the user's desired mode. A fresh
      // presented frame is still required before reacquiring the host grant.
      _endingControl = true;
      _inputEpoch++;
      if (connected) {
        unawaited(api.control(false).catchError((Object _) {}));
      }
    } else {
      _recoveryStarted = null;
      scheduleMicrotask(() => unawaited(poll()));
    }
  }

  Future<void> send(
    DesktopStatus rendered,
    Map<String, Object?> event, {
    int? mediaEpoch,
  }) async {
    if (mediaEpoch != null && mediaEpoch != _mediaEpoch) return;
    if (!controlling || rendered.generation != frame?.status.generation) return;
    final moving = event['kind'] == 'move';
    final pending = _pendingMove;
    if (moving && pending != null && pending.epoch == _inputEpoch) {
      pending.rendered = rendered;
      pending.event = event;
      return pending.done;
    }
    // A button/key is an ordering barrier. Only consecutive unsent pointer
    // motion may be replaced, so a click still follows its intended position.
    _pendingMove = null;
    // Bound calls from gesture sources as well as the native input queue. If
    // saturated, end control instead of dropping a release or replaying a click.
    if (_inFlight >= 16) {
      await _interruptInput();
      return;
    }
    _inFlight++;
    final epoch = _inputEpoch;
    final move = moving ? _PendingMove(rendered, event, epoch) : null;
    _pendingMove = move;
    final operation = _inputTail.then((_) async {
      if (identical(_pendingMove, move)) _pendingMove = null;
      final currentRendered = move?.rendered ?? rendered;
      final currentEvent = move?.event ?? event;
      try {
        if (_disposed ||
            epoch != _inputEpoch ||
            !controlling ||
            currentRendered.generation != frame?.status.generation) {
          return;
        }
        await api.input(
          generation: currentRendered.generation,
          frameId: currentRendered.frameId,
          sequence: ++_sequence,
          input: currentEvent,
        );
      } catch (e) {
        if (_disposed || epoch != _inputEpoch) return;
        error = e.toString();
        // Do not await our own queue tail from inside it.
        await _interruptInput();
      } finally {
        _inFlight--;
      }
    });
    if (move != null) move.done = operation;
    _inputTail = operation;
    await operation;
  }

  Future<void> _interruptInput() async {
    _endingControl = true;
    _inputEpoch++;
    _pendingMove = null;
    try {
      await api.control(false);
    } catch (_) {
      // Transport loss already releases remote input. The current invitation
      // may reconnect, but queued clicks and keys are never replayed.
    }
    _notify();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _cancelHostResume();
    _disposed = true;
    _target = null;
    _retryTimer?.cancel();
    _timer?.cancel();
    _inputEpoch++;
    unawaited(_clearVideo());
    unawaited(api.disconnect().catchError((Object _) {}));
    unawaited(api.stopHost(onExit: true).catchError((Object _) {}));
    videoPresentation.dispose();
    cursorStatus.dispose();
    super.dispose();
  }
}

class _PendingMove {
  _PendingMove(this.rendered, this.event, this.epoch);
  DesktopStatus rendered;
  Map<String, Object?> event;
  final int epoch;
  late Future<void> done;
}
