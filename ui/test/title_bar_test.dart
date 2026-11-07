import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_title_bar.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';

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

/// Pump the app at [size] logical pixels and [dpr].
Future<void> _pump(
  WidgetTester tester, {
  required Size size,
  double dpr = 1,
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
}

/// Where Windows puts its own close button: the window's top-right corner,
/// flush with both edges.
Rect _cornerOf(Size window, {double height = kCaptionButtonHeight}) =>
    Rect.fromLTWH(
      window.width - kCaptionButtonWidth,
      0,
      kCaptionButtonWidth,
      height,
    );

void main() {
  testWidgets(
    "caption buttons sit in the window's corner, on the page's own surface",
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
            await _pump(tester, size: size, dpr: dpr);
            final at = 'dpr $dpr, ${maximized ? 'maximised' : 'restored'}';
            expect(
              tester.getRect(find.byKey(_close)),
              _cornerOf(size),
              reason: at,
            );
            // One layer: the sheet runs to the window's top and right edges,
            // so no backdrop shows around the buttons.
            final sheet = tester.getRect(find.byType(ContentSheet));
            expect(sheet.topRight, Offset(size.width, 0), reason: at);
            await finish(tester);
          }
        }

        // A compact window's page fills it, bottom bar and all.
        _maximized(tester, false);
        const compact = Size(640, 760);
        await _pump(tester, size: compact);
        expect(find.byType(ContentSheet), findsNothing);
        expect(tester.getRect(find.byKey(_close)), _cornerOf(compact));
        await finish(tester);

        const window = Size(1180, 780);
        await _pump(tester, size: window, dpr: 1.25, needsSetup: true);
        expect(tester.getRect(find.byKey(_close)), _cornerOf(window));
        await finish(tester);
      } finally {
        debugDefaultTargetPlatformOverride = null;
      }
    },
  );

  testWidgets('on macOS the sheet still floats inset on the backdrop', (
    tester,
  ) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
    try {
      _maximized(tester, false);
      const window = Size(1180, 780);
      await _pump(tester, size: window);
      expect(find.byKey(_close), findsNothing);
      final sheet = tester.getRect(find.byType(ContentSheet));
      expect(sheet.topRight, const Offset(1180 - kSheetInset, kSheetInset));
      await finish(tester);
    } finally {
      debugDefaultTargetPlatformOverride = null;
    }
  });

  testWidgets("the viewer's caption buttons fill its bar in the corner", (
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
      // The title must not hold the free space and push them off the edge.
      expect(
        tester.getRect(find.byKey(_close)),
        _cornerOf(const Size(1440, 900), height: 36),
      );
    } finally {
      debugDefaultTargetPlatformOverride = null;
    }
  });
}
