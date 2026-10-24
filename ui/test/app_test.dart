import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/setup_state.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/pages/overview_page.dart';
import 'package:pb_mapper_ui/src/pages/settings_page.dart';
import 'package:pb_mapper_ui/src/pages/tunnels_page.dart';
import 'package:pb_mapper_ui/src/shell/navigation.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

FakePbMapperApi _fresh() => FakePbMapperApi(
  config: const ConfigStatus(
    serverAddress: 'localhost:7666',
    keepAliveEnabled: true,
    msgHeaderKey: '',
  ),
);

void main() {
  group('shell', () {
    testWidgets('a wide window has a rail with the four pages', (tester) async {
      await pumpApp(tester, FakePbMapperApi());
      expect(find.byType(NavRail), findsOneWidget);
      expect(find.byType(NavBar), findsNothing);
      expect(find.byType(OverviewPage), findsOneWidget);
      for (final page in ['overview', 'tunnels', 'logs', 'settings']) {
        expect(find.byKey(Key('nav-$page')), findsOneWidget);
      }

      await tester.tap(find.byKey(const Key('nav-tunnels')));
      await settle(tester);
      expect(find.byType(TunnelsPage), findsOneWidget);
      await finish(tester);
    });

    testWidgets('a phone navigates from the bottom', (tester) async {
      await pumpApp(tester, FakePbMapperApi(), size: const Size(393, 852));
      expect(find.byType(NavBar), findsOneWidget);
      expect(find.byType(NavRail), findsNothing);

      await tester.tap(find.byKey(const Key('nav-settings')));
      await settle(tester);
      expect(find.byType(SettingsPage), findsOneWidget);
      await finish(tester);
    });

    testWidgets('the rail says whether the relay answers', (tester) async {
      await pumpApp(tester, FakePbMapperApi(relayUp: false));
      expect(
        find.descendant(
          of: find.byKey(const Key('nav-relay')),
          matching: find.text('Relay unreachable'),
        ),
        findsOneWidget,
      );
      await tester.tap(find.byKey(const Key('nav-relay')));
      await settle(tester);
      expect(find.byType(SettingsPage), findsOneWidget);
      await finish(tester);
    });

    testWidgets('the chrome speaks Chinese', (tester) async {
      await pumpApp(tester, FakePbMapperApi(), locale: const Locale('zh'));
      expect(find.text('总览'), findsWidgets);
      expect(find.text('隧道'), findsOneWidget);
      expect(find.text('新建隧道'), findsOneWidget);
      await finish(tester);
    });

    testWidgets('nothing on screen overflows on a small phone', (tester) async {
      for (final page in ['overview', 'tunnels', 'logs', 'settings']) {
        await pumpApp(
          tester,
          FakePbMapperApi(
            services: [
              fakeService(
                serviceKey: 'a-rather-long-service-key-for-a-phone',
                status: 'failed',
                message: 'the relay closed the connection',
              ),
            ],
            clients: [fakeClient(serviceKey: 'db')],
            serverServices: ['db-primary', 'cache'],
          ),
          size: const Size(320, 640),
          locale: const Locale('zh'),
        );
        await tester.tap(find.byKey(Key('nav-$page')).last);
        await settle(tester);
        expect(tester.takeException(), isNull, reason: page);
        await finish(tester);
      }
    });
  });

  group('first run', () {
    testWidgets('opens setup, and skipping lands on the overview', (
      tester,
    ) async {
      await pumpApp(tester, _fresh(), needsSetup: true);
      expect(find.byKey(const Key('setup-title')), findsOneWidget);
      expect(find.byType(NavRail), findsNothing);

      await tester.tap(find.byKey(const Key('setup-skip')));
      await settle(tester);
      expect(find.byType(OverviewPage), findsOneWidget);
      await finish(tester);
    });

    testWidgets('refuses an address without a port', (tester) async {
      final api = _fresh();
      await pumpApp(tester, api, needsSetup: true);
      await tester.enterText(
        find.byKey(const Key('setup-address')),
        'relay.example',
      );
      await tester.tap(find.byKey(const Key('setup-continue')));
      await settle(tester);
      expect(find.textContaining('host:port'), findsOneWidget);
      expect(api.calls, isEmpty);
      await finish(tester);
    });

    testWidgets('refuses a credential of the wrong shape', (tester) async {
      final api = _fresh();
      await pumpApp(tester, api, needsSetup: true);
      await tester.enterText(
        find.byKey(const Key('setup-address')),
        'relay.example:7666',
      );
      await tester.enterText(
        find.byKey(const Key('setup-credential')),
        'too-short',
      );
      await tester.tap(find.byKey(const Key('setup-continue')));
      await settle(tester);
      expect(find.textContaining('pbmt1_'), findsWidgets);
      expect(api.calls, isEmpty);
      await finish(tester);
    });

    testWidgets('saves the relay, then shares a first service', (tester) async {
      final api = _fresh();
      await pumpApp(tester, api, needsSetup: true);
      await tester.enterText(
        find.byKey(const Key('setup-address')),
        'relay.example:7666',
      );
      await tester.tap(find.byKey(const Key('setup-continue')));
      await settle(tester);
      expect(api.calls, ['updateConfig(relay.example:7666)']);

      await tester.tap(find.byKey(const Key('setup-choose-share')));
      await settle(tester);
      await tester.enterText(find.byKey(const Key('tunnel-key')), 'web');
      await tester.tap(find.byKey(const Key('tunnel-submit')));
      await settle(tester);

      expect(api.calls.last, 'registerService(web)');
      expect(find.byType(OverviewPage), findsOneWidget);
      await finish(tester);
    });
  });

  group('SetupState', () {
    test('the default address alone is not a configured relay', () {
      expect(SetupState.isServerConfigured('localhost:7666'), isFalse);
      expect(SetupState.isServerConfigured('  '), isFalse);
      expect(SetupState.isServerConfigured('relay.example:7666'), isTrue);
    });

    test('a fresh install needs setup, a used one does not', () async {
      expect(await SetupState.needsSetup(_fresh()), isTrue);
      expect(
        await SetupState.needsSetup(
          _fresh()..services = [fakeService(serviceKey: 'web')],
        ),
        isFalse,
      );
    });
  });

  group('LocaleController', () {
    test('falls back to English for an unsupported system language', () {
      expect(LocaleController.resolve(const Locale('fr')).languageCode, 'en');
      expect(LocaleController.resolve(const Locale('zh')).languageCode, 'zh');
    });
  });
}
