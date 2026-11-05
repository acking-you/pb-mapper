import 'dart:async';
import 'dart:isolate';
import 'dart:ui' as ui;

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_video.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_service.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'fake_desktop_api.dart';

void _frameWorker((SendPort, bool) args) {
  final commands = ReceivePort();
  args.$1.send(commands.sendPort);
  commands.listen((message) {
    if (message == false) {
      commands.close();
    } else if (message is (int, Map)) {
      if (message.$2['op'] != 'wait') {
        args.$1.send((message.$1, {'success': true}));
      }
    } else if (message == true && args.$2) {
      args.$1.send({'frame': 1});
    }
  });
}

class _StreamingApi extends FakeDesktopApi {
  StreamController<DesktopFrame>? source;
  Completer<void>? controlGate;
  int disconnects = 0;
  final presentations = <(int, int)>[];
  void Function()? onPresentation;
  @override
  Future<void> connect(String invitation, {required bool relayOnly}) async {
    source = StreamController<DesktopFrame>(sync: true);
    viewer = const DesktopStatus(state: 'streaming');
  }

  @override
  Stream<DesktopFrame> frames() => source!.stream;
  @override
  Future<void> disconnect() async {
    disconnects++;
    viewer = null;
  }

  @override
  Future<void> control(bool enabled) async {
    await controlGate?.future;
  }

  @override
  Future<void> presented(int generation, int frameId) async {
    presentations.add((generation, frameId));
    onPresentation?.call();
  }

  void emit(int generation, int frameId, {bool blank = false}) {
    viewer = DesktopStatus(
      state: blank ? 'capture_unavailable' : 'streaming',
      generation: generation,
      frameId: frameId,
      controlling: true,
      controlAllowed: true,
      codec: 'h264',
      videoWidth: blank ? 0 : 1920,
      videoHeight: blank ? 0 : 1080,
      display: const DesktopDisplay(1, 'test', 1920, 1080),
    );
    source!.add(DesktopFrame(viewer!, Uint8List.fromList(blank ? [] : [1])));
  }
}

const _channel = MethodChannel('pb_mapper/desktop_video');
Future<void> _nativePresentation(int texture, int generation, int frame) async {
  final completed = Completer<void>();
  ui.channelBuffers.push(
    _channel.name,
    const StandardMethodCodec().encodeMethodCall(
      MethodCall('event', {
        'textureId': texture,
        'generation': generation,
        'frameId': frame,
        'width': 1920,
        'height': 1080,
        'presented': true,
      }),
    ),
    (_) => completed.complete(),
  );
  await completed.future;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  test(
    'the final presented frame is acknowledged without another frame and teardown cancels it',
    () async {
      final api = _StreamingApi();
      api.background = const DesktopBackground(configured: true, enabled: true);
      var now = DateTime.utc(2026);
      final store = DesktopStore(api, now: () => now);
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      Completer<void>? submitted;
      messenger.setMockMethodCallHandler(_channel, (call) async {
        if (call.method == 'submit') submitted!.complete();
        return call.method == 'create' ? 19 : null;
      });
      await store.connect('test', false);
      Future<void> present(int id) async {
        submitted = Completer<void>();
        api.emit(7, id);
        await submitted!.future;
        await _nativePresentation(19, 7, id);
      }

      await present(1);
      expect(store.snapshot.background?.enabled, isTrue);
      expect(api.presentations, [(7, 1)]);
      now = now.add(const Duration(milliseconds: 10));
      await present(2);
      expect(api.presentations, [(7, 1)]);
      final trailing = Completer<void>();
      api.onPresentation = trailing.complete;
      now = now.add(const Duration(milliseconds: 90));
      await trailing.future.timeout(const Duration(seconds: 1));
      api.onPresentation = null;
      expect(api.presentations, [(7, 1), (7, 2)]);
      await present(3);
      await store.disconnect();
      expect(store.snapshot.background?.enabled, isTrue);
      now = now.add(const Duration(milliseconds: 100));
      await Future<void>.delayed(const Duration(milliseconds: 120));
      expect(api.presentations, [(7, 1), (7, 2)]);
      store.dispose();
      await api.source!.close();
      messenger.setMockMethodCallHandler(_channel, null);
    },
    skip: !DesktopVideoTexture.supported,
  );
  test(
    'worker cancellation releases its native borrow before handshake and while paused',
    () async {
      final started = Completer<void>(), launch = Completer<void>();
      var closed = 0;
      final opening = DesktopFrameStream(
        spawn: (messages, exited, errors) async {
          started.complete();
          await launch.future;
          return Isolate.spawn(
            _frameWorker,
            (messages, false),
            onExit: exited,
            onError: errors,
          );
        },
        onClosed: () => closed++,
      );
      final iterator = StreamIterator(opening.stream);
      final pending = iterator.moveNext();
      await started.future;
      final cancelledCall = expectLater(
        opening.call({'op': 'presented'}),
        throwsStateError,
      );
      final cancellation = iterator.cancel();
      launch.complete();
      await cancellation.timeout(const Duration(seconds: 2));
      await cancelledCall;
      expect(await pending, false);
      expect(closed, 1);

      final paused = DesktopFrameStream(
        spawn: (messages, exited, errors) => Isolate.spawn(
          _frameWorker,
          (messages, true),
          onExit: exited,
          onError: errors,
        ),
        onClosed: () => closed++,
      );
      final frames = StreamIterator(paused.stream);
      expect(await frames.moveNext(), true);
      // StreamIterator is now paused with a delivered frame and no new credit.
      expect(await paused.call({'op': 'presented'}), {'success': true});
      final waitingCall = expectLater(
        paused.call({'op': 'wait'}),
        throwsStateError,
      );
      await frames.cancel().timeout(const Duration(seconds: 2));
      await waitingCall;
      expect(closed, 2);
    },
  );

  test(
    'disconnect is effective while busy and disposes a late native texture',
    () async {
      final api = _StreamingApi();
      final store = DesktopStore(api);
      final creating = Completer<int>();
      final createCalled = Completer<void>(),
          disposedCalled = Completer<void>();
      final disposed = <int>[];
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      messenger.setMockMethodCallHandler(_channel, (call) async {
        if (call.method == 'create') {
          createCalled.complete();
          return creating.future;
        }
        if (call.method == 'dispose') {
          disposed.add((call.arguments as Map)['textureId'] as int);
          disposedCalled.complete();
        }
        return null;
      });
      await store.connect('test', false);
      api.emit(1, 1);
      await createCalled.future;
      api.controlGate = Completer<void>();
      final control = store.control(true);
      expect(store.busy, true);
      await store.disconnect();
      expect(api.disconnects, 1);
      expect(store.connected, false);
      api.controlGate!.complete();
      creating.complete(77);
      await Future.wait([control, disposedCalled.future]);
      expect(disposed, [77]);
      expect(store.videoTexture, isNull);
      expect(store.frame, isNull);
      store.dispose();
      await api.source!.close();
      messenger.setMockMethodCallHandler(_channel, null);
    },
    skip: !DesktopVideoTexture.supported,
  );

  test(
    'native consumption must be current and recent before input becomes live',
    () async {
      var now = DateTime(2026, 11, 5);
      final api = _StreamingApi();
      final store = DesktopStore(api, now: () => now);
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      Completer<void>? accepted;
      var resets = 0;
      messenger.setMockMethodCallHandler(_channel, (call) async {
        if (call.method == 'reset') resets++;
        if (call.method == 'submit' && accepted?.isCompleted == false) {
          accepted!.complete();
        }
        return call.method == 'create' ? 9 : null;
      });
      Future<void> emit(int generation, int frame, {bool blank = false}) async {
        accepted = Completer<void>();
        void received() {
          if (blank &&
              store.frame?.status.frameId == frame &&
              !accepted!.isCompleted) {
            accepted!.complete();
          }
        }

        store.addListener(received);
        api.emit(generation, frame, blank: blank);
        await accepted!.future;
        store.removeListener(received);
      }

      await store.connect('test', false);
      await emit(3, 1);
      expect(store.fresh, false);
      await _nativePresentation(9, 3, 1);
      expect(store.controlling, true);
      expect(api.presentations, [(3, 1)]);

      final picture = store.frame;
      final presented = store.videoPresented;
      final initialResets = resets;
      now = now.add(const Duration(milliseconds: 500));
      api.source!.add(DesktopFrame(api.viewer!, Uint8List(0)));
      await Future<void>.delayed(Duration.zero);
      expect(store.frame, same(picture));
      expect(store.videoPresented, same(presented));
      expect(store.pictureAge, const Duration(milliseconds: 500));
      expect(store.controlling, isTrue);
      await emit(3, 2);
      expect(resets, initialResets);

      now = now.add(const Duration(milliseconds: 2100));
      await emit(3, 2);
      expect(
        store.fresh,
        false,
        reason: 'new encoded bytes do not renew an old rendered screen',
      );
      now = now.add(const Duration(milliseconds: 2100));
      await _nativePresentation(9, 3, 2);
      expect(
        store.fresh,
        false,
        reason: 'late rendering cannot make a two-second-old frame current',
      );
      expect(api.presentations, [(3, 1)]);

      final beforeUnavailable = resets;
      final texture = store.videoTexture;
      await emit(4, 3, blank: true);
      await emit(4, 3, blank: true);
      expect(resets, beforeUnavailable);
      expect(identical(store.videoTexture, texture), isTrue);
      await _nativePresentation(9, 3, 1);
      expect(store.fresh, false);
      await emit(4, 4);
      await _nativePresentation(9, 4, 4);
      expect(store.fresh, true);
      expect(api.presentations.last, (4, 4));
      expect(store.presentFps, greaterThan(0));
      now = now.add(const Duration(milliseconds: 2100));
      expect(store.presentFps, 0);
      expect(store.controlling, false);
      await store.disconnect();
      store.dispose();
      await api.source!.close();
      messenger.setMockMethodCallHandler(_channel, null);
    },
    skip: !DesktopVideoTexture.supported,
  );
}
