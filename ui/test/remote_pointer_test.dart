import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_pointer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_cursor_layer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_menus.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_stage.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';

import 'fake_desktop_pointer.dart';
import 'remote_viewer_harness.dart';

/// A [side]-pixel square shape, its hotspot at ([hot], [hot]); [tint] in
/// its first byte tells apart shapes otherwise alike.
DesktopCursorShape _shape(
  DesktopCursorKind kind, {
  int side = 1,
  int hot = 0,
  int tint = 0,
}) => DesktopCursorShape(
  kind: kind,
  width: side,
  height: side,
  hotX: hot,
  hotY: hot,
  rgba: Uint8List(side * side * 4)..[0] = tint,
);

/// What this computer's pointer was told: each system kind through
/// `SystemChannels.mouseCursor` as `kind <name>`, each runner call through
/// `pb_mapper/desktop_pointer` as `<method> <arguments>`, and the scheduler
/// phase each came in.
class _PointerLog {
  _PointerLog(WidgetTester tester) {
    final messenger = tester.binding.defaultBinaryMessenger;
    messenger
      ..setMockMethodCallHandler(SystemChannels.mouseCursor, (call) async {
        _add('kind ${(call.arguments as Map)['kind']}');
        return null;
      })
      ..setMockMethodCallHandler(_runner, (call) async {
        final args = Map.of(call.arguments as Map);
        final rgba = args.remove('rgba');
        if (rgba is Uint8List) {
          args['bytes'] = rgba.length;
          bitmaps.add(rgba);
        }
        _add('${call.method} $args');
        return null;
      });
    addTearDown(() {
      messenger
        ..setMockMethodCallHandler(SystemChannels.mouseCursor, null)
        ..setMockMethodCallHandler(_runner, null);
    });
  }

  static const _runner = MethodChannel('pb_mapper/desktop_pointer');

  final calls = <String>[];
  final phases = <SchedulerPhase>[];
  final bitmaps = <Uint8List>[];

  void _add(String call) {
    calls.add(call);
    phases.add(SchedulerBinding.instance.schedulerPhase);
  }

  /// The system kind told last.
  String? get kind {
    for (final call in calls.reversed) {
      if (call.startsWith('kind ')) return call.substring(5);
    }
    return null;
  }

  /// The calls since the last time this was asked, none of them made by
  /// the mouse tracker after a frame: each came the moment its change did.
  List<String> take() {
    expect(phases, everyElement(isNot(SchedulerPhase.postFrameCallbacks)));
    final taken = List.of(calls);
    calls.clear();
    phases.clear();
    return taken;
  }
}

/// A viewer in control of a 1920×1080 display, its picture filling a
/// 1920×1080 logical view at a device pixel ratio of 2, and the local mouse
/// over its middle. [report] makes the host report its pointer there, in
/// shape [report]'s `shape`, as the first report and each after the local
/// pointer moved it.
class _Scene {
  _Scene._(this.h, this.pointer, this.log, this.mouse);

  static Future<_Scene> open(
    WidgetTester tester, {
    DesktopPointerCursors? native,
  }) async {
    tester.view
      ..physicalSize = const Size(3840, 2160)
      ..devicePixelRatio = 2;
    addTearDown(tester.view.reset);
    final log = _PointerLog(tester);
    final h = await ViewerHarness.live(tester);
    h.viewport.layout(const Size(1920, 1080));
    final pointer = RemotePointer(
      mouse: true,
      cursor: h.store.cursor,
      controlling: h.input.ready,
      geometry: h.store.geometry,
      viewport: h.viewport,
      native: native,
    );
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Stack(
          fit: StackFit.expand,
          children: [
            RemotePointerRegion(
              pointer: pointer,
              child: const SizedBox.expand(),
            ),
            RemoteCursorLayer(
              viewport: h.viewport,
              input: h.input,
              pointer: pointer,
            ),
          ],
        ),
      ),
    );
    final mouse = await tester.createGesture(
      kind: PointerDeviceKind.mouse,
      pointer: 1,
    );
    await mouse.addPointer(location: _at);
    final scene = _Scene._(h, pointer, log, mouse);
    scene._tester = tester;
    await scene.report(shape: 0);
    h.input.hover(h.viewport.toNormalized(_at)!);
    await tester.pump(const Duration(milliseconds: 16));
    log.take();
    return scene;
  }

  static const _at = Offset(960, 540);

  final ViewerHarness h;
  final RemotePointer pointer;
  final _PointerLog log;
  final TestGesture mouse;
  late final WidgetTester _tester;

  Future<void> report({required int shape}) async {
    h.api.viewValue = DesktopView(
      generation: 1,
      presentedFrameId: 1,
      cursorSeq: 1,
      displayWidth: 1920,
      displayHeight: 1080,
      videoWidth: 1920,
      videoHeight: 1080,
      cursorX: 960,
      cursorY: 540,
      cursorShape: shape,
      state: 2,
      cursorVisible: true,
      controlling: true,
      controlAllowed: true,
      fresh: true,
    );
    await _tester.pump(const Duration(milliseconds: 16));
  }

  Finder get layer => find.byType(RemoteCursorLayer);

  Future<void> close() async {
    await mouse.removePointer();
    await _tester.pumpWidget(const SizedBox());
    pointer.dispose();
    h.dispose();
  }
}

void main() {
  test('a touch viewer paints the host shape; a host without shapes keeps '
      'the arrow', () {
    RemotePointerLook look({required bool mouse, DesktopCursorKind? kind}) =>
        RemotePointerLook.of(
          mouse: mouse,
          controlling: true,
          atLocalPointer: true,
          kind: kind,
        );
    expect(
      look(mouse: false, kind: DesktopCursorKind.ibeam),
      const RemotePointerLook(MouseCursor.defer, RemotePointerPaint.shape),
    );
    expect(look(mouse: true), RemotePointerLook.generic);
    expect(look(mouse: false), RemotePointerLook.generic);
  });

  test('in control, the local pointer is the system one where this OS has '
      'it, else the host bitmap where the runner can show it', () {
    RemotePointerLook at(
      DesktopCursorKind kind,
      TargetPlatform platform, {
      bool nativeBitmap = true,
      bool atLocalPointer = true,
    }) => RemotePointerLook.of(
      mouse: true,
      controlling: true,
      atLocalPointer: atLocalPointer,
      kind: kind,
      nativeBitmap: nativeBitmap,
      platform: platform,
    );
    const bitmap = RemotePointerLook(null, RemotePointerPaint.none);
    const windows = TargetPlatform.windows, mac = TargetPlatform.macOS;

    // A Windows viewer has a busy pointer; a Mac's embedder shows any kind
    // it lacks as the arrow, so the host's bitmap shows it there.
    expect(
      at(DesktopCursorKind.wait, windows),
      const RemotePointerLook(SystemMouseCursors.wait, RemotePointerPaint.none),
    );
    for (final kind in [
      DesktopCursorKind.wait,
      DesktopCursorKind.progress,
      DesktopCursorKind.help,
      DesktopCursorKind.move,
      DesktopCursorKind.resizeNwse,
      DesktopCursorKind.resizeNesw,
      DesktopCursorKind.upArrow,
      DesktopCursorKind.custom,
    ]) {
      expect(at(kind, mac), bitmap, reason: '$kind');
    }
    expect(
      at(DesktopCursorKind.ibeam, mac),
      const RemotePointerLook(SystemMouseCursors.text, RemotePointerPaint.none),
    );
    expect(at(DesktopCursorKind.custom, TargetPlatform.linux), bitmap);
    expect(at(DesktopCursorKind.upArrow, windows), bitmap);

    // Without the runner, the bitmap is painted under a hidden pointer.
    expect(
      at(DesktopCursorKind.custom, windows, nativeBitmap: false),
      const RemotePointerLook(
        SystemMouseCursors.none,
        RemotePointerPaint.shape,
      ),
    );
    // The host's pointer elsewhere: painted there, and the local pointer
    // the nearest this OS has.
    expect(
      at(DesktopCursorKind.wait, mac, atLocalPointer: false),
      const RemotePointerLook(
        SystemMouseCursors.basic,
        RemotePointerPaint.shape,
      ),
    );
  });

  test('a pointer bitmap enlarged by a whole factor keeps its edges, and a '
      'reduction lends transparent pixels no colour', () {
    const red = [255, 0, 0, 255], blue = [0, 0, 255, 255];
    final pair = DesktopCursorShape(
      kind: DesktopCursorKind.custom,
      width: 2,
      height: 1,
      hotX: 1,
      hotY: 0,
      rgba: Uint8List.fromList([...red, ...blue]),
    );
    expect(identical(scalePointerBitmap(pair, 1).rgba, pair.rgba), isTrue);
    final doubled = scalePointerBitmap(pair, 2);
    expect((doubled.width, doubled.height), (4, 2));
    expect((doubled.hotX, doubled.hotY), (2, 0));
    expect(doubled.rgba, [
      for (var row = 0; row < 2; row++) ...[...red, ...red, ...blue, ...blue],
    ]);

    // Opaque white beside a transparent black: half of a white pixel, not
    // a grey one.
    final edge = DesktopCursorShape(
      kind: DesktopCursorKind.custom,
      width: 2,
      height: 1,
      hotX: 0,
      hotY: 0,
      rgba: Uint8List.fromList([255, 255, 255, 255, 0, 0, 0, 0]),
    );
    final halved = scalePointerBitmap(edge, 0.5);
    expect((halved.width, halved.height), (1, 1));
    expect(halved.rgba, [255, 255, 255, 128]);
  });

  testWidgets('a desktop viewer in control shows one pointer in the host '
      'shape, and paints the host pointer only where it is elsewhere; '
      'without the runner, a shape this OS lacks is painted under a hidden '
      'pointer', (tester) async {
    final log = _PointerLog(tester);
    final h = await ViewerHarness.live(tester);
    h.viewport.layout(const Size(800, 450));
    h.api.cursorShapes.addAll({
      1: _shape(DesktopCursorKind.ibeam),
      2: _shape(DesktopCursorKind.custom),
    });
    Future<void> report({required int shape, int x = 480, int seq = 1}) async {
      h.api.viewValue = DesktopView(
        generation: 1,
        presentedFrameId: 1,
        cursorSeq: seq,
        displayWidth: 1920,
        displayHeight: 1080,
        videoWidth: 1920,
        videoHeight: 1080,
        cursorX: x,
        cursorY: 540,
        cursorShape: shape,
        state: 2,
        cursorVisible: true,
        controlling: true,
        controlAllowed: true,
        fresh: true,
      );
      await tester.pump(const Duration(milliseconds: 16));
    }

    // The test's own platform, which has no runner for host bitmaps.
    final pointer = RemotePointer(
      mouse: true,
      cursor: h.store.cursor,
      controlling: h.input.ready,
      geometry: h.store.geometry,
      viewport: h.viewport,
    );
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Stack(
          fit: StackFit.expand,
          children: [
            RemotePointerRegion(
              pointer: pointer,
              child: const SizedBox.expand(),
            ),
            RemoteCursorLayer(
              viewport: h.viewport,
              input: h.input,
              pointer: pointer,
            ),
          ],
        ),
      ),
    );
    final layer = find.byType(RemoteCursorLayer);
    // A point inside the arrow, whose tip is at the cursor.
    PaintPattern arrowAt(Offset n) =>
        paints..path(includes: [h.viewport.toLocal(n) + const Offset(2, 6)]);
    final mouse = await tester.createGesture(
      kind: PointerDeviceKind.mouse,
      pointer: 1,
    );
    const at = Offset(400, 225);
    await mouse.addPointer(location: at);
    addTearDown(mouse.removePointer);
    expect(
      RendererBinding.instance.mouseTracker.debugDeviceActiveCursor(1),
      pointer.shapeCursor,
    );

    // A host that sends no shape: the arrow, painted, as before.
    await report(shape: 0);
    expect(layer, arrowAt(const Offset(0.25, 0.5)));
    expect(log.kind, 'basic');

    // The local pointer moves the host's, which turns a text beam: the
    // local pointer shows it, and nothing is painted under it.
    h.input.hover(h.viewport.toNormalized(at)!);
    await report(shape: 1);
    expect(log.kind, 'text');
    expect(layer, isNot(paints..path()));

    // Someone at the host moves it away: painted there, the local pointer
    // keeps the shape.
    await report(shape: 1, x: 1440, seq: 2);
    expect(layer, arrowAt(const Offset(0.75, 0.5)));
    expect(log.kind, 'text');

    // Back under the local pointer, a shape the viewer's OS has not: the
    // local pointer hides, and the shape is painted in its place.
    h.input.hover(h.viewport.toNormalized(at)!);
    await report(shape: 2, x: 1440, seq: 2);
    expect(log.kind, 'none');
    expect(layer, arrowAt(const Offset(0.5, 0.5)));
    expect(log.take().where((c) => !c.startsWith('kind ')), isEmpty);

    // A definition that comes back is the one decoded before.
    await report(shape: 1, x: 1440, seq: 2);
    final ibeam = h.store.cursor.shape;
    expect(ibeam?.definition.kind, DesktopCursorKind.ibeam);
    await report(shape: 2, x: 1440, seq: 2);
    await report(shape: 1, x: 1440, seq: 2);
    expect(h.store.cursor.shape, same(ibeam));

    // Once the viewer's counter has gone round, a number names another
    // definition: that one shows, not the one decoded for it before.
    h.api.cursorShapes[2] = _shape(DesktopCursorKind.hand);
    await report(shape: 2, x: 1440, seq: 2);
    expect(h.store.cursor.shape?.definition.kind, DesktopCursorKind.hand);
    expect(log.kind, 'click');

    // Viewing only: the local pointer is its own, the host's is painted.
    h.store.control(false);
    await tester.pump(const Duration(milliseconds: 500));
    expect(
      RendererBinding.instance.mouseTracker.debugDeviceActiveCursor(1),
      SystemMouseCursors.basic,
    );
    expect(layer, paints..path());

    pointer.dispose();
    h.dispose();
  });

  testWidgets(
    'a host shape this OS lacks becomes its own pointer the moment it '
    'changes, with no rebuild, and the runner keeps it per scale',
    (tester) async {
      final scene = await _Scene.open(tester);
      final _Scene(:h, :log, :pointer) = scene;
      h.api.cursorShapes.addAll({
        1: _shape(DesktopCursorKind.ibeam),
        2: _shape(DesktopCursorKind.custom, side: 32, hot: 16),
      });
      final rebuilt = <Widget>[];
      debugOnRebuildDirtyWidget = (element, _) => rebuilt.add(element.widget);
      try {
        await scene.report(shape: 1);
        expect(log.take(), ['kind text']);

        // The host's own bitmap at the picture's scale, 1, in this
        // computer's physical pixels, shown at once.
        await scene.report(shape: 2);
        final custom = log.take();
        expect(custom, [
          'cursor_create {id: ${pointer.debugBitmapIds.last}, width: 64, '
              'height: 64, hot_x: 32, hot_y: 32, scale: 2.0, bytes: 16384}',
          'cursor_set {id: ${pointer.debugBitmapIds.last}}',
        ]);
        expect(pointer.value.paint, RemotePointerPaint.none);
        expect(scene.layer, isNot(paints..path()));
        expect(scene.layer, isNot(paints..drawImageRect()));

        // Back and forth: the runner's bitmap is shown again, not made
        // again.
        await scene.report(shape: 1);
        expect(log.take(), ['kind text']);
        await scene.report(shape: 2);
        expect(log.take(), [custom.last]);
      } finally {
        debugOnRebuildDirtyWidget = null;
      }
      expect(rebuilt, isEmpty);

      // Zoomed in: the bitmap grows with the picture.
      h.viewport.zoomAt(const Offset(960, 540), 1.5);
      expect(log.take(), [
        'cursor_create {id: ${pointer.debugBitmapIds.last}, width: 96, '
            'height: 96, hot_x: 48, hot_y: 48, scale: 2.0, bytes: 36864}',
        'cursor_set {id: ${pointer.debugBitmapIds.last}}',
      ]);

      // The viewer closes: the runner frees both.
      final ids = List.of(pointer.debugBitmapIds);
      await scene.close();
      expect(log.calls, [for (final id in ids) 'cursor_delete {id: $id}']);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.windows),
  );

  testWidgets(
    "a Mac viewer shows a host's busy pointer as the host's bitmap, which "
    'a Windows viewer has as its own',
    (tester) async {
      final runner = FakeDesktopPointer();
      final scene = await _Scene.open(tester, native: runner);
      scene.h.api.cursorShapes[1] = _shape(
        DesktopCursorKind.wait,
        side: 32,
        hot: 16,
      );
      await scene.report(shape: 1);
      if (defaultTargetPlatform == TargetPlatform.macOS) {
        expect(runner.take(), ['create #1 64x64 @32,32 dpr 2.0', 'set #1']);
        expect(scene.log.take(), isEmpty);
      } else {
        expect(runner.take(), isEmpty);
        expect(scene.log.take(), ['kind wait']);
      }
      expect(scene.pointer.value.paint, RemotePointerPaint.none);
      await scene.close();
    },
    variant: TargetPlatformVariant({
      TargetPlatform.macOS,
      TargetPlatform.windows,
    }),
  );

  testWidgets(
    'a bitmap is kept per device pixel ratio: the same pixels at another '
    'ratio are another size on a Mac',
    (tester) async {
      final runner = FakeDesktopPointer();
      final scene = await _Scene.open(tester, native: runner);
      scene.h.api.cursorShapes[1] = _shape(
        DesktopCursorKind.custom,
        side: 32,
        hot: 16,
      );
      await scene.report(shape: 1);
      expect(runner.take(), ['create #1 64x64 @32,32 dpr 2.0', 'set #1']);

      // The window moves to a display at 1x: the same logical size, so
      // half the pixels.
      tester.view
        ..physicalSize = const Size(1920, 1080)
        ..devicePixelRatio = 1;
      await tester.pump();
      expect(runner.take(), ['create #2 32x32 @16,16 dpr 1.0', 'set #2']);

      // Zoomed in twice there: as many pixels as the first bitmap, made
      // for 2x, which would show half as large; a new one, made for 1x.
      scene.h.viewport.zoomAt(const Offset(960, 540), 2);
      expect(runner.take(), ['create #3 64x64 @32,32 dpr 1.0', 'set #3']);

      // Back on the 2x display, unzoomed: the first is shown again.
      scene.h.viewport.zoomAt(const Offset(960, 540), 0.5);
      tester.view
        ..physicalSize = const Size(3840, 2160)
        ..devicePixelRatio = 2;
      await tester.pump();
      expect(runner.take(), ['set #2', 'set #1']);
      await scene.close();
    },
    variant: TargetPlatformVariant.only(TargetPlatform.macOS),
  );

  testWidgets(
    'the runner holds at most 16 bitmaps, frees the oldest, and frees '
    'them all when the viewer closes',
    (tester) async {
      final runner = FakeDesktopPointer();
      final scene = await _Scene.open(tester, native: runner);
      for (var n = 1; n <= kNativeBitmapCache + 1; n++) {
        scene.h.api.cursorShapes[n] = _shape(
          DesktopCursorKind.custom,
          side: 2,
          tint: n,
        );
        await scene.report(shape: n);
      }
      final calls = runner.take();
      expect(calls.where((c) => c.startsWith('create ')), hasLength(17));
      expect(calls.where((c) => c.startsWith('delete ')), ['delete #1']);
      expect(calls.last, 'set #17');
      expect(runner.held, hasLength(kNativeBitmapCache));

      // Leaving the picture, the session ends: a new shape makes nothing
      // until the pointer is back.
      scene.h.api.cursorShapes[18] = _shape(
        DesktopCursorKind.custom,
        side: 2,
        tint: 18,
      );
      await scene.mouse.removePointer();
      await scene.report(shape: 18);
      expect(runner.take(), isEmpty);
      await scene.mouse.addPointer(location: const Offset(960, 540));
      await tester.pump();
      expect(runner.take().last, startsWith('set '));

      await scene.close();
      expect(runner.held, isEmpty);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.linux),
  );

  testWidgets(
    'hovering while the local pointer shows the host shape asks for no '
    'frame; hovering with the shape painted repaints it',
    (tester) async {
      final runner = FakeDesktopPointer()..fail = true;
      final scene = await _Scene.open(tester, native: runner);
      final _Scene(:h, :pointer) = scene;
      h.api.cursorShapes.addAll({
        1: _shape(DesktopCursorKind.ibeam),
        2: _shape(DesktopCursorKind.custom),
      });
      Future<void> sweep({required bool frames}) async {
        for (var i = 1; i <= 24; i++) {
          h.input.hover(
            h.viewport.toNormalized(_Scene._at + Offset(i * 4.0, i * 2.0))!,
          );
          expect(tester.binding.hasScheduledFrame, frames, reason: 'hover $i');
          if (frames) await tester.pump();
          // The move gate closes and reads the view; nothing in it changed.
          await tester.binding.delayed(const Duration(milliseconds: 2));
          expect(tester.binding.hasScheduledFrame, isFalse, reason: 'read $i');
        }
      }

      // The text beam is this computer's own pointer: nothing is painted,
      // and a hover is no frame, the view's reads included.
      await scene.report(shape: 1);
      expect(pointer.value.paint, RemotePointerPaint.none);
      expect(tester.binding.hasScheduledFrame, isFalse);
      await sweep(frames: false);
      expect(scene.layer, isNot(paints..path()));

      // A shape the runner cannot show is painted at the pointer: each
      // hover repaints it there.
      await scene.report(shape: 2);
      await tester.pump();
      expect(pointer.value.paint, RemotePointerPaint.shape);
      await sweep(frames: true);
      expect(scene.layer, paints..path());
      await scene.close();
    },
    variant: TargetPlatformVariant.only(TargetPlatform.windows),
  );

  testWidgets(
    'a runner that cannot show the bitmap leaves it painted under a hidden '
    'pointer',
    (tester) async {
      final runner = FakeDesktopPointer()..fail = true;
      final scene = await _Scene.open(tester, native: runner);
      scene.h.api.cursorShapes[1] = _shape(DesktopCursorKind.custom);
      await scene.report(shape: 1);
      await tester.pump();
      expect(
        scene.pointer.value,
        const RemotePointerLook(
          SystemMouseCursors.none,
          RemotePointerPaint.shape,
        ),
      );
      expect(scene.log.kind, 'none');
      expect(scene.layer, paints..path());
      await scene.close();
    },
    variant: TargetPlatformVariant.only(TargetPlatform.windows),
  );

  testWidgets('the local pointer takes the host shape only over the picture '
      'itself, never over the bars beside it', (tester) async {
    // A 16:9 picture in a wider stage: 800 wide, between bars 200 wide.
    tester.view
      ..physicalSize = const Size(1200, 450)
      ..devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final log = _PointerLog(tester);
    final h = await ViewerHarness.live(tester);
    h.api.cursorShapes[1] = _shape(DesktopCursorKind.custom);
    h.api.viewValue = const DesktopView(
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
    final prefs = ViewerPrefs(persist: false);
    await tester.pumpWidget(
      MaterialApp(
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: Builder(
          builder: (context) => Theme(
            data: viewerTheme(context),
            child: ViewerStage.of(
              h.store,
              viewport: h.viewport,
              input: h.input,
              machine: h.machine,
              touchMode: h.touchMode,
              title: 'Office',
              actions: ViewerActions(
                desktop: h.store,
                input: h.input,
                prefs: prefs,
                viewport: h.viewport,
                onDisconnect: () async {},
                onQuality: (_) {},
                onResolution: (_) {},
                onMaxFps: (_) {},
                onDisplayMode: (_) {},
                onTypeText: () {},
              ),
              localPointer: true,
            ),
          ),
        ),
      ),
    );
    await tester.pump(const Duration(milliseconds: 16));
    MouseCursor? local() =>
        RendererBinding.instance.mouseTracker.debugDeviceActiveCursor(1);
    final mouse = await tester.createGesture(
      kind: PointerDeviceKind.mouse,
      pointer: 1,
    );
    await mouse.addPointer(location: const Offset(600, 225));
    addTearDown(mouse.removePointer);
    await mouse.moveTo(const Offset(610, 225));
    await tester.pump(const Duration(milliseconds: 16));
    expect(h.store.cursor.shape?.definition.kind, DesktopCursorKind.custom);
    expect(local(), isA<RemoteShapeCursor>());
    expect(log.kind, 'none');

    // Over the bar beside the picture: the local pointer is its own again.
    await mouse.moveTo(const Offset(100, 225));
    await tester.pump(const Duration(milliseconds: 16));
    expect(local(), SystemMouseCursors.basic);
    expect(log.kind, 'basic');

    await mouse.moveTo(const Offset(610, 225));
    await tester.pump(const Duration(milliseconds: 16));
    expect(local(), isA<RemoteShapeCursor>());
    expect(log.kind, 'none');

    await tester.pumpWidget(const SizedBox());
    prefs.dispose();
    h.dispose();
  });

  testWidgets(
    'peer cursors paint in distinct colours on an isolated layer and tear down '
    'to zero cost when no peer controls',
    (tester) async {
      tester.view
        ..physicalSize = const Size(800, 450)
        ..devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final h = await ViewerHarness.live(tester);
      final prefs = ViewerPrefs(persist: false);
      DesktopView viewWithPeers({
        required int seq,
        required List<DesktopPeerCursor> peers,
      }) => DesktopView(
        generation: 1,
        presentedFrameId: 1,
        cursorSeq: 1,
        displayWidth: 1920,
        displayHeight: 1080,
        videoWidth: 1920,
        videoHeight: 1080,
        cursorX: 960,
        cursorY: 540,
        state: 2,
        cursorVisible: true,
        controlling: true,
        controlAllowed: true,
        fresh: true,
        peerCursorSeq: seq,
        peerCursors: peers,
      );

      h.api.viewValue = viewWithPeers(seq: 0, peers: const []);
      await tester.pumpWidget(
        MaterialApp(
          localizationsDelegates: AppLocalizations.localizationsDelegates,
          supportedLocales: AppLocalizations.supportedLocales,
          home: Builder(
            builder: (context) => Theme(
              data: viewerTheme(context),
              child: ViewerStage.of(
                h.store,
                viewport: h.viewport,
                input: h.input,
                machine: h.machine,
                touchMode: h.touchMode,
                title: 'Office',
                actions: ViewerActions(
                  desktop: h.store,
                  input: h.input,
                  prefs: prefs,
                  viewport: h.viewport,
                  onDisconnect: () async {},
                  onQuality: (_) {},
                  onResolution: (_) {},
                  onMaxFps: (_) {},
                  onDisplayMode: (_) {},
                  onTypeText: () {},
                ),
              ),
            ),
          ),
        ),
      );
      await tester.pump(const Duration(milliseconds: 16));

      final peerLayer = find.byType(RemotePeerCursorLayer);
      expect(
        find.descendant(of: peerLayer, matching: find.byType(CustomPaint)),
        findsNothing,
      );

      // Two peer controllers appear with distinct palette indices.
      h.api.viewValue = viewWithPeers(
        seq: 1,
        peers: const [
          DesktopPeerCursor(color: 0, x: 480, y: 540),
          DesktopPeerCursor(color: 2, x: 1440, y: 270),
        ],
      );
      await tester.pump(const Duration(milliseconds: 16));
      await tester.pump();
      expect(
        peerLayer,
        paints
          ..path(
            style: PaintingStyle.fill,
            color: peerCursorColor(0),
            includes: [
              h.viewport.toLocal(const Offset(0.25, 0.5)) + const Offset(2, 6),
            ],
          )
          ..path(style: PaintingStyle.stroke, color: const Color(0xFFFFFFFF))
          ..path(
            style: PaintingStyle.fill,
            color: peerCursorColor(2),
            includes: [
              h.viewport.toLocal(const Offset(0.75, 0.25)) + const Offset(2, 6),
            ],
          )
          ..path(style: PaintingStyle.stroke, color: const Color(0xFFFFFFFF)),
      );

      // Moving a peer cursor repaints only RemotePeerCursorLayer.
      final outside = <String>[];
      var inside = 0;
      debugOnProfilePaint = (o) {
        final element = (o.debugCreator! as DebugCreator).element;
        var owned = element.widget is RemotePeerCursorLayer;
        element.visitAncestorElements((e) {
          owned |= e.widget is RemotePeerCursorLayer;
          return !owned;
        });
        if (owned) {
          inside++;
        } else {
          outside.add('${element.widget.runtimeType}');
        }
      };
      try {
        h.api.viewValue = viewWithPeers(
          seq: 2,
          peers: const [
            DesktopPeerCursor(color: 0, x: 960, y: 540),
            DesktopPeerCursor(color: 2, x: 1440, y: 270),
          ],
        );
        await tester.pump(const Duration(milliseconds: 16));
      } finally {
        debugOnProfilePaint = null;
      }
      expect(inside, isPositive);
      expect(outside, isEmpty);

      // Peers leave: the layer tears down back to SizedBox.shrink.
      h.api.viewValue = viewWithPeers(seq: 3, peers: const []);
      await tester.pump(const Duration(milliseconds: 16));
      await tester.pump();
      expect(
        find.descendant(of: peerLayer, matching: find.byType(CustomPaint)),
        findsNothing,
      );

      await tester.pumpWidget(const SizedBox());
      prefs.dispose();
      h.dispose();
    },
  );
}
