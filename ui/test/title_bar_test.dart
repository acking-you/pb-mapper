import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_title_bar.dart';
import 'package:pb_mapper_ui/src/pages/settings_page.dart';
import 'package:pb_mapper_ui/src/pages/tunnels_page.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

const _window = MethodChannel('window_manager');
final _close = find.byKey(const Key('caption-close'));

/// Answer window_manager as a window that is, or is not, maximised.
void _maximized(WidgetTester tester, bool maximized) {
  final messenger = tester.binding.defaultBinaryMessenger;
  messenger.setMockMethodCallHandler(
    _window,
    (call) async => call.method == 'isMaximized' ? maximized : null,
  );
  addTearDown(() => messenger.setMockMethodCallHandler(_window, null));
}

/// Pump the app on Windows at [size] logical pixels and [dpr].
Future<void> _pump(
  WidgetTester tester, {
  required Size size,
  double dpr = 1,
  bool needsSetup = false,
  bool reducedMotion = true,
  bool viewed = false,
}) async {
  final api = FakePbMapperApi();
  if (viewed) api.desktop.hostSnapshot = viewedHost(controlling: true);
  await pumpApp(
    tester,
    api,
    page: AppPage.tunnels,
    needsSetup: needsSetup,
    size: size,
    reducedMotion: reducedMotion,
  );
  tester.view
    ..devicePixelRatio = dpr
    ..physicalSize = size * dpr;
  await settle(tester);
}

/// Run [body] as Windows, which draws its own caption buttons.
Future<void> _onWindows(Future<void> Function() body) async {
  debugDefaultTargetPlatformOverride = TargetPlatform.windows;
  try {
    await body();
  } finally {
    debugDefaultTargetPlatformOverride = null;
  }
}

/// Where Windows puts its own close button: flush in the window's top-right
/// corner, as tall as the title bar, to within one device pixel.
void _expectInCorner(
  WidgetTester tester,
  Size window,
  double height, {
  String? reason,
}) {
  final close = tester.getRect(_close);
  final pixel = 1 / tester.view.devicePixelRatio;
  expect(close.right, closeTo(window.width, 1e-6), reason: reason);
  expect(close.top, 0, reason: reason);
  expect(close.width, closeTo(kCaptionButtonWidth, pixel), reason: reason);
  expect(close.height, closeTo(height, pixel), reason: reason);
}

/// Whether a click at [at] reaches something inside [finder] first.
bool _reaches(WidgetTester tester, Offset at, Finder finder) {
  final targets = {for (final e in finder.evaluate()) e.renderObject};
  final hit = tester.hitTestOnBinding(at).path.first.target;
  for (RenderObject? o = hit as RenderObject; o != null; o = o.parent) {
    if (targets.contains(o)) return true;
  }
  return false;
}

/// The opacity [finder]'s ancestors multiply into it.
double _opacityOver(WidgetTester tester, Finder finder) {
  var alpha = 1.0;
  for (RenderObject? o = tester.renderObject(finder); o != null; o = o.parent) {
    if (o is RenderOpacity) alpha *= o.opacity;
    if (o is RenderAnimatedOpacity) alpha *= o.opacity.value;
  }
  return alpha;
}

void main() {
  testWidgets(
    "the caption buttons sit flush in the window's corner, as tall as the "
    "page's bar, on one surface",
    (tester) async {
      await _onWindows(() async {
        for (final (dpr, maximized) in const [(1.25, false), (1.0, true)]) {
          _maximized(tester, maximized);
          final size = maximized
              ? const Size(1920, 1032)
              : const Size(1180, 780);
          await _pump(tester, size: size, dpr: dpr);
          final at = 'dpr $dpr, ${maximized ? 'maximised' : 'restored'}';
          _expectInCorner(tester, size, kPageBarHeight, reason: at);
          // No backdrop shows around them: the sheet reaches the corner.
          final sheet = tester.getRect(find.byType(ContentSheet));
          expect(sheet.topRight, Offset(size.width, 0), reason: at);

          // The window's very corner closes a maximised window. A restored
          // one resizes from its top edge there, as Windows' own does, and
          // closes just below it.
          final corner = Offset(size.width - 0.5, 0.5);
          final band = find.byKey(const Key('window-resize-top'));
          expect(_reaches(tester, corner, _close), maximized, reason: at);
          expect(_reaches(tester, corner, band), !maximized, reason: at);
          expect(
            _reaches(tester, corner.translate(0, kTopResizeBand), _close),
            isTrue,
            reason: at,
          );
          // Beside them, right below the band, the bar still drags.
          expect(
            _reaches(
              tester,
              Offset(size.width - kCaptionButtonsWidth - 8, kTopResizeBand + 1),
              find.byType(WindowDragArea),
            ),
            isTrue,
            reason: at,
          );
          await finish(tester);
        }
      });
    },
  );

  testWidgets("a compact window, the viewer and setup each have them in the "
      "corner at their own bar's height", (tester) async {
    await _onWindows(() async {
      _maximized(tester, false);
      const compact = Size(640, 760);
      await _pump(tester, size: compact);
      expect(find.byType(ContentSheet), findsNothing);
      _expectInCorner(tester, compact, kCompactPageBarHeight);
      await finish(tester);

      const window = Size(1180, 780);
      await _pump(tester, size: window);
      AppScope.of(
        tester.element(find.byType(TunnelsPage)),
      ).remote.viewer.open();
      // The viewer's spinner never settles.
      for (var i = 0; i < 4; i++) {
        await tester.pump(const Duration(milliseconds: 50));
      }
      expect(find.byType(ViewerTitleBar), findsOneWidget);
      _expectInCorner(tester, window, kViewerTitleBarHeight);
      await finish(tester);

      await _pump(tester, size: window, dpr: 1.25, needsSetup: true);
      _expectInCorner(tester, window, kCaptionButtonHeight);
      await finish(tester);
    });
  });

  testWidgets("a dialog's barrier leaves the caption buttons uncovered", (
    tester,
  ) async {
    await _onWindows(() async {
      _maximized(tester, false);
      await _pump(tester, size: const Size(1180, 780));
      unawaited(
        confirmDestructive(
          tester.element(find.byType(TunnelsPage)),
          title: 'Delete',
          body: 'It cannot be undone.',
          action: 'Delete',
        ),
      );
      await settle(tester);
      expect(find.byType(AlertDialog), findsOneWidget);
      expect(_reaches(tester, tester.getCenter(_close), _close), isTrue);
      await finish(tester);
    });
  });

  testWidgets("the title bar's menu opens over the caption buttons", (
    tester,
  ) async {
    await _onWindows(() async {
      _maximized(tester, false);
      // The narrowest window: the capsule is a dot, and its menu reaches
      // under the caption buttons.
      await _pump(tester, size: const Size(720, 560), viewed: true);
      await tester.tap(find.byKey(const Key('sharing-indicator')));
      await settle(tester);
      final item = find.byKey(const Key('sharing-indicator-disconnect'));
      final corner = tester.getRect(item).topRight.translate(-1, 1);
      expect(_reaches(tester, corner, item), isTrue);
      await finish(tester);
    });
  });

  testWidgets('switching page neither moves nor fades the caption buttons', (
    tester,
  ) async {
    await _onWindows(() async {
      _maximized(tester, false);
      const window = Size(1180, 780);
      await _pump(tester, size: window, reducedMotion: false);
      await tester.tap(find.byKey(const Key('nav-settings')));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 100));
      // The page is still arriving; the buttons are where they were.
      expect(_opacityOver(tester, find.byType(SettingsPage)), lessThan(1));
      _expectInCorner(tester, window, kPageBarHeight);
      expect(_opacityOver(tester, _close), 1);
      await finish(tester);
    });
  });

  testWidgets('on macOS the sheet still floats inset on the backdrop', (
    tester,
  ) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
    try {
      _maximized(tester, false);
      const window = Size(1180, 780);
      await _pump(tester, size: window);
      expect(_close, findsNothing);
      final sheet = tester.getRect(find.byType(ContentSheet));
      expect(sheet.topRight, const Offset(1180 - kSheetInset, kSheetInset));
      await finish(tester);
    } finally {
      debugDefaultTargetPlatformOverride = null;
    }
  });
}
