// How long a remote pointer shape change takes to reach this computer's
// pointer in a desktop viewer, both ways a viewer has done it:
//
// - `rebuild`: a ValueListenableBuilder rebuilds the MouseRegion with the new
//   cursor, and the mouse tracker activates it in the next frame's
//   post-frame pass, as RemotePointerRegion did before RemoteShapeCursor.
//   Timed from the change to the cursor session's activation, before its
//   platform message is even sent.
// - `system`: RemoteShapeCursor's session activates a system cursor itself
//   (`activateSystemCursor`), timed from the call to the engine's reply.
// - `bitmap_set`: it shows a bitmap the runner holds (`cursor_set` on
//   `pb_mapper/desktop_pointer`), timed from the call to the runner's reply,
//   which comes after the cursor is set.
// - `bitmap_create`: a bitmap the runner does not hold yet: a 32-pixel host
//   shape scaled to 64 pixels, made and shown (`cursor_create`, then
//   `cursor_set`), timed from before the scaling to the second reply.
//
// Each change waits 20 to 60 ms after the last, so it lands anywhere in a
// display refresh, as a host's report does. The pointer is a synthetic mouse
// inside the window: the real one is never moved. Build it in release and run
// the app binary with the result's path:
//
//   flutter build macos --release -t tool/pointer_latency.dart
//   build/macos/Build/Products/Release/pb-mapper.app/Contents/MacOS/pb-mapper \
//     pointer_latency.json
//
// It prints the summary, writes it with every sample to the path given, and
// exits. Keep the window visible while it runs: a hidden window gets no
// frames.
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math' as math;
import 'dart:ui' show PlatformDispatcher;

import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_pointer.dart';
import 'package:pb_mapper_ui/src/state/desktop_models.dart';

const _samples = 200;
const _device = 77;

final _clock = Stopwatch()..start();
int get _now => _clock.elapsedMicroseconds;

Future<void> main(List<String> args) async {
  WidgetsFlutterBinding.ensureInitialized();
  final out = args.isEmpty ? 'pointer_latency.json' : args.first;
  final cursor = ValueNotifier<MouseCursor>(_TimedCursor('text'));
  runApp(
    Directionality(
      textDirection: TextDirection.ltr,
      child: ValueListenableBuilder<MouseCursor>(
        valueListenable: cursor,
        builder: (context, cursor, _) => MouseRegion(
          cursor: cursor,
          child: const ColoredBox(color: Color(0xFF202020)),
        ),
      ),
    ),
  );
  await _frame();
  await Future<void>.delayed(const Duration(milliseconds: 500));
  final view = PlatformDispatcher.instance.implicitView;
  final centre = view == null
      ? const Offset(200, 200)
      : view.physicalSize.center(Offset.zero) / view.devicePixelRatio;
  GestureBinding.instance
    ..handlePointerEvent(
      PointerAddedEvent(
        kind: PointerDeviceKind.mouse,
        device: _device,
        position: centre,
      ),
    )
    ..handlePointerEvent(
      PointerHoverEvent(
        kind: PointerDeviceKind.mouse,
        device: _device,
        position: centre,
      ),
    );
  await _frame();

  final random = math.Random(7);
  Future<void> pause() =>
      Future<void>.delayed(Duration(milliseconds: 20 + random.nextInt(41)));

  final rebuild = <int>[];
  for (var i = 0; i < _samples; i++) {
    await pause();
    final next = _TimedCursor(i.isEven ? 'click' : 'text');
    final start = _now;
    cursor.value = next;
    rebuild.add(await next.activated.future - start);
  }

  final system = <int>[];
  for (var i = 0; i < _samples; i++) {
    await pause();
    final start = _now;
    await SystemChannels.mouseCursor.invokeMethod<void>(
      'activateSystemCursor',
      {'device': _device, 'kind': i.isEven ? 'click' : 'text'},
    );
    system.add(_now - start);
  }

  final native = desktopPointerCursors;
  final shape = _disc(32);
  var id = 1 << 20;
  final held = [id++, id++];
  for (final made in held) {
    await native.create(made, scalePointerBitmap(shape, 2));
  }
  final bitmapSet = <int>[];
  for (var i = 0; i < _samples; i++) {
    await pause();
    final start = _now;
    await native.set(held[i & 1]);
    bitmapSet.add(_now - start);
  }

  final bitmapCreate = <int>[];
  for (var i = 0; i < _samples; i++) {
    await pause();
    final made = id++;
    final start = _now;
    await native.create(made, scalePointerBitmap(shape, 2));
    await native.set(made);
    bitmapCreate.add(_now - start);
    await native.set(held[0]);
    await native.delete(made);
  }
  for (final made in held) {
    await native.delete(made);
  }

  final display = PlatformDispatcher.instance.displays.firstOrNull;
  final result = {
    'samples': _samples,
    'os': Platform.operatingSystemVersion,
    'processors': Platform.numberOfProcessors,
    'refresh_hz': display?.refreshRate,
    'device_pixel_ratio': view?.devicePixelRatio,
    'summary_ms': {
      'rebuild': _summary(rebuild),
      'system': _summary(system),
      'bitmap_set': _summary(bitmapSet),
      'bitmap_create': _summary(bitmapCreate),
    },
    'samples_us': {
      'rebuild': rebuild,
      'system': system,
      'bitmap_set': bitmapSet,
      'bitmap_create': bitmapCreate,
    },
  };
  // ignore: avoid_print
  print(const JsonEncoder.withIndent('  ').convert(result['summary_ms']));
  await File(
    out,
  ).writeAsString(const JsonEncoder.withIndent('  ').convert(result));
  exit(0);
}

Map<String, double> _summary(List<int> micros) {
  final sorted = List.of(micros)..sort();
  double at(double q) =>
      sorted[math.min(sorted.length - 1, (q * sorted.length).floor())] / 1000;
  return {
    'p50': at(0.5),
    'p90': at(0.9),
    'p99': at(0.99),
    'max': sorted.last / 1000,
  };
}

Future<void> _frame() {
  final done = Completer<void>();
  WidgetsBinding.instance
    ..addPostFrameCallback((_) => done.complete())
    ..scheduleFrame();
  return done.future;
}

/// A custom shape like the host's: a [side]-pixel checkered disc.
DesktopCursorShape _disc(int side) {
  final rgba = Uint8List(side * side * 4);
  for (var y = 0; y < side; y++) {
    for (var x = 0; x < side; x++) {
      final dx = x - side / 2, dy = y - side / 2;
      if (dx * dx + dy * dy > side * side / 4) continue;
      final at = (y * side + x) * 4;
      final magenta = (x ~/ 4 + y ~/ 4).isEven;
      rgba
        ..[at] = magenta ? 255 : 0
        ..[at + 1] = magenta ? 0 : 200
        ..[at + 2] = magenta ? 255 : 0
        ..[at + 3] = 255;
    }
  }
  return DesktopCursorShape(
    kind: DesktopCursorKind.custom,
    width: side,
    height: side,
    hotX: side ~/ 2,
    hotY: side ~/ 2,
    rgba: rgba,
  );
}

/// A system cursor whose session notes when the mouse tracker activates it.
class _TimedCursor extends MouseCursor {
  _TimedCursor(this.kind);

  final String kind;
  final activated = Completer<int>();

  @override
  MouseCursorSession createSession(int device) => _TimedSession(this, device);

  @override
  String get debugDescription => 'TimedCursor($kind)';
}

class _TimedSession extends MouseCursorSession {
  _TimedSession(_TimedCursor super.cursor, super.device);

  @override
  Future<void> activate() {
    final cursor = this.cursor as _TimedCursor;
    if (!cursor.activated.isCompleted) cursor.activated.complete(_now);
    return SystemChannels.mouseCursor.invokeMethod<void>(
      'activateSystemCursor',
      {'device': device, 'kind': cursor.kind},
    );
  }

  @override
  void dispose() {}
}
