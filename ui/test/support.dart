import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/app.dart';
import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/shell/app_prefs.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';

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
