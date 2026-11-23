import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_surface.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_viewport.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_menus.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_stage.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';

import 'fake_desktop_api.dart';
import 'remote_viewer_harness.dart';

/// The picture's top left, [edge] in the view's logical pixels, falls on a
/// device pixel's.
void _expectOnDevicePixels(Offset edge, double dpr, {String? reason}) {
  final device = edge * dpr;
  expect(
    device.dx,
    moreOrLessEquals(device.dx.roundToDouble(), epsilon: 1e-6),
    reason: reason,
  );
  expect(
    device.dy,
    moreOrLessEquals(device.dy.roundToDouble(), epsilon: 1e-6),
    reason: reason,
  );
}

Matcher _sizeMoreOrLessEquals(Size expected) => isA<Size>()
    .having(
      (s) => s.width,
      'width',
      moreOrLessEquals(expected.width, epsilon: 1e-6),
    )
    .having(
      (s) => s.height,
      'height',
      moreOrLessEquals(expected.height, epsilon: 1e-6),
    );

/// Windows' common scales, and macOS' Retina.
const _dprs = [1.0, 1.25, 1.5, 2.0];

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'original size keeps its 1:1 picture on whole device pixels wherever '
    'the pointer leads it, with no ground at the edges and slow pans kept',
    () {
      const origin = Offset(10.4, 36.7);
      const remote = Size(2560, 1440);
      final random = math.Random(5);
      for (final dpr in _dprs) {
        for (final stage in const [Size(701, 419), Size(1003.3, 577.7)]) {
          final reason = 'dpr $dpr, stage $stage';
          final v = RemoteViewport()
            ..aspectRatio = remote.aspectRatio
            ..layout(stage)
            ..setPixels(display: remote, video: remote, dpr: dpr)
            ..setMode(ViewerScaleMode.original)
            ..setOrigin(origin);
          expect(
            v.scene.width * v.scale * dpr,
            moreOrLessEquals(remote.width, epsilon: 1e-6),
            reason: reason,
          );
          for (var i = 0; i < 40; i++) {
            final pointer = Offset(
              random.nextDouble() * stage.width,
              random.nextDouble() * stage.height,
            );
            v.autoScrollTo(pointer);
            _expectOnDevicePixels(origin + v.offset, dpr, reason: reason);
            final picture = v.offset & v.scene * v.scale;
            expect(picture.left, lessThanOrEqualTo(1e-9), reason: reason);
            expect(picture.top, lessThanOrEqualTo(1e-9), reason: reason);
            expect(picture.right, greaterThan(stage.width - 1e-9));
            expect(picture.bottom, greaterThan(stage.height - 1e-9));
            // What the pointer is over is what the host is told it is over.
            expect(
              v.toLocal(v.toNormalized(pointer)!),
              offsetMoreOrLessEquals(pointer),
              reason: reason,
            );
          }

          // A pan in steps smaller than a device pixel still moves.
          v.autoScrollTo(stage.center(Offset.zero));
          final start = v.offset;
          for (var i = 0; i < 10; i++) {
            v.panBy(const Offset(-0.3, 0.3));
          }
          expect(v.offset.dx, moreOrLessEquals(start.dx - 3, epsilon: 1 / dpr));
          expect(v.offset.dy, moreOrLessEquals(start.dy + 3, epsilon: 1 / dpr));
          _expectOnDevicePixels(origin + v.offset, dpr, reason: reason);
          v.dispose();
        }
      }
    },
  );

  test('a fit that would enlarge the picture by less than 4% shows it one '
      'device pixel per video pixel, centred on device pixels', () {
    const origin = Offset(3.3, 29.9);
    const video = Size(1920, 1080);
    for (final dpr in _dprs) {
      // 1950×1100 device pixels: the fit would enlarge it 1.6%.
      final room = const Size(1950, 1100) / dpr;
      final v = RemoteViewport()
        ..aspectRatio = video.aspectRatio
        ..layout(room)
        ..setPixels(display: video, video: video, dpr: dpr)
        ..setOrigin(origin);
      expect(v.scene * v.scale * dpr, _sizeMoreOrLessEquals(video));
      _expectOnDevicePixels(origin + v.offset, dpr, reason: 'dpr $dpr');
      final centre = v.toLocal(const Offset(0.5, 0.5));
      expect((centre - room.center(Offset.zero)).distance, lessThan(1 / dpr));
      final n = const Offset(0.3, 0.7);
      expect(v.toNormalized(v.toLocal(n)), offsetMoreOrLessEquals(n));

      // A 4% enlargement or more, and any reduction, still fit the room.
      for (final device in const [Size(1997, 1124), Size(1900, 1069)]) {
        v.layout(device / dpr);
        expect(
          v.scene.width * v.scale * dpr,
          moreOrLessEquals(device.width),
          reason: 'dpr $dpr, $device',
        );
      }
      v.dispose();
    }
  });

  test('a pinch moves the picture freely under the fingers, and it settles '
      'on device pixels once they lift', () {
    const origin = Offset(3.3, 29.9);
    const video = Size(1920, 1080);
    const dpr = 1.25;
    final v = RemoteViewport()
      ..aspectRatio = video.aspectRatio
      ..layout(const Size(1950, 1100) / dpr)
      ..setPixels(display: video, video: video, dpr: dpr)
      ..setOrigin(origin);
    _expectOnDevicePixels(origin + v.offset, dpr);
    final settled = v.offset;
    // Fingers pinching in hold the picture at its smallest, centred in the
    // room: a fractional device pixel from the grid here.
    final start = [const Offset(500, 300), const Offset(700, 300)];
    v.pinchPan(start, [const Offset(550, 300), const Offset(650, 300)]);
    expect(v.scale, kMinScale);
    final free = v.offset;
    expect(((origin + free) * dpr).dx % 1, isNot(moreOrLessEquals(0)));
    v.endPinch();
    expect(v.offset, isNot(free));
    expect(v.offset, offsetMoreOrLessEquals(settled));
    _expectOnDevicePixels(origin + v.offset, dpr);
    v.dispose();
  });

  testWidgets('the surface finds its place in the view for itself, and the '
      'host gets the pixel under the pointer at 125%', (tester) async {
    const dpr = 1.25;
    const at = Offset(10.4, 36.6);
    tester.view
      ..devicePixelRatio = dpr
      ..physicalSize = const Size(1000, 700);
    addTearDown(tester.view.reset);
    final h = await ViewerHarness.live(tester);
    await tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Align(
          alignment: Alignment.topLeft,
          child: Padding(
            padding: EdgeInsets.only(left: at.dx, top: at.dy),
            child: SizedBox(
              width: 701,
              height: 419.4,
              child: RemoteSurface(
                viewport: h.viewport,
                input: h.input,
                machine: h.machine,
                touchMode: h.touchMode,
                child: const Texture(textureId: 17),
              ),
            ),
          ),
        ),
      ),
    );
    h.viewport
      ..setPixels(
        display: const Size(1920, 1080),
        video: const Size(1920, 1080),
        dpr: dpr,
      )
      ..setMode(ViewerScaleMode.original);
    await tester.pump();
    h.sent();
    final origin = tester.getTopLeft(find.byType(RemoteSurface));
    expect(origin, at);
    Offset shown() {
      final transform = tester
          .widget<Transform>(find.byKey(const Key('remote-scene-transform')))
          .transform;
      final translation = transform.getTranslation();
      return Offset(translation.x, translation.y);
    }

    // Settled where the layout put it, before any pointer came.
    expect(shown(), h.viewport.offset);
    _expectOnDevicePixels(origin + shown(), dpr);

    final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
    await mouse.addPointer(location: at + const Offset(1, 1));
    addTearDown(mouse.removePointer);
    final random = math.Random(3);
    for (var i = 0; i < 12; i++) {
      // The centre of a device pixel on the stage.
      final device = Offset(
        (at.dx * dpr).ceilToDouble() + random.nextInt(870),
        (at.dy * dpr).ceilToDouble() + random.nextInt(520),
      );
      await mouse.moveTo((device + const Offset(0.5, 0.5)) / dpr);
      // Past the store's gate between moves.
      await tester.pump(const Duration(milliseconds: 16));
      _expectOnDevicePixels(origin + shown(), dpr);
      final edge = (origin + shown()) * dpr;
      final x = device.dx.toInt() - edge.dx.round();
      final y = device.dy.toInt() - edge.dy.round();
      expect(h.sent().last, ['move $x,$y']);
    }
    h.dispose();
  });

  testWidgets('a viewer whose picture nearly fits shows it one device pixel '
      'per video pixel, on device pixels', (tester) async {
    // At 125%, a stage of 1949.5×1100.25 device pixels for a 1920×1080
    // video, at a fractional device pixel in the view.
    const at = Offset(0.4, 36.6);
    tester.view
      ..devicePixelRatio = 1.25
      ..physicalSize = const Size(1950, 1146);
    addTearDown(tester.view.reset);
    final h = await ViewerHarness.live(tester);
    final prefs = ViewerPrefs(persist: false);
    await tester.pumpWidget(
      MaterialApp(
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: Builder(
          builder: (context) => Theme(
            data: viewerTheme(context),
            child: Padding(
              padding: EdgeInsets.only(left: at.dx, top: at.dy),
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
      ),
    );
    await tester.pump(const Duration(milliseconds: 16));
    final v = h.viewport;
    expect(tester.getTopLeft(find.byType(RemoteSurface)), at);
    expect(
      v.scene * v.scale * 1.25,
      _sizeMoreOrLessEquals(const Size(1920, 1080)),
    );
    _expectOnDevicePixels(at + v.offset, 1.25);

    await tester.pumpWidget(const SizedBox());
    prefs.dispose();
    h.dispose();
  });

  testWidgets(
    'zoom stays local through pinch, reconnects and the soft keyboard',
    (tester) async {
      final h = await ViewerHarness.live(tester, mode: TouchMode.direct);
      await tester.pumpWidget(
        Directionality(
          textDirection: TextDirection.ltr,
          child: Align(
            alignment: Alignment.topLeft,
            child: SizedBox(
              width: 400,
              height: 300,
              child: RemoteSurface(
                viewport: h.viewport,
                input: h.input,
                machine: h.machine,
                touchMode: h.touchMode,
                child: const Texture(textureId: 17),
              ),
            ),
          ),
        ),
      );
      await tester.pump();
      final texture = tester.element(find.byType(Texture));
      Matrix4 transform() => tester
          .widget<Transform>(find.byKey(const Key('remote-scene-transform')))
          .transform;
      // A 16:9 picture fitted into 400×300: 400×225 at y 37.5.
      expect(h.viewport.scene, const Size(400, 225));
      h.sent();

      // A pinch zooms locally: nothing reaches the host.
      final a = await tester.startGesture(const Offset(150, 150), pointer: 1);
      final b = await tester.startGesture(const Offset(250, 150), pointer: 2);
      await a.moveTo(const Offset(100, 150));
      await b.moveTo(const Offset(300, 150));
      await a.up();
      await b.up();
      await tester.pump();
      expect(h.sent(), isEmpty);
      expect(h.viewport.scale, greaterThan(1.2));
      expect(tester.element(find.byType(Texture)), same(texture));
      final zoomed = (h.viewport.scale, h.viewport.offset, transform());

      // A later direct tap lands where the zoomed picture shows it.
      await tester.tapAt(h.viewport.toLocal(const Offset(0.375, 0.625)));
      expect(h.sent(), [
        ['move 720,675', 'left↓', 'left↑'],
      ]);

      // A reconnect (new generation, new geometry epoch) keeps the view.
      final epoch = h.store.geometry.value!.epoch;
      h.api.viewerStatusValue = liveStatus(controlling: true, generation: 2);
      await tester.pump(const Duration(milliseconds: 500));
      expect(h.store.geometry.value!.epoch, isNot(epoch));
      expect((h.viewport.scale, h.viewport.offset, transform()), zoomed);
      expect(tester.element(find.byType(Texture)), same(texture));

      // The soft keyboard occludes: the scene and the scale stay, the
      // cursor is followed into view, and closing it restores the view.
      final scene = h.viewport.scene;
      h.viewport.beginKeyboard();
      h.viewport.setOcclusion(const EdgeInsets.only(bottom: 200));
      h.viewport.ensureVisible(const Offset(0.5, 0.9));
      await tester.pump();
      expect(h.viewport.scene, scene);
      expect(h.viewport.scale, zoomed.$1);
      expect(h.viewport.offset, isNot(zoomed.$2));
      expect(tester.element(find.byType(Texture)), same(texture));
      h.viewport.endKeyboard();
      await tester.pump();
      expect((h.viewport.scale, h.viewport.offset, transform()), zoomed);

      // Unless the user panned meanwhile: then the pan stays.
      h.viewport.beginKeyboard();
      h.viewport.setOcclusion(const EdgeInsets.only(bottom: 200));
      h.viewport.panBy(const Offset(0, -40));
      h.viewport.endKeyboard();
      await tester.pump();
      expect(h.viewport.scale, zoomed.$1);
      expect(h.viewport.offset, zoomed.$2 + const Offset(0, -40));
      expect(tester.element(find.byType(Texture)), same(texture));

      h.dispose();
    },
  );
}
