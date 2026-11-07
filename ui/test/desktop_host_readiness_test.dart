import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/pages/desktop_page.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'fake_desktop_api.dart';

class _HostProbe extends FakeDesktopApi {
  Completer<DesktopHostEnable>? enableResult;
  bool rejectRevoke = false;

  @override
  Future<void> revokeTrust() {
    if (rejectRevoke) {
      throw StateError('service management unavailable');
    }
    return super.revokeTrust();
  }

  @override
  Future<DesktopHostEnable> enableHost(
    String service,
    int displayId, {
    required bool allowControl,
  }) async {
    if (enableResult case final result?) {
      calls.add(('host_enable', {'service': service, 'display_id': displayId}));
      return result.future;
    }
    return super.enableHost(service, displayId, allowControl: allowControl);
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
        invitation: 'pbd1.pending',
      ),
    );
  }

  final restored = Completer<String>();

  @override
  Future<DesktopHostEnable> enableHost(
    String service,
    int displayId, {
    required bool allowControl,
  }) async {
    calls.add(('host_enable', {'service': service}));
    throw StateError('helper configuration reply was lost');
  }

  @override
  Future<String?> restoreHost() async {
    calls.add(('host_restore', const {}));
    if (argsOf('host_restore').length == 1) {
      throw StateError('helper is restarting');
    }
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
          helper: DesktopHelper(installed: true, updateRequired: true),
        );
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
      // An installed helper of an older protocol must be reinstalled (D2).
      expect(store.hostView.value.helper, DesktopHelperState.outdated);
      final enable = find.byKey(const Key('desktop-share'));
      // The one action prepares the helper too: no separate setup button or
      // second background toggle.
      expect(tester.widget<FilledButton>(enable).onPressed, isNotNull);
      await tester.tap(enable);
      await tester.pumpAndSettle();
      expect(api.argsOf('host_enable'), [
        {
          'service': 'desktop:my-computer',
          'display_id': 1,
          'allow_control': true,
        },
      ]);
      var host = store.hostView.value;
      expect(host.sharing, isTrue);
      expect(host.background, isTrue);
      expect(host.controlAllowed, isTrue);
      expect(host.invitation, isNotNull);
      expect(find.byKey(const Key('desktop-stop')), findsOneWidget);
      expect(enable, findsNothing);

      final invitation = host.invitation;
      api.rejectRevoke = true;
      await store.revokeTrust();
      await tester.pumpAndSettle();
      host = store.hostView.value;
      expect(host.sharing, isTrue);
      expect(host.invitation, invitation);
      expect(host.error, isNotNull);
      api.rejectRevoke = false;
      await store.revokeTrust();
      await tester.pumpAndSettle();
      host = store.hostView.value;
      expect(host.sharing, isFalse);
      expect(host.configured, isFalse);
      expect(host.invitation, isNull);
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
      await store.enableHost(
        displayId: 1,
        allowControl: true,
        service: 'desktop:pending',
      );
      expect(api.argsOf('host_enable'), hasLength(1));
      expect(api.argsOf('host_restore'), isEmpty);
      await tester.pump(const Duration(seconds: 1));
      expect(api.argsOf('host_restore'), hasLength(1));
      expect(store.hostView.value.error, isNotNull);
      await tester.pump(const Duration(seconds: 2));
      expect(api.argsOf('host_restore'), hasLength(2));
      expect(api.argsOf('host_enable'), hasLength(1));

      await store.stopHost();
      api.restored.complete('obsolete invitation');
      await tester.pump();
      await tester.pump(const Duration(seconds: 20));
      expect(store.hostView.value.sharing, isFalse);
      expect(store.hostView.value.invitation, isNot('obsolete invitation'));
      expect(api.argsOf('host_restore'), hasLength(2));
      expect(api.argsOf('helper_action'), isEmpty);
      store.dispose();
      await tester.pump();
    },
  );

  testWidgets(
    'restart waits for a successful handoff and cancellation keeps UI alive',
    (tester) async {
      final api = _HostProbe()..enableResult = Completer<DesktopHostEnable>();
      var exits = 0;
      final store = DesktopStore(api, onRestartReady: () => exits++);
      var enabling = store.enableHost(
        displayId: 1,
        allowControl: true,
        service: 'desktop:machine',
      );
      // A second enable while the first runs is ignored.
      await store.enableHost(
        displayId: 1,
        allowControl: true,
        service: 'desktop:machine',
      );
      expect(api.argsOf('host_enable'), hasLength(1));
      expect(exits, 0);
      api.enableResult!.completeError(StateError('consent cancelled'));
      await enabling;
      expect(exits, 0);
      expect(store.hostView.value.restarting, isFalse);
      expect(store.hostView.value.error, isNotNull);
      await tester.pump(const Duration(seconds: 20));
      expect(api.argsOf('host_restore'), isEmpty);
      expect(api.argsOf('host_enable'), hasLength(1));

      api.enableResult = Completer<DesktopHostEnable>();
      enabling = store.enableHost(
        displayId: 1,
        allowControl: true,
        service: 'desktop:machine',
      );
      expect(exits, 0);
      api.enableResult!.complete(const DesktopHostEnable(restart: true));
      await enabling;
      expect(exits, 1);
      expect(store.hostView.value.restarting, isTrue);
      expect(store.hostView.value.error, isNull);
      store.dispose();
    },
  );
}
