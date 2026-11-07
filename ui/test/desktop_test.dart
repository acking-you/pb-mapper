import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'fake_desktop_api.dart';
import 'fake_pb_mapper_api.dart';
import 'support.dart';

void main() {
  testWidgets(
    'background sharing survives closing the app and remains explicitly stoppable',
    (tester) async {
      final api = FakePbMapperApi();
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
      api.desktop.trust = const DesktopTrust(supported: true);
      await pumpApp(tester, api, page: AppPage.tunnels);
      await tester.tap(find.byKey(const Key('nav-remote')));
      await settle(tester);
      await tester.scrollUntilVisible(
        find.byKey(const Key('desktop-share')),
        250,
        scrollable: find.byType(Scrollable).last,
      );
      await Scrollable.ensureVisible(
        tester.element(find.byKey(const Key('desktop-share'))),
        alignment: 0.5,
      );
      await settle(tester);
      await tester.tap(find.byKey(const Key('desktop-share')));
      await settle(tester);
      var background = api.desktop.hostSnapshot.background;
      expect(background?.enabled, isTrue);
      expect(background?.autoStart, isTrue);
      expect(background?.host?.controlAllowed, isTrue);
      await finish(tester);
      expect(api.desktop.hostSnapshot.background?.enabled, isTrue);

      // An enabled grant may remain cached when management IPC is lost. SCM
      // disable/uninstall must still be reachable without first stopping it.
      api.desktop.hostSnapshot = const DesktopHostSnapshot(
        background: DesktopBackground(
          configured: true,
          enabled: true,
          autoStart: true,
          invitation: FakeDesktopApi.backgroundInvitation,
          message: 'Management connection lost',
        ),
      );
      await pumpApp(tester, api, page: AppPage.tunnels);
      await tester.tap(find.byKey(const Key('nav-remote')));
      await settle(tester);
      final maintenance = find.byKey(const Key('desktop-helper-maintenance'));
      await Scrollable.ensureVisible(tester.element(maintenance));
      await tester.tap(maintenance);
      await settle(tester);
      final disable = find.byKey(const Key('desktop-helper-disable'));
      expect(tester.widget<TextButton>(disable).onPressed, isNotNull);
      expect(
        tester
            .widget<TextButton>(
              find.byKey(const Key('desktop-helper-uninstall')),
            )
            .onPressed,
        isNotNull,
      );
      await Scrollable.ensureVisible(tester.element(disable), alignment: 0.5);
      await settle(tester);
      await tester.tap(disable);
      await settle(tester);
      expect(api.desktop.argsOf('helper_action'), [
        {'action': 'disable'},
      ]);
      final stop = find.byKey(const Key('desktop-stop'));
      await Scrollable.ensureVisible(tester.element(stop), alignment: 0.5);
      await settle(tester);
      await tester.tap(stop);
      await settle(tester);
      background = api.desktop.hostSnapshot.background;
      expect(api.desktop.hostSnapshot.host, isNull);
      expect(background?.configured, isTrue);
      expect(background?.enabled, isFalse);
      expect(background?.autoStart, isFalse);
      await finish(tester);
    },
  );

  testWidgets(
    'compact host flow enables control, shows sharing on the bar and stops',
    (tester) async {
      final api = FakePbMapperApi();
      await pumpApp(
        tester,
        api,
        page: AppPage.tunnels,
        size: const Size(390, 844),
      );
      final sharingDot = find.descendant(
        of: find.byKey(const Key('nav-remote')),
        matching: find.byType(StatusDot),
      );
      expect(sharingDot, findsNothing);
      await tester.tap(find.byKey(const Key('nav-remote')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('desktop-share')));
      await settle(tester);
      expect(api.desktop.hostSnapshot.background?.host?.controlAllowed, isTrue);
      expect(find.byKey(const Key('desktop-stop')), findsOneWidget);
      expect(tester.takeException(), isNull);
      // Sharing shows on Remote desktop's entry from every page.
      await tester.tap(find.byKey(const Key('nav-tunnels')));
      await settle(tester);
      expect(sharingDot, findsOneWidget);
      await tester.tap(find.byKey(const Key('nav-remote')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('desktop-stop')));
      await settle(tester);
      expect(api.desktop.hostSnapshot.background?.enabled, isFalse);
      expect(sharingDot, findsNothing);
      await finish(tester);
    },
  );
}
