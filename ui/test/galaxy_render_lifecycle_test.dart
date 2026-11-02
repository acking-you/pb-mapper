import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_canvas.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_frame.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_model.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_painter.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_view.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_atlas.dart';
import 'package:pb_mapper_ui/src/state/remote_service.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';

GalaxyScene scene({int count = 8}) => buildScene(
  shares: [
    for (var i = 0; i < count; i++)
      Tunnel(
        kind: TunnelKind.share,
        key: 'share-$i',
        localAddress: '127.0.0.1:1234',
        protocol: 'TCP',
        encrypted: true,
        keepAlive: true,
        health: TunnelHealth.running,
      ),
  ],
  connections: const [],
  relayAddress: 'relay.example:7666',
  relayOnline: true,
  remote: [for (var i = 0; i < count; i++) RemoteService(name: 'remote-$i')],
  homeLabel: 'This computer',
);

Widget app(
  GalaxyView view,
  GalaxyScene scene, {
  bool enabled = true,
  bool visible = true,
  bool still = false,
  ValueChanged<String?>? onSelect,
}) => MaterialApp(
  home: TickerMode(
    enabled: enabled,
    child: Scaffold(
      body: MediaQuery(
        data: MediaQueryData(disableAnimations: still),
        child: visible
            ? GalaxyCanvas(
                view: view,
                scene: scene,
                semanticLabel: 'Galaxy',
                onSelect: onSelect,
              )
            : const SizedBox(),
      ),
    ),
  ),
);

Future<void> frames(WidgetTester tester, int count) async {
  for (var i = 0; i < count; i++) {
    await tester.pump(const Duration(microseconds: 16667));
  }
}

const platforms = TargetPlatformVariant({
  TargetPlatform.windows,
  TargetPlatform.macOS,
  TargetPlatform.linux,
  TargetPlatform.android,
  TargetPlatform.iOS,
});

void main() {
  setUp(
    () => TestWidgetsFlutterBinding.instance.handleAppLifecycleStateChanged(
      AppLifecycleState.resumed,
    ),
  );
  tearDown(
    () => TestWidgetsFlutterBinding.instance.handleAppLifecycleStateChanged(
      AppLifecycleState.resumed,
    ),
  );

  testWidgets(
    'galaxy rests, wakes for interaction and scene changes, then rests again',
    (tester) async {
      final view = GalaxyView();
      addTearDown(view.dispose);
      String? selected;
      final data = scene();
      await tester.pumpWidget(app(view, data, onSelect: (id) => selected = id));
      await frames(tester, 600);
      final time = view.time;
      var repaints = 0;
      view.addListener(() => repaints++);
      await frames(tester, 90);
      expect(view.time, time);
      expect(repaints, 0);
      expect(tester.binding.transientCallbackCount, 0);
      expect(tester.binding.hasScheduledFrame, isFalse);

      await tester.drag(
        find.byKey(const Key('galaxy-canvas')),
        const Offset(90, 35),
      );
      await frames(tester, 10);
      expect(view.time, greaterThan(time));
      expect(repaints, greaterThan(0));
      // Once drag momentum is gone, the decorative interaction interval must
      // not introduce a new camera rotation of its own.
      view.camera.spin = 0;
      final yaw = view.camera.yaw;
      await frames(tester, 120);
      expect(view.camera.yaw, closeTo(yaw, 1e-12));
      await frames(tester, 480);
      final restingTime = view.time;
      await frames(tester, 90);
      expect(view.time, restingTime);
      expect(tester.binding.transientCallbackCount, 0);

      await tester.pumpWidget(
        app(view, scene(count: 9), onSelect: (id) => selected = id),
      );
      expect(view.busy, isTrue);
      await frames(tester, 120);
      expect(view.busy, isFalse);
      final canvas = tester.state<GalaxyCanvasState>(find.byType(GalaxyCanvas));
      canvas.select('share:share-0');
      await tester.pump();
      expect(selected, 'share:share-0');
      expect(view.selected, 'share:share-0');
      await tester.pumpWidget(const SizedBox());
    },
  );

  testWidgets(
    'a visible inactive window keeps its backdrop and renders updates',
    (tester) async {
      final view = GalaxyView();
      addTearDown(view.dispose);
      final data = scene();
      await tester.pumpWidget(app(view, data));
      await frames(tester, 20);
      final finder = find.byKey(const Key('galaxy-canvas'));
      final painter =
          tester.widget<CustomPaint>(finder).painter! as GalaxyPainter;
      final size = tester.getSize(finder);
      final backdrop = painter.backdrop.of(
        size,
        painter.palette.dark,
        painter.palette.ratio,
        painter.onBackdrop,
      );
      final atlas = painter.planets.debugImage;
      expect(backdrop, isNotNull);
      expect(atlas, isNotNull);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      await tester.pump();
      expect(painter.planets.debugImage, same(atlas));
      expect(
        painter.backdrop.of(
          size,
          painter.palette.dark,
          painter.palette.ratio,
          painter.onBackdrop,
        ),
        same(backdrop),
      );
      final time = view.time;
      await tester.pumpWidget(app(view, scene(count: 9)));
      await frames(tester, 10);
      expect(view.time, greaterThan(time));
      expect(view.scene.planetOf('share:share-8'), isNotNull);
      expect(PlanetAtlas.liveBytes, greaterThan(0));
      await tester.pumpWidget(const SizedBox());
    },
    variant: platforms,
  );

  testWidgets(
    'background and TickerMode release images and resume without a time jump',
    (tester) async {
      final view = GalaxyView();
      addTearDown(view.dispose);
      final data = scene();
      await tester.pumpWidget(app(view, data));
      await frames(tester, 10);
      expect(PlanetAtlas.liveBytes, greaterThan(0));
      for (final state in [
        AppLifecycleState.hidden,
        AppLifecycleState.paused,
        AppLifecycleState.detached,
      ]) {
        tester.binding.handleAppLifecycleStateChanged(state);
        await tester.pump();
        final time = view.time;
        await frames(tester, 30);
        expect(view.time, time, reason: '$state pauses the galaxy');
        expect(tester.binding.transientCallbackCount, 0);
        expect(PlanetAtlas.liveBytes, 0);
      }
      final time = view.time;
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      await tester.pump();
      await frames(tester, 2);
      expect(view.time - time, lessThan(0.06));
      expect(PlanetAtlas.liveBytes, greaterThan(0));
      await tester.pumpWidget(app(view, data, enabled: false));
      final paused = view.time;
      await frames(tester, 30);
      expect(view.time, paused);
      expect(PlanetAtlas.liveBytes, 0);
      await tester.pumpWidget(app(view, data, enabled: true));
      await frames(tester, 3);
      expect(PlanetAtlas.liveBytes, greaterThan(0));
      await tester.pumpWidget(app(view, data, visible: false));
      expect(PlanetAtlas.liveBytes, 0);
      await tester.pumpWidget(const SizedBox());
    },
    variant: platforms,
  );

  testWidgets('close-up rotations and viewport changes keep visible bodies', (
    tester,
  ) async {
    final view = GalaxyView();
    addTearDown(view.dispose);
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    tester.view.devicePixelRatio = 1;
    final data = scene(count: 40);
    await tester.pumpWidget(app(view, data, still: true));
    for (final size in [const Size(320, 720), const Size(1920, 1080)]) {
      tester.view.physicalSize = size;
      await tester.pump();
      final camera = view.camera;
      camera
        ..target = data.systems.first.position
        ..goalTarget = data.systems.first.position
        ..distance = 2.2
        ..goalDistance = 2.2
        ..tilt = 0.16
        ..goalTilt = 0.16;
      for (var i = 0; i < 24; i++) {
        camera.yaw = i * 6.283185307179586 / 24;
        view.tick();
        await tester.pump();
        expect(tester.takeException(), isNull, reason: '$size, yaw $i');
        expect(
          view.hits.any((hit) => hit.id == StarSystem.homeId),
          isTrue,
          reason: 'the focused star should remain visible at $size, yaw $i',
        );
      }
    }
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('the Dart fallback renders close-up without a native library', (
    tester,
  ) async {
    GalaxyNative.useDartForTesting();
    addTearDown(GalaxyNative.reset);
    final view = GalaxyView();
    addTearDown(view.dispose);
    final data = scene(count: 25);
    await tester.pumpWidget(app(view, data, still: true));
    final camera = view.camera;
    camera
      ..target = data.systems.first.position
      ..goalTarget = data.systems.first.position
      ..distance = 2.2
      ..goalDistance = 2.2
      ..tilt = 0.16
      ..goalTilt = 0.16;
    for (var i = 0; i < 12; i++) {
      camera.yaw = i * 6.283185307179586 / 12;
      view.tick();
      await tester.pump();
      expect(tester.takeException(), isNull, reason: 'fallback yaw $i');
      expect(view.hits.any((hit) => hit.id == StarSystem.homeId), isTrue);
    }
    expect(GalaxyNative.available, isFalse);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('reduced motion still restores the overview after interaction', (
    tester,
  ) async {
    final view = GalaxyView();
    addTearDown(view.dispose);
    await tester.pumpWidget(app(view, scene(), still: true));
    await frames(tester, 300);
    expect(view.still, isTrue);
    await tester.drag(
      find.byKey(const Key('galaxy-canvas')),
      const Offset(20, 90),
    );
    await frames(tester, 120);
    expect(view.camera.goalTilt, isNot(view.camera.restTilt));
    await tester.pump(const Duration(seconds: 7));
    await frames(tester, 300);
    expect(view.camera.goalTilt, view.camera.restTilt);
    expect(tester.binding.transientCallbackCount, 0);
    await tester.pumpWidget(const SizedBox());
  });

  test('unpainted transitions expire', () {
    final view = GalaxyView();
    addTearDown(view.dispose);
    view.update(scene(count: 1), first: true);
    view.update(scene(count: 2), first: false);
    expect(view.busy, isTrue);
    view.update(GalaxyScene.empty, first: false);
    view.time = 3;
    view.expireTransitions();
    expect(view.busy, isFalse);
  });
}
