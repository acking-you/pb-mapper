import 'dart:async';
import 'package:flutter/foundation.dart';
import '../ffi/desktop_api.dart';
import '../ffi/desktop_video.dart';

/// Owns the session independently of navigation. Only the newest frame is kept,
/// and each input carries the geometry of the frame actually rendered by UI.
class DesktopStore extends ChangeNotifier {
  DesktopStore(this.api, {DateTime Function()? now})
    : _now = now ?? DateTime.now,
      _presentSample = (now ?? DateTime.now)();
  final DateTime Function() _now;
  final DesktopApiClient api;
  DesktopCapabilities capabilities = const DesktopCapabilities();
  DesktopTrust trust = const DesktopTrust();
  bool _trustRestored = false;
  DesktopSnapshot snapshot = const DesktopSnapshot();
  DesktopFrame? frame;
  DesktopVideoTexture? videoTexture;
  final videoPresentation = ValueNotifier<DesktopStatus?>(null);
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
  bool _endingControl = false;
  bool _releasing = false;
  bool get sharing => snapshot.host != null;
  bool get connected => snapshot.viewer != null;
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
    });
    if (_disposed) return;
    _timer ??= Timer.periodic(const Duration(milliseconds: 250), (_) => poll());
    await poll();
  }

  Future<void> restoreOnStartup() async {
    if (_trustRestored || _disposed) return;
    _trustRestored = true;
    try {
      trust = await api.trustStatus();
      if (trust.saved?.autoStart == true && trust.matchesRelay) {
        await restoreHost();
        if (!_disposed) {
          _timer ??= Timer.periodic(
            const Duration(milliseconds: 250),
            (_) => poll(),
          );
        }
      }
    } catch (e) {
      error = e.toString();
      _notify();
    }
  }

  Future<void> restoreHost() async {
    await run(() async {
      invitation = await api.restoreHost();
      trust = await api.trustStatus();
    });
    await poll();
  }

  Future<void> revokeTrust() async {
    try {
      await api.revokeTrust();
      invitation = null;
      trust = await api.trustStatus();
      snapshot = DesktopSnapshot(viewer: snapshot.viewer);
    } catch (e) {
      error = e.toString();
    }
    _notify();
    await poll();
  }

  Future<void> poll() async {
    if (_polling || _disposed || busy || _disconnecting != null) return;
    _polling = true;
    final epoch = _mediaEpoch;
    try {
      final status = await api.status();
      if (!_current(epoch)) return;
      snapshot = status;
      if (snapshot.viewer == null || snapshot.viewer?.state == 'stopped') {
        _mediaFailed = snapshot.viewer != null;
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
    } catch (e) {
      if (_current(epoch)) {
        error = e.toString();
        frame = null;
      }
    } finally {
      _polling = false;
      _notify();
    }
  }

  Future<void> _acceptFrame(DesktopFrame next, int epoch) async {
    if (!_current(epoch)) return;
    frame = next;
    _frameAt = _now();
    snapshot = DesktopSnapshot(host: snapshot.host, viewer: next.status);
    final state = next.status;
    if (state.codec != 'h264' || !state.streaming || next.bytes.isEmpty) {
      videoPresented = null;
      _videoPresentedAt = null;
      _videoStatuses.clear();
      _videoGeneration = -1;
      _notify();
      final texture = videoTexture;
      if (texture != null) await texture.reset();
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
            final delay =
                const Duration(milliseconds: 100) - _now().difference(_lastAck);
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
      );
      trust = await api.trustStatus();
    });
    await poll();
  }

  Future<void> stopHost() async {
    // Local stop must work during an unrelated outgoing connection attempt.
    try {
      await api.stopHost();
      trust = await api.trustStatus();
      invitation = null;
      snapshot = DesktopSnapshot(viewer: snapshot.viewer);
    } catch (e) {
      error = e.toString();
    }
    _notify();
    await poll();
  }

  Future<void> helperAction(String action) async {
    await run(() async {
      await api.helperAction(action);
      capabilities = await api.capabilities();
    });
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
    await run(() async {
      final cleanup = _clearVideo();
      final epoch = _mediaEpoch;
      await cleanup;
      if (!_current(epoch)) return;
      await api.connect(invite, relayOnly: relayOnly);
      if (!_current(epoch)) return;
      _sequence = 0;
      _inputEpoch++;
      frame = null;
      _endingControl = false;
      _releasing = false;
      _mediaFailed = false;
      snapshot = DesktopSnapshot(
        host: snapshot.host,
        viewer: const DesktopStatus(state: 'connecting'),
      );
      final frames = StreamIterator(api.frames());
      _frames = frames;
      unawaited(_consumeFrames(frames, epoch));
    });
    await poll();
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
    _endingControl = true;
    _inputEpoch++;
    final cleanup = _clearVideo();
    snapshot = DesktopSnapshot(host: snapshot.host);
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
    if (!connected) return;
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

  Future<void> send(
    DesktopStatus rendered,
    Map<String, Object?> event, {
    int? mediaEpoch,
  }) async {
    if (mediaEpoch != null && mediaEpoch != _mediaEpoch) return;
    if (!controlling || rendered.generation != frame?.status.generation) return;
    // Bound calls from gesture sources as well as the native input queue. If
    // saturated, end control instead of dropping a release or replaying a click.
    if (_inFlight >= 16) {
      await control(false);
      return;
    }
    _inFlight++;
    final epoch = _inputEpoch;
    final operation = _inputTail.then((_) async {
      try {
        if (_disposed ||
            epoch != _inputEpoch ||
            !controlling ||
            rendered.generation != frame?.status.generation) {
          return;
        }
        await api.input(
          generation: rendered.generation,
          frameId: rendered.frameId,
          sequence: ++_sequence,
          input: event,
        );
      } catch (e) {
        if (_disposed || epoch != _inputEpoch) return;
        error = e.toString();
        // Do not await our own queue tail from inside it.
        _endingControl = true;
        _inputEpoch++;
        try {
          await api.control(false);
        } catch (_) {
          // A disconnected transport has already released remote input. Keep
          // the queue usable for a subsequent explicitly opened session.
        }
        _notify();
      } finally {
        _inFlight--;
      }
    });
    _inputTail = operation;
    await operation;
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _timer?.cancel();
    _inputEpoch++;
    unawaited(_clearVideo());
    unawaited(api.disconnect().catchError((Object _) {}));
    unawaited(api.stopHost(onExit: true).catchError((Object _) {}));
    videoPresentation.dispose();
    super.dispose();
  }
}
