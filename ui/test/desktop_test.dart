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
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'fake_desktop_api.dart';
import 'fake_pb_mapper_api.dart';
import 'support.dart';

void main() {
  testWidgets(
    'background sharing survives closing the app and remains explicitly stoppable',
    (tester) async {
      final api = FakePbMapperApi();
      api.desktop.available = const DesktopCapabilities(
        host: true,
        displays: [DesktopDisplay(1, 'Display 1', 1920, 1080)],
        helper: DesktopHelper(
          installed: true,
          running: true,
          authorized: true,
          identityVerified: true,
        ),
      );
      api.desktop.trust = const DesktopTrust(supported: true);
      await pumpApp(tester, api, page: AppPage.tunnels);
      await tester.tap(find.byKey(const Key('nav-remote')));
      await settle(tester);
      await tester.scrollUntilVisible(
        find.byKey(const Key('desktop-share')),
        250,
        scrollable: find.byType(Scrollable).last,
      );
      await Scrollable.ensureVisible(
        tester.element(find.byKey(const Key('desktop-share'))),
        alignment: 0.5,
      );
      await settle(tester);
      await tester.tap(find.byKey(const Key('desktop-share')));
      await settle(tester);
      var background = api.desktop.hostSnapshot.background;
      expect(background?.enabled, isTrue);
      expect(background?.autoStart, isTrue);
      expect(background?.host?.controlAllowed, isTrue);
      await finish(tester);
      expect(api.desktop.hostSnapshot.background?.enabled, isTrue);

      // An enabled grant may remain cached when management IPC is lost. SCM
      // disable/uninstall must still be reachable without first stopping it.
      api.desktop.hostSnapshot = const DesktopHostSnapshot(
        background: DesktopBackground(
          configured: true,
          enabled: true,
          autoStart: true,
          invitation: FakeDesktopApi.backgroundInvitation,
          message: 'Management connection lost',
        ),
      );
      await pumpApp(tester, api, page: AppPage.tunnels);
      await tester.tap(find.byKey(const Key('nav-remote')));
      await settle(tester);
      final maintenance = find.byKey(const Key('desktop-helper-maintenance'));
      await Scrollable.ensureVisible(tester.element(maintenance));
      await tester.tap(maintenance);
      await settle(tester);
      final disable = find.byKey(const Key('desktop-helper-disable'));
      expect(tester.widget<TextButton>(disable).onPressed, isNotNull);
      expect(
        tester
            .widget<TextButton>(
              find.byKey(const Key('desktop-helper-uninstall')),
            )
            .onPressed,
        isNotNull,
      );
      await Scrollable.ensureVisible(tester.element(disable), alignment: 0.5);
      await settle(tester);
      await tester.tap(disable);
      await settle(tester);
      expect(api.desktop.argsOf('helper_action'), [
        {'action': 'disable'},
      ]);
      final stop = find.byKey(const Key('desktop-stop'));
      await Scrollable.ensureVisible(tester.element(stop), alignment: 0.5);
      await settle(tester);
      await tester.tap(stop);
      await settle(tester);
      background = api.desktop.hostSnapshot.background;
      expect(api.desktop.hostSnapshot.host, isNull);
      expect(background?.configured, isTrue);
      expect(background?.enabled, isFalse);
      expect(background?.autoStart, isFalse);
      await finish(tester);
    },
  );

  testWidgets(
    'viewer maps touch and mouse hover and blocks ungranted or stale input',
    (tester) async {
      if (const bool.fromEnvironment('DESKTOP_QA')) await loadRealFonts();
      tester.view.physicalSize = const Size(390, 844);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
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
      await store.connect(
        const DesktopTarget(invitation: 'pbd1.test', name: 'Test'),
      );
      // The host lets this viewer watch only.
      api.viewerStatusValue = liveStatus(generation: 3, controlAllowed: false);
      api.emitJpeg(png!, generation: 3, frameId: 1);
      for (var i = 0; i < 20 && find.byType(RawImage).evaluate().isEmpty; i++) {
        await tester.runAsync(
          () => Future<void>.delayed(const Duration(milliseconds: 10)),
        );
        await tester.pump();
      }
      await tester.pump(const Duration(milliseconds: 500));
      expect(find.byType(RawImage), findsOneWidget);
      expect(api.argsOf('presented'), [
        {'consumer': 1, 'generation': 3, 'frame_id': 1},
      ]);
      final mouse = await tester.createGesture(
        kind: ui.PointerDeviceKind.mouse,
      );
      await mouse.addPointer(location: Offset.zero);
      await mouse.moveTo(tester.getCenter(find.byType(RawImage)));
      await tester.tap(find.byType(RawImage));
      await tester.pump();
      expect(api.inputs, isEmpty);

      // Allowed: control is the viewer's to ask for.
      api.viewerStatusValue = liveStatus(generation: 3);
      await tester.pump(const Duration(milliseconds: 500));
      await tester.tap(find.text('Request control'));
      await tester.pump();
      await tester.tap(find.byType(RawImage));
      await tester.pump();
      expect(api.inputs.map((e) => e['kind']), ['move', 'button', 'button']);
      expect(api.inputs.first['x'], 960);
      expect(api.inputs.first['y'], 540);
      expect(api.inputs.last['down'], false);
      final viewer = tester.getRect(find.byType(RawImage));
      await mouse.moveTo(viewer.topLeft + viewer.size.center(Offset.zero) / 2);
      await tester.pump(const Duration(milliseconds: 5));
      expect(api.inputs.length, 4);
      expect(api.inputs.last, containsPair('kind', 'move'));
      expect(api.inputs.last['x'], 480);
      expect(api.inputs.last['y'], 270);
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

      // A stale picture takes no input.
      api.viewerStatusValue = liveStatus(
        generation: 3,
        controlling: true,
        fresh: false,
      );
      await tester.pump(const Duration(milliseconds: 500));
      await mouse.moveTo(viewer.center);
      await tester.tap(find.byType(RawImage), warnIfMissed: false);
      await tester.pump(const Duration(milliseconds: 5));
      expect(api.inputs.length, 4);
      await mouse.removePointer();
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox.shrink());
      await tester.pump();
      store.dispose();
    },
  );

  testWidgets(
    'compact host flow enables control, shows sharing on the bar and stops',
    (tester) async {
      final api = FakePbMapperApi();
      await pumpApp(
        tester,
        api,
        page: AppPage.tunnels,
        size: const Size(390, 844),
      );
      final sharingDot = find.descendant(
        of: find.byKey(const Key('nav-remote')),
        matching: find.byType(StatusDot),
      );
      expect(sharingDot, findsNothing);
      await tester.tap(find.byKey(const Key('nav-remote')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('desktop-share')));
      await settle(tester);
      expect(api.desktop.hostSnapshot.background?.host?.controlAllowed, isTrue);
      expect(find.byKey(const Key('desktop-stop')), findsOneWidget);
      expect(tester.takeException(), isNull);
      // Sharing shows on Remote desktop's entry from every page.
      await tester.tap(find.byKey(const Key('nav-tunnels')));
      await settle(tester);
      expect(sharingDot, findsOneWidget);
      await tester.tap(find.byKey(const Key('nav-remote')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('desktop-stop')));
      await settle(tester);
      expect(api.desktop.hostSnapshot.background?.enabled, isFalse);
      expect(sharingDot, findsNothing);
      await finish(tester);
    },
  );
}
