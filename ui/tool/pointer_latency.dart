// How long a painted remote pointer took to follow the mouse in a desktop
// viewer, and how long a remote pointer shape change takes to reach this
// computer's pointer, both ways a viewer has done it:
//
// - `painted`: a hover event moves a 32-pixel bitmap painted in its own
//   layer, as RemoteCursorLayer painted the host's shape under a hidden
//   pointer before a native cursor took its place. Timed from the event to
//   the end of the raster of the frame that drew it (FrameTiming's
//   rasterFinish), on one clock: `Timeline.now` reads the clock the engine
//   stamps frame timings with. `painted_to_build` is the part up to that
//   frame's buildStart, `painted_build_to_raster` the rest. The picture
//   reaches the screen at a later refresh, which nothing here times.
// - `rebuild`: a ValueListenableBuilder rebuilds the MouseRegion with the new
//   cursor, and the mouse tracker activates it in the next frame's
//   post-frame pass, as RemotePointerRegion did before RemoteShapeCursor.
//   The change comes from a timer, as the store's 16 ms read of the host's
//   view does, so it lands anywhere in a refresh. Timed from the change to
//   the cursor session's activation, before its platform message is even
//   sent.
// - `rebuild_in_frame`: the same, the change made in a persistent frame
//   callback after the frame is drawn, as the store's read in every frame
//   (`_onFrame`) does, which then asks for another frame.
// - `system`: the call RemoteShapeCursor's session makes for a kind this
//   computer has (`activateSystemCursor`), made here directly, timed from the
//   call to the engine's reply.
// - `bitmap_set`: the call it makes for a bitmap the runner holds
//   (`cursor_set` on `pb_mapper/desktop_pointer`), made here directly, timed
//   from the call to the runner's reply, which comes after the cursor is set.
// - `bitmap_create`: the calls it makes for a bitmap the runner does not hold
//   yet: a 32-pixel host shape scaled to 64 pixels, made and shown
//   (`cursor_create`, then `cursor_set`), timed from before the scaling to
//   the second reply.
//
// The last three take turns, sample by sample, so whatever else loads the
// machine falls on each alike. Each change or event waits 20 to 60 ms after
// the last, so it lands anywhere in a display refresh, as a host's report or
// a mouse move does. The pointer is a synthetic mouse inside the window: the
// real one is never moved. Build it in release and run the app binary with
// the result's path:
//
//   flutter build macos --release -t tool/pointer_latency.dart
//   build/macos/Build/Products/Release/pb-mapper.app/Contents/MacOS/pb-mapper \
//     pointer_latency.json
//
// It prints the summary, writes it with every sample to the path given, and
// exits. Keep the window visible while it runs: macOS slows the frames and
// the main thread of a window it hides. The result lists the app's lifecycle
// states with when each began, and when each path began, to tell such a run.
import 'dart:async';
import 'dart:convert';
import 'dart:developer' show Timeline;
import 'dart:io';
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_pointer.dart';
import 'package:pb_mapper_ui/src/state/desktop_models.dart';

const _samples = 200;
const _device = 77;

final _clock = Stopwatch()..start();
int get _now => _clock.elapsedMicroseconds;
double get _nowMs => _now / 1000;

Future<void> main(List<String> args) async {
  WidgetsFlutterBinding.ensureInitialized();
  final out = args.isEmpty ? 'pointer_latency.json' : args.first;
  final lifecycle = <Map<String, Object>>[
    {
      'at_ms': _nowMs,
      'state': WidgetsBinding.instance.lifecycleState?.name ?? 'unknown',
    },
  ];
  final listener = AppLifecycleListener(
    onStateChange: (state) =>
        lifecycle.add({'at_ms': _nowMs, 'state': state.name}),
  );
  final began = <String, double>{};
  final shape = _disc(32);
  final image = await _image(shape);
  final cursor = ValueNotifier<MouseCursor>(_TimedCursor('text'));
  final hover = ValueNotifier<Offset?>(null);
  runApp(
    Directionality(
      textDirection: TextDirection.ltr,
      child: Listener(
        onPointerHover: (event) => hover.value = event.localPosition,
        child: Stack(
          fit: StackFit.expand,
          children: [
            ValueListenableBuilder<MouseCursor>(
              valueListenable: cursor,
              builder: (context, cursor, _) => MouseRegion(
                cursor: cursor,
                child: const ColoredBox(color: Color(0xFF202020)),
              ),
            ),
            IgnorePointer(
              child: RepaintBoundary(
                child: CustomPaint(
                  size: Size.infinite,
                  painter: _PaintedPointer(hover, image, shape),
                ),
              ),
            ),
          ],
        ),
      ),
    ),
  );
  await _frame();
  await Future<void>.delayed(const Duration(milliseconds: 500));
  final view = ui.PlatformDispatcher.instance.implicitView;
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

  began['painted'] = _nowMs;
  final painted = await _painted(centre, pause);

  began['rebuild'] = _nowMs;
  final rebuild = <int>[];
  for (var i = 0; i < _samples; i++) {
    await pause();
    final next = _TimedCursor(i.isEven ? 'click' : 'text');
    final start = _now;
    cursor.value = next;
    rebuild.add(await next.activated.future - start);
  }

  // The store's per-frame read: a persistent frame callback added after the
  // binding's own, so it runs once the frame is drawn.
  began['rebuild_in_frame'] = _nowMs;
  _TimedCursor? inFrame;
  var changed = 0;
  SchedulerBinding.instance.addPersistentFrameCallback((_) {
    final next = inFrame;
    if (next == null) return;
    inFrame = null;
    changed = _now;
    cursor.value = next;
    // A rebuild asked for after the frame is drawn asks for no frame.
    SchedulerBinding.instance.scheduleFrame();
  });
  final rebuildInFrame = <int>[];
  for (var i = 0; i < _samples; i++) {
    await pause();
    final next = _TimedCursor(i.isEven ? 'click' : 'text');
    inFrame = next;
    SchedulerBinding.instance.scheduleFrame();
    rebuildInFrame.add(await next.activated.future - changed);
  }

  // The calls that replace the rebuild take turns, so a stall from the rest
  // of the machine lands on all three alike.
  began['native_calls'] = _nowMs;
  final native = desktopPointerCursors;
  final dpr = view?.devicePixelRatio ?? 1;
  var id = 1 << 20;
  final held = [id++, id++];
  for (final made in held) {
    await native.create(
      made,
      scalePointerBitmap(shape, 2),
      devicePixelRatio: dpr,
    );
  }
  final system = <int>[], bitmapSet = <int>[], bitmapCreate = <int>[];
  for (var i = 0; i < _samples; i++) {
    await pause();
    var start = _now;
    await SystemChannels.mouseCursor.invokeMethod<void>(
      'activateSystemCursor',
      {'device': _device, 'kind': i.isEven ? 'click' : 'text'},
    );
    system.add(_now - start);

    await pause();
    start = _now;
    await native.set(held[i & 1]);
    bitmapSet.add(_now - start);

    await pause();
    final made = id++;
    start = _now;
    await native.create(
      made,
      scalePointerBitmap(shape, 2),
      devicePixelRatio: dpr,
    );
    await native.set(made);
    bitmapCreate.add(_now - start);
    await native.set(held[0]);
    await native.delete(made);
  }
  for (final made in held) {
    await native.delete(made);
  }
  began['end'] = _nowMs;
  listener.dispose();

  final display = ui.PlatformDispatcher.instance.displays.firstOrNull;
  final result = {
    'samples': _samples,
    'os': Platform.operatingSystemVersion,
    'processors': Platform.numberOfProcessors,
    'refresh_hz': display?.refreshRate,
    'device_pixel_ratio': view?.devicePixelRatio,
    'lifecycle': lifecycle,
    'began_ms': began,
    'summary_ms': {
      'painted': _summary(painted.total),
      'painted_to_build': _summary(painted.toBuild),
      'painted_build_to_raster': _summary(painted.frame),
      'rebuild': _summary(rebuild),
      'rebuild_in_frame': _summary(rebuildInFrame),
      'system': _summary(system),
      'bitmap_set': _summary(bitmapSet),
      'bitmap_create': _summary(bitmapCreate),
    },
    'samples_us': {
      'painted': painted.total,
      'painted_to_build': painted.toBuild,
      'painted_build_to_raster': painted.frame,
      'rebuild': rebuild,
      'rebuild_in_frame': rebuildInFrame,
      'system': system,
      'bitmap_set': bitmapSet,
      'bitmap_create': bitmapCreate,
    },
  };
  // ignore: avoid_print
  print(
    const JsonEncoder.withIndent('  ').convert({
      'lifecycle': lifecycle,
      'began_ms': began,
      'summary_ms': result['summary_ms'],
    }),
  );
  await File(
    out,
  ).writeAsString(const JsonEncoder.withIndent('  ').convert(result));
  exit(0);
}

/// The painted pointer: each hover event moves the bitmap, and the frame
/// that draws it is found by its number in the engine's frame timings.
/// Empty where this build's Dart VM keeps no timeline clock (`Timeline.now`
/// is then -1).
Future<({List<int> total, List<int> toBuild, List<int> frame})> _painted(
  Offset centre,
  Future<void> Function() pause,
) async {
  final timings = <int, ui.FrameTiming>{};
  void onTimings(List<ui.FrameTiming> reported) {
    for (final timing in reported) {
      timings[timing.frameNumber] = timing;
    }
  }

  final binding = SchedulerBinding.instance;
  binding.addTimingsCallback(onTimings);
  final samples = <_PaintedSample>[];
  for (var i = 0; i < _samples; i++) {
    await pause();
    final sample = _PaintedSample(Timeline.now);
    GestureBinding.instance.handlePointerEvent(
      PointerHoverEvent(
        kind: PointerDeviceKind.mouse,
        device: _device,
        position: centre + Offset(((i * 7) % 41) - 20.0, ((i * 3) % 21) - 10.0),
      ),
    );
    // The number of the frame the event asked for, once it is drawn; this
    // asks for no frame of its own.
    binding.addPostFrameCallback(
      (_) =>
          sample.frame = ui.PlatformDispatcher.instance.frameData.frameNumber,
    );
    samples.add(sample);
  }
  // The engine reports timings in batches, up to a second apart in release.
  for (var wait = 0; wait < 50; wait++) {
    if (samples.every((s) => timings.containsKey(s.frame))) break;
    await Future<void>.delayed(const Duration(milliseconds: 100));
    binding.scheduleFrame();
  }
  binding.removeTimingsCallback(onTimings);
  final total = <int>[], toBuild = <int>[], frame = <int>[];
  for (final sample in samples) {
    final timing = timings[sample.frame];
    if (timing == null || sample.event <= 0) continue;
    final build = timing.timestampInMicroseconds(ui.FramePhase.buildStart);
    final raster = timing.timestampInMicroseconds(ui.FramePhase.rasterFinish);
    total.add(raster - sample.event);
    toBuild.add(build - sample.event);
    frame.add(raster - build);
  }
  return (total: total, toBuild: toBuild, frame: frame);
}

final class _PaintedSample {
  _PaintedSample(this.event);

  /// When the event was handled, on the engine's frame timing clock.
  final int event;
  int frame = -1;
}

Map<String, Object> _summary(List<int> micros) {
  if (micros.isEmpty) return {'n': 0};
  final sorted = List.of(micros)..sort();
  double at(double q) =>
      sorted[math.min(sorted.length - 1, (q * sorted.length).floor())] / 1000;
  return {
    'n': sorted.length,
    'min': sorted.first / 1000,
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

Future<ui.Image> _image(DesktopCursorShape shape) {
  final done = Completer<ui.Image>();
  ui.decodeImageFromPixels(
    shape.rgba,
    shape.width,
    shape.height,
    ui.PixelFormat.rgba8888,
    done.complete,
  );
  return done.future;
}

/// The host's bitmap at the hover point, its hotspot there, drawn as
/// RemoteCursorPainter draws it.
class _PaintedPointer extends CustomPainter {
  _PaintedPointer(this.at, this.image, this.shape) : super(repaint: at);

  final ValueListenable<Offset?> at;
  final ui.Image image;
  final DesktopCursorShape shape;

  static final _bitmap = Paint()..filterQuality = FilterQuality.medium;

  @override
  void paint(Canvas canvas, Size _) {
    final point = at.value;
    if (point == null) return;
    final size = Size(shape.width.toDouble(), shape.height.toDouble());
    canvas.drawImageRect(
      image,
      Offset.zero & size,
      (point - Offset(shape.hotX.toDouble(), shape.hotY.toDouble())) & size,
      _bitmap,
    );
  }

  @override
  bool shouldRepaint(covariant _PaintedPointer oldDelegate) =>
      oldDelegate.at != at;
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
