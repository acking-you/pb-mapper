import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/pages/remote/invite_scanner.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_page.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_screen.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';

import 'fake_desktop_api.dart';
import 'fake_pb_mapper_api.dart';
import 'fake_remote_computer_vault.dart';
import 'support.dart';

/// A host whose permission change can be held in flight: the store is busy
/// with it.
class _HostDesktop extends FakeDesktopApi {
  Completer<void>? holdPermission;

  @override
  Future<void> allowControl(bool allow) async {
    await holdPermission?.future;
    return super.allowControl(allow);
  }
}

class _Api extends FakePbMapperApi {
  final _host = _HostDesktop();

  @override
  _HostDesktop get desktop => _host;
}

Finder _key(String key) => find.byKey(Key(key));

/// Scrolls [finder] into view and taps it, without waiting for anything
/// else to settle.
Future<void> _tapIn(WidgetTester tester, Finder finder) async {
  await tester.ensureVisible(finder);
  await tester.pump();
  await tester.tap(finder);
}

void main() {
  setUp(() => InviteScanner.debugSupportedOverride = false);
  tearDown(() => InviteScanner.debugSupportedOverride = null);

  testWidgets(
    'an invitation is recognised before connecting and never shown in full',
    (tester) async {
      final api = FakePbMapperApi();
      final vault = FakeRemoteComputerVault()
        ..records.add(
          const RemoteComputer(
            id: '0123456789abcdef0123456789abcdef',
            name: 'Lab',
            service: 'desktop:lab',
            fingerprint: 'FFFF-0000-FFFF-0000',
            relay: 'localhost:7666',
            namespace: 0,
          ),
        );
      await pumpApp(tester, api, page: AppPage.tunnels, vault: vault);
      await tester.tap(_key('nav-remote'));
      await settle(tester);
      await tester.tap(_key('remote-add'));
      await settle(tester);

      // A setup string is refused for what it is, and nothing connects.
      await tester.enterText(_key('add-invite'), 'pbmt1_not-an-invitation');
      await tester.pump(const Duration(milliseconds: 300));
      await settle(tester);
      expect(find.textContaining('pb-mapper setup string'), findsOneWidget);
      expect(
        tester.widget<FilledButton>(_key('add-connect')).onPressed,
        isNull,
      );

      const invitation = 'pbd1.office-pc.SECRET-TOKEN-0123';
      await tester.enterText(_key('add-invite'), invitation);
      await tester.pump(const Duration(milliseconds: 300));
      await settle(tester);
      expect(find.text('desktop:office-pc'), findsOneWidget);
      expect(find.textContaining('AB12-CD34-EF56-7890'), findsOneWidget);
      expect(find.textContaining('SECRET-TOKEN'), findsNothing);
      expect(tester.widget<SwitchListTile>(_key('add-remember')).value, isTrue);

      await tester.tap(_key('add-connect'));
      // The viewer's Connecting spinner never settles.
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 50));
      }
      expect(api.desktop.argsOf('connect').single['invitation'], invitation);
      final desktop = AppScope.of(
        tester.element(find.byType(RemoteViewerScreen)),
      ).store.desktop;
      final session = desktop.session.value;
      expect(session.target?.computerId, matches(RegExp(r'^[0-9a-f]{32}$')));
      expect(session.phase, DesktopPhase.connecting);
      expect(find.byType(RemotePage), findsNothing);
      // Nothing is remembered before the first picture.
      expect(vault.ops, isNot(contains('remember')));
      await finish(tester);
    },
  );

  testWidgets(
    'enabling shares view-only or with control, Stop works while busy, and '
    'helper maintenance stays reachable',
    (tester) async {
      // An installed helper of an older protocol: the one action
      // reinstalls it too (D2). This share is view-only.
      var api = _Api();
      api.desktop.available = const DesktopCapabilities(
        host: true,
        displays: [DesktopDisplay(1, 'Display 1', 1920, 1080)],
        helper: DesktopHelper(installed: true, updateRequired: true),
      );
      await pumpApp(tester, api, page: AppPage.remote);
      await tester.tap(_key('host-allow-control'));
      await settle(tester);
      await _tapIn(tester, _key('host-enable'));
      await settle(tester);
      final enable = api.desktop.argsOf('host_enable').single;
      expect(enable['service'], startsWith('desktop:'));
      expect(enable['display_id'], 1);
      expect(enable['allow_control'], isFalse);
      expect(api.desktop.hostSnapshot.background?.enabled, isTrue);
      expect(_key('host-enable'), findsNothing);

      // Allowing control waits its turn; Stop never waits behind it.
      final hold = api.desktop.holdPermission = Completer<void>();
      final control = find.descendant(
        of: _key('host-permission'),
        matching: find.text('View and control'),
      );
      await _tapIn(tester, control);
      await tester.pump();
      await _tapIn(tester, _key('host-stop'));
      await tester.pump();
      expect(api.desktop.ops, contains('host_stop'));
      expect(api.desktop.argsOf('host_permission'), isEmpty);
      hold.complete();
      await settle(tester);
      expect(api.desktop.argsOf('host_permission'), [
        {'allow_control': true},
      ]);
      await finish(tester);

      // A grant cached while service management is lost: the helper's
      // maintenance is reachable without stopping first, and Stop works.
      api = _Api();
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
      api.desktop.hostSnapshot = const DesktopHostSnapshot(
        background: DesktopBackground(
          configured: true,
          enabled: true,
          autoStart: true,
          invitation: FakeDesktopApi.backgroundInvitation,
          message: 'Management connection lost',
        ),
      );
      await pumpApp(tester, api, page: AppPage.remote);
      await _tapIn(tester, _key('host-more'));
      await settle(tester);
      expect(
        tester.widget<MenuItemButton>(_key('host-helper-uninstall')).onPressed,
        isNotNull,
      );
      await tester.tap(_key('host-helper-disable'));
      await settle(tester);
      expect(api.desktop.argsOf('helper_action'), [
        {'action': 'disable'},
      ]);
      await _tapIn(tester, _key('host-stop'));
      await settle(tester);
      final background = api.desktop.hostSnapshot.background;
      expect(api.desktop.hostSnapshot.host, isNull);
      expect(background?.configured, isTrue);
      expect(background?.enabled, isFalse);
      expect(background?.autoStart, isFalse);
      await finish(tester);

      // On a phone-sized window: control by default, and sharing shows on
      // Remote's bar entry from every page until it stops.
      api = _Api();
      await pumpApp(
        tester,
        api,
        page: AppPage.tunnels,
        size: const Size(390, 844),
      );
      final sharingDot = find.descendant(
        of: _key('nav-remote'),
        matching: find.byType(StatusDot),
      );
      expect(sharingDot, findsNothing);
      await tester.tap(_key('nav-remote'));
      await settle(tester);
      await _tapIn(tester, _key('host-enable'));
      await settle(tester);
      expect(api.desktop.hostSnapshot.background?.host?.controlAllowed, isTrue);
      expect(tester.takeException(), isNull);
      await tester.tap(_key('nav-tunnels'));
      await settle(tester);
      expect(sharingDot, findsOneWidget);
      await tester.tap(_key('nav-remote'));
      await settle(tester);
      await _tapIn(tester, _key('host-stop'));
      await settle(tester);
      expect(api.desktop.hostSnapshot.background?.enabled, isFalse);
      expect(sharingDot, findsNothing);
      await finish(tester);
    },
  );

  testWidgets(
    'the invitation QR shows only on request and hides when the page is left '
    'or the app goes inactive',
    (tester) async {
      final api = FakePbMapperApi();
      api.desktop.hostSnapshot = viewedHost();
      await pumpApp(tester, api, page: AppPage.remote);
      expect(_key('host-qr-panel'), findsNothing);
      await _tapIn(tester, _key('host-qr'));
      await settle(tester);
      expect(_key('host-qr-panel'), findsOneWidget);

      await tester.tap(_key('nav-tunnels'));
      await settle(tester);
      await tester.tap(_key('nav-remote'));
      await settle(tester);
      expect(_key('host-qr-panel'), findsNothing);

      await _tapIn(tester, _key('host-qr'));
      await settle(tester);
      expect(_key('host-qr-panel'), findsOneWidget);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      await tester.pump();
      expect(_key('host-qr-panel'), findsNothing);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      await finish(tester);
    },
  );

  testWidgets(
    "the viewer's controls stay on screen when the host card scrolls away",
    (tester) async {
      final api = FakePbMapperApi();
      api.desktop.hostSnapshot = viewedHost();
      await pumpApp(tester, api, page: AppPage.remote);
      // The viewer panel offers them, so no pill floats over the page.
      expect(_key('host-viewer'), findsOneWidget);
      expect(_key('sharing-fallback'), findsNothing);

      final page = find.byKey(const PageStorageKey('remote-page'));
      await tester.drag(page, const Offset(0, -600));
      await settle(tester);
      expect(_key('sharing-fallback'), findsOneWidget);
      await tester.drag(page, const Offset(0, 600));
      await settle(tester);
      expect(_key('sharing-fallback'), findsNothing);
      await finish(tester);
    },
  );
}
