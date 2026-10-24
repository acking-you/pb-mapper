import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/app.dart';
import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/shell/app_prefs.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/updates/update_controller.dart';

import 'fake_pb_mapper_api.dart';

/// The root the app is pumped under, for screenshots.
const Key appRootKey = Key('app-root');

/// Change events a test can send, standing in for the Rust callback.
final changes = StreamController<StateChange>.broadcast();

/// Pump the whole app over [api] at [size], with motion reduced so the
/// galaxy settles and pumpAndSettle can finish.
Future<void> pumpApp(
  WidgetTester tester,
  FakePbMapperApi api, {
  bool needsSetup = false,
  AppPage page = AppPage.overview,
  Size size = const Size(1280, 820),
  ThemeMode themeMode = ThemeMode.light,
  Locale? locale = const Locale('en'),
  bool reducedMotion = true,
  UpdateController? updates,
}) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  if (reducedMotion) {
    tester.platformDispatcher.accessibilityFeaturesTestValue =
        const FakeAccessibilityFeatures(disableAnimations: true);
    addTearDown(tester.platformDispatcher.clearAccessibilityFeaturesTestValue);
  }
  final prefs = AppPrefs(persist: false)
    ..themeMode = themeMode
    ..locale = locale;
  await tester.pumpWidget(
    RepaintBoundary(
      key: appRootKey,
      child: PbMapperApp(
        api: api,
        changes: changes.stream,
        prefs: prefs,
        needsSetup: needsSetup,
        initialPage: page,
        updates: updates,
      ),
    ),
  );
  await settle(tester);
}

/// Let async loads land and the galaxy's camera come to rest.
Future<void> settle(WidgetTester tester) async {
  for (var i = 0; i < 4; i++) {
    await tester.pump(const Duration(milliseconds: 50));
  }
  await tester.pumpAndSettle(const Duration(milliseconds: 16));
}

/// Unmount the app and let any toast's timer run out, so no timer outlives
/// the test.
Future<void> finish(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox.shrink());
  await tester.pump(const Duration(seconds: 6));
}

/// Load the app's real typefaces, which `flutter test` otherwise draws as
/// boxes, for the tests that render images meant to be looked at. Material's
/// own icon font comes from the SDK, when `FLUTTER_ROOT` says where it is.
Future<void> loadRealFonts() async {
  Future<void> family(String name, List<String> assets) async {
    final loader = FontLoader(name);
    for (final asset in assets) {
      loader.addFont(rootBundle.load(asset));
    }
    await loader.load();
  }

  await family('GoogleSansFlex', [
    'assets/fonts/GoogleSansFlex-Regular.ttf',
    'assets/fonts/GoogleSansFlex-Medium.ttf',
    'assets/fonts/GoogleSansFlex-SemiBold.ttf',
    'assets/fonts/GoogleSansFlex-Bold.ttf',
  ]);
  await family('GeistMono', [
    'assets/fonts/GeistMono-Regular.ttf',
    'assets/fonts/GeistMono-Medium.ttf',
    'assets/fonts/GeistMono-SemiBold.ttf',
  ]);
  await family('NotoSansSC', [
    'assets/fonts/NotoSansSC-Regular.subset.ttf',
    'assets/fonts/NotoSansSC-Medium.subset.ttf',
    'assets/fonts/NotoSansSC-SemiBold.subset.ttf',
  ]);
  await family('packages/lucide_icons_flutter/Lucide300', [
    'packages/lucide_icons_flutter/assets/build_font/LucideVariable-w300.ttf',
  ]);
  final flutterRoot = Platform.environment['FLUTTER_ROOT'];
  if (flutterRoot != null) {
    final icons = File(
      '$flutterRoot/bin/cache/artifacts/material_fonts/materialicons-regular.otf',
    );
    if (icons.existsSync()) {
      final loader = FontLoader('MaterialIcons')
        ..addFont(Future.value(ByteData.sublistView(icons.readAsBytesSync())));
      await loader.load();
    }
  }
}
