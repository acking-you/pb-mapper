import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/pages/desktop_page.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'fake_desktop_api.dart';

class _HostProbe extends FakeDesktopApi {
  final enables = <(String, int)>[];
  Completer<DesktopHostEnable>? enableResult;
  bool rejectRevoke = false;
  int restores = 0;

  @override
  Future<String> restoreHost() {
    restores++;
    return super.restoreHost();
  }

  @override
  Future<void> revokeTrust() {
    if (rejectRevoke) {
      throw StateError('service management unavailable');
    }
    return super.revokeTrust();
  }

  @override
  Future<DesktopHostEnable> enableHost(String service, int displayId) async {
    enables.add((service, displayId));
    if (enableResult case final result?) return result.future;
    return super.enableHost(service, displayId);
  }
}

class _PendingHostProbe extends FakeDesktopApi {
  _PendingHostProbe() {
    trust = const DesktopTrust(
      supported: true,
      matchesRelay: true,
      saved: DesktopTrustedHost(
        relay: 'test',
        service: 'desktop:pending',
        displayId: 1,
        background: true,
        autoStart: true,
      ),
    );
  }

  int restores = 0;
  int enables = 0;
  final restored = Completer<String>();
  @override
  Future<DesktopHostEnable> enableHost(String service, int displayId) async {
    enables++;
    throw StateError('helper configuration reply was lost');
  }

  @override
  Future<String> restoreHost() async {
    if (++restores == 1) throw StateError('helper is restarting');
    return restored.future;
  }
}

void main() {
  testWidgets(
    'one action upgrades local viewing and revoke reflects service ownership',
    (tester) async {
      final api = _HostProbe()
        ..available = const DesktopCapabilities(
          host: true,
          displays: [DesktopDisplay(1, 'Display', 1920, 1080)],
          helper: DesktopHelper(),
        )
        ..host = const DesktopStatus(state: 'waiting');
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
      final enable = find.byKey(const Key('desktop-share'));
      // Missing helper preparation is handled by this operation, never by a
      // separately required setup button or a second background toggle.
      expect(tester.widget<FilledButton>(enable).onPressed, isNotNull);
      await tester.tap(enable);
      await tester.pumpAndSettle();
      expect(api.enables, [('desktop:my-computer', 1)]);
      expect(store.sharing, isTrue);
      expect(store.snapshot.background?.autoStart, isTrue);
      expect(store.snapshot.host?.controlAllowed, isTrue);
      expect(store.invitation, isNotNull);
      expect(find.byKey(const Key('desktop-stop')), findsOneWidget);
      expect(enable, findsNothing);
      final invitation = store.invitation;
      api.rejectRevoke = true;
      await store.revokeTrust();
      await tester.pumpAndSettle();
      expect(store.sharing, isTrue);
      expect(store.invitation, invitation);
      expect(store.error, isNotNull);
      api.rejectRevoke = false;
      await store.revokeTrust();
      await tester.pumpAndSettle();
      expect(store.sharing, isFalse);
      expect(store.invitation, isNull);
      await tester.pumpWidget(const SizedBox.shrink());
      store.dispose();
      await tester.pump();
    },
  );

  testWidgets(
    'one-click setup resumes after IPC failure but stop rejects a late reply',
    (tester) async {
      final api = _PendingHostProbe();
      final store = DesktopStore(api);
      await store.enableHost('desktop:pending', 1);
      expect(api.enables, 1);
      expect(api.restores, 0);
      await tester.pump(const Duration(seconds: 1));
      expect(api.restores, 1);
      expect(store.error, isNotNull);
      await tester.pump(const Duration(seconds: 2));
      expect(api.restores, 2);
      expect(api.enables, 1);
      await store.stopHost();
      api.restored.complete('obsolete invitation');
      await tester.pump();
      await tester.pump(const Duration(seconds: 20));
      expect(store.invitation, isNull);
      expect(store.sharing, isFalse);
      expect(api.restores, 2);
      expect(api.helperActions, isEmpty);
      store.dispose();
      await tester.pump();
    },
  );

  testWidgets(
    'restart waits for a successful handoff and cancellation keeps UI alive',
    (tester) async {
      final api = _HostProbe()..enableResult = Completer<DesktopHostEnable>();
      var exits = 0;
      final store = DesktopStore(
        api,
        onRestartReady: () async {
          exits++;
        },
      );
      var enabling = store.enableHost('desktop:machine', 1);
      await store.enableHost('desktop:machine', 1);
      expect(api.enables, [('desktop:machine', 1)]);
      expect(exits, 0);
      api.enableResult!.completeError(StateError('consent cancelled'));
      await enabling;
      expect(exits, 0);
      expect(store.restarting, isFalse);
      expect(store.error, isNotNull);
      await tester.pump(const Duration(seconds: 20));
      expect(api.restores, 0);
      expect(api.enables, [('desktop:machine', 1)]);

      api.enableResult = Completer<DesktopHostEnable>();
      enabling = store.enableHost('desktop:machine', 1);
      expect(exits, 0);
      api.enableResult!.complete(const DesktopHostEnable(restart: true));
      await enabling;
      expect(exits, 1);
      expect(store.restarting, isTrue);
      expect(store.error, isNull);
      store.dispose();
    },
  );
}
