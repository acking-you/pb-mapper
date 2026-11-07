import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_title_bar.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

const _window = MethodChannel('window_manager');
const _close = Key('caption-close');

/// Answer window_manager as a window that is, or is not, maximised.
void _maximized(WidgetTester tester, bool maximized) {
  final messenger = tester.binding.defaultBinaryMessenger;
  messenger.setMockMethodCallHandler(
    _window,
    (call) async => call.method == 'isMaximized' ? maximized : null,
  );
  addTearDown(() => messenger.setMockMethodCallHandler(_window, null));
}

/// Pump the app at [size] logical pixels and [dpr], and rest the mouse on
/// the close button, whose red is what must keep clear of the edge.
Future<Rect> _hoveredClose(
  WidgetTester tester, {
  required Size size,
  required double dpr,
  bool needsSetup = false,
}) async {
  await pumpApp(
    tester,
    FakePbMapperApi(),
    page: AppPage.tunnels,
    needsSetup: needsSetup,
    size: size,
  );
  tester.view
    ..devicePixelRatio = dpr
    ..physicalSize = size * dpr;
  await settle(tester);
  final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
  await mouse.addPointer(location: tester.getCenter(find.byKey(_close)));
  await tester.pump();
  final rect = tester.getRect(find.byKey(_close));
  await mouse.removePointer();
  return rect;
}

void main() {
  testWidgets(
    "caption buttons keep 16 px from the sheet edge and the setup screen's edge",
    (tester) async {
      debugDefaultTargetPlatformOverride = TargetPlatform.windows;
      try {
        for (final dpr in const [1.0, 1.25, 1.5]) {
          for (final maximized in const [false, true]) {
            _maximized(tester, maximized);
            // Maximised fills a 1080p screen above its taskbar; restored is
            // the window's usual size.
            final size = maximized
                ? Size(1920 / dpr, 1032 / dpr)
                : const Size(1180, 780);
            final close = await _hoveredClose(tester, size: size, dpr: dpr);
            final sheet = tester.getRect(find.byType(ContentSheet));
            final at = 'dpr $dpr, ${maximized ? 'maximised' : 'restored'}';
            expect(close.size, kCaptionButtonSize, reason: at);
            expect(
              close.right,
              lessThanOrEqualTo(sheet.right - 16),
              reason: at,
            );
            // Centred in the 58 px title bar at the top of the sheet.
            expect(close.center.dy, closeTo(sheet.top + 29, 0.01), reason: at);
            await finish(tester);
          }
        }

        _maximized(tester, false);
        const window = Size(1180, 780);
        final close = await _hoveredClose(
          tester,
          size: window,
          dpr: 1.25,
          needsSetup: true,
        );
        expect(close.size, kCaptionButtonSize);
        expect(close.right, lessThanOrEqualTo(window.width - 16));
        // Centred in the setup screen's 44 px drag strip.
        expect(close.center.dy, closeTo(22, 0.01));
        await finish(tester);
      } finally {
        debugDefaultTargetPlatformOverride = null;
      }
    },
  );

  testWidgets("the viewer's caption buttons keep the window's right edge", (
    tester,
  ) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.windows;
    try {
      _maximized(tester, false);
      tester.view
        ..physicalSize = const Size(1440, 900)
        ..devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      await tester.pumpWidget(
        const MaterialApp(
          localizationsDelegates: AppLocalizations.localizationsDelegates,
          supportedLocales: AppLocalizations.supportedLocales,
          home: Material(
            child: Align(
              alignment: Alignment.topCenter,
              child: ViewerTitleBar(title: 'Office PC'),
            ),
          ),
        ),
      );
      await tester.pump();
      // The bar's end inset is 4 px; the title must not hold the free space.
      expect(tester.getRect(find.byKey(_close)).right, 1440 - 4);
    } finally {
      debugDefaultTargetPlatformOverride = null;
    }
  });
}
