import 'dart:typed_data';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_cursor_layer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_menus.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_stage.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';

import 'remote_viewer_harness.dart';

DesktopCursorShape _shape(DesktopCursorKind kind) => DesktopCursorShape(
  kind: kind,
  width: 1,
  height: 1,
  hotX: 0,
  hotY: 0,
  rgba: Uint8List(4),
);

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

  testWidgets('a desktop viewer in control shows one pointer in the host '
      'shape, and paints the host pointer only where it is elsewhere', (
    tester,
  ) async {
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

    final pointer = RemotePointer(
      mouse: true,
      cursor: h.store.cursor,
      controlling: h.input.ready,
      geometry: h.store.geometry,
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
    MouseCursor? local() =>
        RendererBinding.instance.mouseTracker.debugDeviceActiveCursor(1);
    final mouse = await tester.createGesture(
      kind: PointerDeviceKind.mouse,
      pointer: 1,
    );
    const at = Offset(400, 225);
    await mouse.addPointer(location: at);
    addTearDown(mouse.removePointer);

    // A host that sends no shape: the arrow, painted, as before.
    await report(shape: 0);
    expect(layer, arrowAt(const Offset(0.25, 0.5)));
    expect(local(), SystemMouseCursors.basic);

    // The local pointer moves the host's, which turns a text beam: the
    // local pointer shows it, and nothing is painted under it.
    h.input.hover(h.viewport.toNormalized(at)!);
    await report(shape: 1);
    expect(local(), SystemMouseCursors.text);
    expect(layer, isNot(paints..path()));

    // Someone at the host moves it away: painted there, the local pointer
    // keeps the shape.
    await report(shape: 1, x: 1440, seq: 2);
    expect(layer, arrowAt(const Offset(0.75, 0.5)));
    expect(local(), SystemMouseCursors.text);

    // Back under the local pointer, a shape the viewer's OS has not: the
    // local pointer hides, and the shape is painted in its place.
    h.input.hover(h.viewport.toNormalized(at)!);
    await report(shape: 2, x: 1440, seq: 2);
    expect(local(), SystemMouseCursors.none);
    expect(layer, arrowAt(const Offset(0.5, 0.5)));

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

    // Viewing only: the local pointer is its own, the host's is painted.
    h.store.control(false);
    await tester.pump(const Duration(milliseconds: 500));
    expect(local(), SystemMouseCursors.basic);
    expect(layer, paints..path());

    pointer.dispose();
    h.dispose();
  });

  testWidgets('the local pointer takes the host shape only over the picture '
      'itself, never over the bars beside it', (tester) async {
    // A 16:9 picture in a wider stage: 800 wide, between bars 200 wide.
    tester.view
      ..physicalSize = const Size(1200, 450)
      ..devicePixelRatio = 1;
    addTearDown(tester.view.reset);
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
    expect(local(), SystemMouseCursors.none);

    // Over the bar beside the picture: the local pointer is its own again.
    await mouse.moveTo(const Offset(100, 225));
    await tester.pump(const Duration(milliseconds: 16));
    expect(local(), SystemMouseCursors.basic);

    await mouse.moveTo(const Offset(610, 225));
    await tester.pump(const Duration(milliseconds: 16));
    expect(local(), SystemMouseCursors.none);

    await tester.pumpWidget(const SizedBox());
    prefs.dispose();
    h.dispose();
  });
}
