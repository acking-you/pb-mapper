import 'dart:async';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'fake_desktop_api.dart';

class _RecoveryApi extends FakeDesktopApi {
  final connections = <(String, bool)>[];
  final controls = <bool>[];
  final sources = <StreamController<DesktopFrame>>[];
  final connectionEvents = StreamController<void>.broadcast(sync: true);
  Completer<void>? connectGate;
  bool failNext = false;
  int keyframes = 0, serial = 0;

  @override
  Future<void> connect(String invitation, {required bool relayOnly}) async {
    connections.add((invitation, relayOnly));
    connectionEvents.add(null);
    final current = ++serial;
    final gate = connectGate;
    connectGate = null;
    await gate?.future;
    if (current != serial) return;
    if (failNext) {
      failNext = false;
      throw StateError('test transport unavailable');
    }
    sources.add(StreamController<DesktopFrame>(sync: true));
    viewer = const DesktopStatus(state: 'connecting');
  }

  @override
  Stream<DesktopFrame> frames() => sources.last.stream;

  @override
  Future<void> disconnect() async {
    serial++;
    viewer = null;
  }

  @override
  Future<void> control(bool enabled) async {
    controls.add(enabled);
    await super.control(enabled);
  }

  @override
  Future<void> keyframe() async => keyframes++;

  void emit({bool controlling = false, bool unavailable = false}) {
    viewer = DesktopStatus(
      state: unavailable ? 'capture_unavailable' : 'streaming',
      controlling: controlling,
      controlAllowed: true,
      generation: connections.length,
      frameId: 1,
    );
    image = DesktopFrame(viewer!, Uint8List.fromList(unavailable ? [] : [1]));
    sources.last.add(image!);
  }

  Future<void> close() async {
    for (final source in sources) {
      await source.close();
    }
    await connectionEvents.close();
  }
}

Future<void> _until(DesktopStore store, bool Function() ready) async {
  if (ready()) return;
  final completed = Completer<void>();
  void changed() {
    if (!completed.isCompleted && ready()) completed.complete();
  }

  store.addListener(changed);
  try {
    await completed.future.timeout(const Duration(seconds: 5));
  } finally {
    store.removeListener(changed);
  }
}

Future<void> _noConnectionFor(_RecoveryApi api, Duration duration) =>
    expectLater(
      api.connectionEvents.stream.timeout(duration).first,
      throwsA(isA<TimeoutException>()),
    );

void main() {
  test(
    'a failed stream retries the same target and path, restores only fresh control, and manual stop cancels retries',
    () async {
      final api = _RecoveryApi();
      var now = DateTime.utc(2026);
      final store = DesktopStore(api, now: () => now);
      addTearDown(() async {
        await store.disconnect();
        store.dispose();
        await api.close();
      });
      await store.connect('private invitation', true);
      api.emit();
      await _until(store, () => store.fresh);
      await store.control(true);
      api.emit(controlling: true);
      await _until(store, () => store.controlling);
      expect(store.controlling, isTrue);
      api.failNext = true;
      api.sources.last.addError(StateError('connection reset'));
      await _until(
        store,
        () => store.connectionPhase == DesktopConnectionPhase.retryWaiting,
      );
      expect(store.controlling, isFalse);
      expect(store.retryIn, const Duration(seconds: 1));
      await _until(store, () => store.reconnectAttempt == 2);
      expect(api.connections.length, 2);
      expect(store.retryIn, const Duration(seconds: 2));
      await _until(
        store,
        () =>
            api.connections.length == 3 &&
            store.connectionPhase == DesktopConnectionPhase.recoveringVideo,
      );
      expect(api.connections, List.filled(3, ('private invitation', true)));
      expect(api.controls.where((enabled) => enabled).length, 1);
      api.emit();
      await _until(
        store,
        () => api.controls.where((enabled) => enabled).length == 2,
      );
      expect(api.controls.where((enabled) => enabled).length, 2);
      expect(
        api.inputs,
        isEmpty,
        reason: 'recovery never replays old gestures',
      );
      now = now.add(const Duration(seconds: 3));
      api.emit(controlling: true);
      await _until(store, () => store.reconnectAttempt == 0);
      api.sources.last.addError(StateError('connection reset'));
      await _until(
        store,
        () => store.connectionPhase == DesktopConnectionPhase.retryWaiting,
      );
      final retryDeadline = store.retryIn + const Duration(milliseconds: 100);
      final remainsStopped = _noConnectionFor(api, retryDeadline);
      await store.disconnect();
      await remainsStopped;
      expect(api.connections.length, 3);
      expect(store.connectionPhase, DesktopConnectionPhase.idle);
    },
  );

  test(
    'stalls request an IDR before reconnect, while desktop transitions and backgrounding do not churn connections',
    () async {
      final api = _RecoveryApi();
      var now = DateTime.utc(2026);
      final store = DesktopStore(api, now: () => now);
      addTearDown(() async {
        await store.disconnect();
        store.dispose();
        await api.close();
      });
      await store.connect('target', false);
      now = now.add(const Duration(milliseconds: 750));
      await store.poll();
      expect(api.keyframes, 1);
      api.emit(unavailable: true);
      await _until(
        store,
        () => store.connectionPhase == DesktopConnectionPhase.waitingForDesktop,
      );
      now = now.add(const Duration(seconds: 6));
      await store.poll();
      expect(store.connectionPhase, DesktopConnectionPhase.waitingForDesktop);
      expect(api.connections.length, 1);
      expect(api.keyframes, 1);
      api.emit();
      await _until(store, () => store.fresh);
      expect(store.connectionPhase, DesktopConnectionPhase.live);
      store.setViewerForeground(false);
      api.sources.last.addError(StateError('connection reset'));
      await _until(store, () => store.error != null && !store.fresh);
      now = now.add(const Duration(seconds: 20));
      await _noConnectionFor(api, const Duration(milliseconds: 1100));
      expect(api.connections.length, 1);
      store.setViewerForeground(true);
      await _until(
        store,
        () =>
            api.connections.length == 2 &&
            store.connectionPhase == DesktopConnectionPhase.recoveringVideo,
      );
      expect(api.connections.length, 2);
      expect(api.controls, [false]);
    },
  );

  test(
    'cancelled connection completion cannot replace a newer session',
    () async {
      final api = _RecoveryApi();
      final now = DateTime.utc(2026);
      final store = DesktopStore(api, now: () => now);
      addTearDown(() async {
        await store.disconnect();
        store.dispose();
        await api.close();
      });
      final gate = Completer<void>();
      api.connectGate = gate;
      final entered = api.connectionEvents.stream.first;
      final first = store.connect('old target', false);
      await entered;
      await store.disconnect();
      await store.connect('new target', false);
      api.emit();
      await _until(store, () => store.fresh);
      final epoch = store.mediaEpoch;
      gate.complete();
      await first;
      expect(store.fresh, isTrue);
      expect(store.mediaEpoch, epoch);
      expect(api.connections, [('old target', false), ('new target', false)]);
    },
  );

  test('pointer motion coalesces without crossing click barriers', () async {
    final api = FakeDesktopApi();
    final store = DesktopStore(api);
    const status = DesktopStatus(
      state: 'streaming',
      controlling: true,
      generation: 1,
      frameId: 1,
    );
    api.viewer = status;
    api.image = DesktopFrame(status, Uint8List.fromList([1]));
    await store.poll();
    api.inputGate = Completer<void>();
    final first = store.send(status, {'kind': 'move', 'x': 0});
    await Future<void>.delayed(Duration.zero);
    final work = [first];
    for (var x = 1; x <= 1000; x++) {
      work.add(store.send(status, {'kind': 'move', 'x': x}));
    }
    work.add(store.send(status, {'kind': 'button', 'down': true}));
    work.add(store.send(status, {'kind': 'button', 'down': false}));
    work.add(store.send(status, {'kind': 'move', 'x': 1001}));
    api.inputGate!.complete();
    await Future.wait(work);
    expect(api.inputs, [
      {'kind': 'move', 'x': 0},
      {'kind': 'move', 'x': 1000},
      {'kind': 'button', 'down': true},
      {'kind': 'button', 'down': false},
      {'kind': 'move', 'x': 1001},
    ]);
    expect(api.sequences, [1, 2, 3, 4, 5]);
    expect(store.controlling, isTrue);
    store.dispose();
  });
}
