import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/pages/desktop_page.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'fake_desktop_api.dart';

class _HostProbe extends FakeDesktopApi {
  final privileges = <bool>[];

  @override
  Future<String> startHost({
    required String service,
    required int displayId,
    required bool allowControl,
    required bool relayOnly,
    bool privileged = false,
    bool remember = false,
    bool autoStart = false,
    bool background = false,
  }) {
    privileges.add(privileged);
    return super.startHost(
      service: service,
      displayId: displayId,
      allowControl: allowControl,
      relayOnly: relayOnly,
      privileged: privileged,
      remember: remember,
      autoStart: autoStart,
      background: background,
    );
  }

  void helper(DesktopHelper state) {
    available = DesktopCapabilities(
      host: true,
      displays: const [DesktopDisplay(1, 'Display', 1920, 1080)],
      helper: state,
    );
  }
}

Future<DesktopStore> _open(WidgetTester tester, _HostProbe api) async {
  tester.view.physicalSize = const Size(1000, 1500);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final store = DesktopStore(api);
  await tester.pumpWidget(
    MaterialApp(
      locale: const Locale('en'),
      localizationsDelegates: AppLocalizations.localizationsDelegates,
      supportedLocales: AppLocalizations.supportedLocales,
      home: DesktopPage(store: store),
    ),
  );
  await tester.pumpAndSettle();
  return store;
}

Future<void> _close(WidgetTester tester, DesktopStore store) async {
  await tester.pumpWidget(const SizedBox.shrink());
  store.dispose();
  await tester.pump();
}

void main() {
  testWidgets(
    'sharing requires secure setup unless ordinary access is explicitly chosen',
    (tester) async {
      final api = _HostProbe()..helper(const DesktopHelper());
      final store = await _open(tester, api);
      final share = find.byKey(const Key('desktop-share'));
      expect(tester.widget<FilledButton>(share).onPressed, isNull);
      await tester.tap(find.byKey(const Key('desktop-secure-setup-action')));
      await tester.pumpAndSettle();
      expect(api.helperActions, ['install']);
      expect(tester.widget<FilledButton>(share).onPressed, isNull);
      await tester.tap(find.byKey(const Key('desktop-ordinary-only')));
      await tester.pumpAndSettle();
      await tester.ensureVisible(share);
      await tester.pumpAndSettle();
      await tester.tap(share);
      await tester.pumpAndSettle();
      expect(api.privileges, [false]);
      await _close(tester, store);
    },
  );

  testWidgets(
    'protected application and running service are required before default full access',
    (tester) async {
      final api = _HostProbe()
        ..helper(
          const DesktopHelper(
            installed: true,
            running: true,
            canOpenApplication: true,
          ),
        );
      final store = await _open(tester, api);
      final share = find.byKey(const Key('desktop-share'));
      final setup = find.byKey(const Key('desktop-secure-setup-action'));
      expect(tester.widget<FilledButton>(share).onPressed, isNull);
      // A remembered background grant can survive management IPC failure.
      // Its recovery entry point must stay usable even while `sharing` is true.
      api.background = const DesktopBackground(configured: true, enabled: true);
      await store.poll();
      await tester.pumpAndSettle();
      await tester.tap(setup);
      await tester.pumpAndSettle();
      expect(api.helperActions, ['open']);
      api.helper(
        const DesktopHelper(
          installed: true,
          authorized: true,
          canOpenApplication: true,
        ),
      );
      await store.load();
      await tester.pumpAndSettle();
      await tester.tap(setup);
      await tester.pumpAndSettle();
      expect(api.helperActions, ['open', 'enable']);
      api.background = const DesktopBackground();
      api.helper(
        const DesktopHelper(
          installed: true,
          running: true,
          authorized: true,
          canOpenApplication: true,
        ),
      );
      await store.load();
      await tester.pumpAndSettle();
      expect(setup, findsNothing);
      expect(tester.widget<FilledButton>(share).onPressed, isNotNull);
      await tester.ensureVisible(share);
      await tester.pumpAndSettle();
      await tester.tap(share);
      await tester.pumpAndSettle();
      expect(api.privileges, [true]);
      await _close(tester, store);
    },
  );
}
