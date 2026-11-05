import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/pages/desktop_page.dart';
import 'package:pb_mapper_ui/src/pages/desktop_viewer.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';

import 'fake_desktop_api.dart';

class _SlowConnectionApi extends FakeDesktopApi {
  final connectGate = Completer<void>();
  final connectStarted = Completer<void>();

  @override
  Future<void> connect(String invitation, {required bool relayOnly}) async {
    if (!connectStarted.isCompleted) connectStarted.complete();
    await connectGate.future;
    viewer = const DesktopStatus(state: 'connecting');
  }
}

void main() {
  testWidgets('opening during a slow connection keeps one viewer route', (
    tester,
  ) async {
    final api = _SlowConnectionApi()
      ..available = const DesktopCapabilities(host: false);
    final store = DesktopStore(api);
    await tester.pumpWidget(
      MaterialApp(
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: DesktopPage(store: store),
      ),
    );
    await tester.pump();
    await tester.enterText(
      find.byKey(const Key('desktop-invitation')),
      'private test invitation',
    );
    await tester.tap(find.byKey(const Key('desktop-connect')));
    await tester.pump();
    expect(api.connectStarted.isCompleted, isTrue);
    await tester.tap(find.text('Open'));
    await tester.pump();
    final route = ModalRoute.of(
      tester.element(find.byType(DesktopViewer, skipOffstage: false)),
    )!;
    final transitionDuration = route.transitionDuration;
    await tester.pump(transitionDuration);
    expect(find.byType(DesktopViewer, skipOffstage: false), findsOneWidget);

    api.connectGate.complete();
    await tester.pump();
    await tester.pump(transitionDuration);
    expect(find.byType(DesktopViewer, skipOffstage: false), findsOneWidget);
    var removed = false;
    unawaited(route.completed.then((_) => removed = true));
    await tester.pageBack();
    for (var frame = 0; frame < 30 && !removed; frame++) {
      await tester.pump(const Duration(milliseconds: 50));
    }
    expect(removed, isTrue);
    expect(find.byType(DesktopViewer, skipOffstage: false), findsNothing);
    expect(find.byType(DesktopPage), findsOneWidget);

    // Popping releases the route guard so the existing session can reopen.
    await tester.tap(find.text('Open'));
    await tester.pump();
    await tester.pump(transitionDuration);
    expect(find.byType(DesktopViewer, skipOffstage: false), findsOneWidget);
    final reopened = ModalRoute.of(
      tester.element(find.byType(DesktopViewer, skipOffstage: false)),
    )!;
    var reopenedRemoved = false;
    unawaited(reopened.completed.then((_) => reopenedRemoved = true));
    await tester.pageBack();
    for (var frame = 0; frame < 30 && !reopenedRemoved; frame++) {
      await tester.pump(const Duration(milliseconds: 50));
    }
    expect(reopenedRemoved, isTrue);
    await tester.pumpWidget(const SizedBox.shrink());
    store.dispose();
    expect(tester.takeException(), isNull);
  });
}
