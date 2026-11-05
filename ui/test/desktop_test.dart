import 'dart:async';
import 'dart:typed_data';
import 'dart:ui' as ui;
import 'dart:io';
import 'package:flutter/rendering.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/pages/desktop_viewer.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'fake_desktop_api.dart';
import 'fake_pb_mapper_api.dart';
import 'support.dart';

void main() {
  testWidgets(
    'phone viewer maps a tap to display pixels and blocks stale input',
    (tester) async {
      if (const bool.fromEnvironment('DESKTOP_QA')) await loadRealFonts();
      tester.view.physicalSize = const Size(390, 844);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      const screen = DesktopStatus(
        state: 'streaming',
        controlAllowed: true,
        generation: 3,
        frameId: 4,
        display: DesktopDisplay(1, 'Test display', 1920, 1080),
        path: 'direct',
      );
      final png = await tester.runAsync(() async {
        final recorder = ui.PictureRecorder();
        final canvas = Canvas(recorder);
        canvas.drawRect(
          const Rect.fromLTWH(0, 0, 320, 180),
          Paint()..color = const Color(0xff16293d),
        );
        canvas.drawRect(
          const Rect.fromLTWH(18, 18, 284, 144),
          Paint()..color = const Color(0xff26445e),
        );
        final paragraph =
            (ui.ParagraphBuilder(
                    ui.ParagraphStyle(
                      fontSize: 18,
                      fontFamily: 'GoogleSansFlex',
                    ),
                  )
                  ..pushStyle(ui.TextStyle(color: const Color(0xffffffff)))
                  ..addText('Remote desktop\nViewer acceptance test'))
                .build()
              ..layout(const ui.ParagraphConstraints(width: 260));
        canvas.drawParagraph(paragraph, const Offset(30, 55));
        final picture = recorder.endRecording();
        final image = await picture.toImage(320, 180);
        final bytes = (await image.toByteData(
          format: ui.ImageByteFormat.png,
        ))!.buffer.asUint8List();
        image.dispose();
        picture.dispose();
        paragraph.dispose();
        return bytes;
      });
      api.viewer = screen;
      api.image = DesktopFrame(screen, png!);
      await store.poll();
      const root = Key('desktop-qa');
      await tester.pumpWidget(
        MaterialApp(
          theme: buildAppTheme(Brightness.light),
          localizationsDelegates: AppLocalizations.localizationsDelegates,
          supportedLocales: AppLocalizations.supportedLocales,
          home: RepaintBoundary(
            key: root,
            child: DesktopViewer(store: store),
          ),
        ),
      );
      for (var i = 0; i < 20 && find.byType(RawImage).evaluate().isEmpty; i++) {
        await tester.runAsync(
          () => Future<void>.delayed(const Duration(milliseconds: 10)),
        );
        await tester.pump();
      }
      await tester.pump();
      expect(find.byType(RawImage), findsOneWidget);
      await tester.tap(find.byType(RawImage));
      await tester.pump();
      expect(api.inputs, isEmpty);
      await tester.tap(find.text('Request control'));
      await store.poll();
      await tester.pump();
      await tester.tap(find.byType(RawImage));
      await tester.pump();
      expect(api.inputs.map((e) => e['kind']), ['move', 'button', 'button']);
      expect(api.inputs.first['x'], 960);
      expect(api.inputs.first['y'], 540);
      expect(api.inputs.last['down'], false);
      if (const bool.fromEnvironment('DESKTOP_QA')) {
        await tester.runAsync(() async {
          final image =
              await (tester.renderObject(find.byKey(root))
                      as RenderRepaintBoundary)
                  .toImage();
          final data = await image.toByteData(format: ui.ImageByteFormat.png);
          final file = File('../target/desktop-phone-viewer.png');
          await file.writeAsBytes(data!.buffer.asUint8List());
          image.dispose();
        });
      }
      api.viewer = const DesktopStatus(
        state: 'stale',
        controlling: true,
        generation: 3,
      );
      await store.poll();
      await tester.pump();
      await tester.tap(find.byType(RawImage), warnIfMissed: false);
      await tester.pump();
      expect(api.inputs.length, 3);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox.shrink());
      await tester.pump();
      store.dispose();
    },
  );
  test(
    'input is ordered, view-only is read-only, and release cancels queued actions',
    () async {
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      const visible = DesktopStatus(
        state: 'streaming',
        controlAllowed: true,
        generation: 3,
        frameId: 4,
      );
      api.viewer = visible;
      api.image = DesktopFrame(visible, Uint8List.fromList([1]));
      await store.poll();
      await store.send(visible, {'kind': 'text', 'text': 'denied'});
      expect(api.inputs, isEmpty);
      await api.control(true);
      await store.poll();
      final press = store.send(visible, {
        'kind': 'button',
        'button': 'left',
        'down': true,
      });
      final lift = store.send(visible, {
        'kind': 'button',
        'button': 'left',
        'down': false,
      });
      await Future.wait([press, lift]);
      expect(api.inputs.map((e) => e['down']), [true, false]);
      expect(api.sequences, [1, 2]);
      api.inputGate = Completer<void>();
      final held = store.send(visible, {
        'kind': 'key',
        'key': 'shift',
        'down': true,
      });
      await Future<void>.delayed(Duration.zero);
      final queued = store.send(visible, {'kind': 'text', 'text': 'cancelled'});
      final release = store.release();
      api.inputGate!.complete();
      await Future.wait([held, queued, release]);
      expect(api.inputs.any((e) => e['text'] == 'cancelled'), isFalse);
      expect(api.releases, 1);
      api.viewer = const DesktopStatus(
        state: 'stale',
        controlling: true,
        generation: 3,
      );
      await store.poll();
      expect(store.controlling, isFalse);
      store.dispose();
    },
  );

  testWidgets(
    'phone host flow exposes a persistent stop and defaults to view-only',
    (tester) async {
      final api = FakePbMapperApi();
      await pumpApp(
        tester,
        api,
        page: AppPage.tunnels,
        size: const Size(390, 844),
      );
      await tester.tap(find.byKey(const Key('open-desktop')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('desktop-share')));
      await settle(tester);
      expect(api.desktop.host?.controlAllowed, isFalse);
      expect(find.byKey(const Key('desktop-stop')), findsOneWidget);
      expect(tester.takeException(), isNull);
      await tester.pageBack();
      await settle(tester);
      expect(find.text('This desktop is being shared'), findsOneWidget);
      // An outgoing operation must not disable the persistent local stop.
      AppScope.of(
        tester.element(find.byKey(const Key('open-desktop'))),
      ).store.desktop.busy = true;
      await tester.tap(find.text('Stop sharing'));
      await settle(tester);
      expect(api.desktop.host, isNull);
      await finish(tester);
    },
  );
}
