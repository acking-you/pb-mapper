// How many frames a desktop viewer in control draws, and how often it reads
// the view leaf, while the mouse sweeps over a still remote picture whose
// pointer this computer shows as its own (the host's text beam, a kind every
// OS has), so the cursor layer paints nothing; and the same at rest.
//
// The viewer is the app's own: a DesktopStore over the tests' scriptable
// FakeDesktopApi in place of the Rust viewer, the real RemoteInput with its
// move gate and the view leaf's reads, RemotePointer, RemotePointerRegion and
// RemoteCursorLayer over a still picture, in the real runner. Every 8 ms the
// pointer moves along a line across the picture as a desktop viewer's hover
// moves it (`RemoteInput.hover`, what `RemoteSurface` calls for each hover
// event), as a mouse whose events the system coalesces to about 125 a second
// would; the local pointer counts as over the picture throughout. Nothing
// moves the real pointer. Each window lasts 5 s:
//
// - `sweep`: the hovers; `rest`: none.
// - `frames_drawn`: Flutter frames begun (a persistent frame callback);
//   `frame_timings`: FrameTiming reports of the frames numbered in the
//   window, counted once the engine has sent them all.
// - `view_reads`: reads of the view leaf, the frame's, the timer's and the
//   move gate's alike; `moves_sent`: input batches the store handed on.
//
// Build it in release and run the app with the result's path, its window in
// front (macOS draws no frames for a window it does not show):
//
//   flutter build macos --release -t tool/hover_frames.dart
//   open -W -n -a build/macos/Build/Products/Release/pb-mapper.app \
//     --args "$PWD/hover_frames.json"
//
// It writes the result to the path given and exits.
import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/modifier_latch.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_cursor_layer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_input.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_viewport.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';

import '../test/fake_desktop_api.dart';

const _window = Duration(seconds: 5);
const _hoverEvery = Duration(milliseconds: 8);

final _clock = Stopwatch()..start();
int get _nowUs => _clock.elapsedMicroseconds;

Future<void> main(List<String> args) async {
  WidgetsFlutterBinding.ensureInitialized();
  final out = args.isEmpty ? 'hover_frames.json' : args.first;
  final lifecycle = <String>[
    WidgetsBinding.instance.lifecycleState?.name ?? 'unknown',
  ];
  final listener = AppLifecycleListener(
    onStateChange: (state) => lifecycle.add(state.name),
  );

  final api = FakeDesktopApi();
  api.cursorShapes[1] = DesktopCursorShape(
    kind: DesktopCursorKind.ibeam,
    width: 1,
    height: 1,
    hotX: 0,
    hotY: 0,
    rgba: Uint8List(4),
  );
  final store = DesktopStore(api);
  final demand = store.demand(DesktopDemand.viewer);
  await store.connect(
    const DesktopTarget(invitation: 'pbd1.viewer', name: 'Viewer'),
  );
  api.viewerStatusValue = liveStatus(controlling: true);
  // Connecting cleared the view: the host's pointer now, in its text beam.
  api.viewValue = const DesktopView(
    generation: 1,
    presentedFrameId: 1,
    cursorSeq: 1,
    displayWidth: 1920,
    displayHeight: 1080,
    videoWidth: 1920,
    videoHeight: 1080,
    cursorX: 960,
    cursorY: 540,
    cursorShape: 1,
    state: 2,
    cursorVisible: true,
    controlling: true,
    controlAllowed: true,
    fresh: true,
  );
  final latch = ModifierLatch();
  final input = RemoteInput(desktop: store, latch: latch, onLocalCancel: () {});
  final viewport = RemoteViewport()..aspectRatio = 1920 / 1080;
  final pointer = RemotePointer(
    mouse: true,
    cursor: store.cursor,
    controlling: input.ready,
    geometry: store.geometry,
    viewport: viewport,
  );
  runApp(
    Directionality(
      textDirection: TextDirection.ltr,
      child: LayoutBuilder(
        builder: (context, constraints) {
          viewport.layout(constraints.biggest);
          return Stack(
            fit: StackFit.expand,
            children: [
              RemotePointerRegion(
                pointer: pointer,
                child: const ColoredBox(color: Color(0xFF202020)),
              ),
              RemoteCursorLayer(
                viewport: viewport,
                input: input,
                pointer: pointer,
              ),
            ],
          );
        },
      ),
    ),
  );
  // Live and in control, the host's text beam named, the local pointer over
  // the picture where the host's is.
  for (var wait = 0; wait < 100 && !input.ready.value; wait++) {
    await Future<void>.delayed(const Duration(milliseconds: 50));
  }
  pointer.over = true;
  input.hover(const Offset(0.5, 0.5));
  await Future<void>.delayed(const Duration(seconds: 1));
  final ready = {
    'input_ready': input.ready.value,
    'shape': store.cursor.shape?.definition.kind.name,
    'paint': pointer.value.paint.name,
  };

  // Frames begun; the engine's timings, by frame number.
  var begun = 0;
  SchedulerBinding.instance.addPersistentFrameCallback((_) => begun++);
  final timings = <int>{};
  SchedulerBinding.instance.addTimingsCallback((reported) {
    for (final timing in reported) {
      timings.add(timing.frameNumber);
    }
  });
  int reads() => api.ops.where((op) => op == 'view').length;
  int moves() => api.ops.where((op) => op == 'input').length;

  Future<Map<String, Object>> measure(
    String name, {
    required bool hover,
  }) async {
    final startUs = _nowUs;
    final startFrames = begun;
    final startReads = reads(), startMoves = moves();
    // The engine's timestamps are on another clock: the window is bounded by
    // frame numbers instead.
    final firstFrame = ui.PlatformDispatcher.instance.frameData.frameNumber;
    var events = 0;
    Timer? sweep;
    if (hover) {
      sweep = Timer.periodic(_hoverEvery, (_) {
        final t = (events % 250) / 250;
        final x = 0.2 + 0.6 * (t < 0.5 ? 2 * t : 2 - 2 * t);
        input.hover(Offset(x, 0.5 + ((events % 7) - 3) / 1080));
        events++;
      });
    }
    await Future<void>.delayed(_window);
    sweep?.cancel();
    final seconds = (_nowUs - startUs) / 1e6;
    final frames = begun - startFrames;
    final lastFrame = ui.PlatformDispatcher.instance.frameData.frameNumber;
    return {
      'window': name,
      'seconds': seconds,
      'hovers': events,
      'frames_drawn': frames,
      'frames_drawn_per_s': frames / seconds,
      'view_reads_per_s': (reads() - startReads) / seconds,
      'moves_sent_per_s': (moves() - startMoves) / seconds,
      'first_frame_number': firstFrame,
      'last_frame_number': lastFrame,
    };
  }

  final windows = [
    await measure('rest', hover: false),
    await measure('sweep', hover: true),
    await measure('rest', hover: false),
    await measure('sweep', hover: true),
  ];
  // The engine sends timings in batches, up to a second apart in release.
  SchedulerBinding.instance.scheduleFrame();
  await Future<void>.delayed(const Duration(milliseconds: 1500));
  for (final window in windows) {
    final first = window['first_frame_number']! as int;
    final last = window['last_frame_number']! as int;
    final count = timings.where((n) => n > first && n <= last).length;
    window['frame_timings'] = count;
    window['frame_timings_per_s'] = count / (window['seconds']! as double);
  }
  listener.dispose();
  final view = ui.PlatformDispatcher.instance.implicitView;
  final display = ui.PlatformDispatcher.instance.displays.firstOrNull;
  final result = {
    'os': Platform.operatingSystemVersion,
    'processors': Platform.numberOfProcessors,
    'refresh_hz': display?.refreshRate,
    'device_pixel_ratio': view?.devicePixelRatio,
    'hover_every_ms': _hoverEvery.inMilliseconds,
    'lifecycle': lifecycle,
    'ready': ready,
    'windows': windows,
  };
  final text = const JsonEncoder.withIndent('  ').convert(result);
  debugPrint(text);
  await File(out).writeAsString(text);
  pointer.dispose();
  input.dispose();
  demand.dispose();
  exit(0);
}
