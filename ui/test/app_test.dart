import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/common/locale_controller.dart';
import 'package:pb_mapper_ui/src/common/setup_state.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/pages/logs_page.dart';
import 'package:pb_mapper_ui/src/pages/overview_page.dart';
import 'package:pb_mapper_ui/src/pages/relay_page.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_page.dart';
import 'package:pb_mapper_ui/src/pages/settings_page.dart';
import 'package:pb_mapper_ui/src/pages/tunnels_page.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
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

/// Windows' face, Segoe UI, is the system's and CI has none: there its words
/// are em-wide boxes. Google Sans Flex stands in under its name. It sets a
/// little wider (the full capsule 366 px against Segoe UI's 343), so a bar
/// that fits here fits on Windows.
Future<void> _loadSegoeUiStandIn() async {
  final loader = FontLoader('Segoe UI');
  for (final weight in ['Regular', 'Medium', 'SemiBold', 'Bold']) {
    loader.addFont(rootBundle.load('assets/fonts/GoogleSansFlex-$weight.ttf'));
  }
  await loader.load();
}

void main() {
  testWidgets(
    'the --desktop hand-off (initialPage: remote) lands on Remote desktop',
    (tester) async {
      await pumpApp(tester, FakePbMapperApi(), page: AppPage.remote);
      expect(find.byType(RemotePage), findsOneWidget);
      expect(find.text('Remote desktop'), findsWidgets);
      await finish(tester);
    },
  );
  group('shell', () {
    testWidgets('a wide window has a rail with the six pages', (tester) async {
      await pumpApp(tester, FakePbMapperApi());
      expect(find.byType(NavRail), findsOneWidget);
      expect(find.byType(NavBar), findsNothing);
      expect(find.byType(OverviewPage), findsOneWidget);
      for (final page in AppPage.railEntries) {
        expect(find.byKey(Key('nav-${page.name}')), findsOneWidget);
      }

      await tester.tap(find.byKey(const Key('nav-tunnels')));
      await settle(tester);
      expect(find.byType(TunnelsPage), findsOneWidget);
      await tester.tap(find.byKey(const Key('nav-remote')));
      await settle(tester);
      expect(find.byType(RemotePage), findsOneWidget);
      await finish(tester);
    });

    testWidgets(
      'a phone has five pages at the bottom, and Logs inside Settings',
      (tester) async {
        await pumpApp(tester, FakePbMapperApi(), size: const Size(393, 852));
        expect(find.byType(NavBar), findsOneWidget);
        expect(find.byType(NavRail), findsNothing);
        for (final page in AppPage.barEntries) {
          expect(find.byKey(Key('nav-${page.name}')), findsOneWidget);
        }
        expect(find.byKey(const Key('nav-logs')), findsNothing);

        Future<void> openLogs() async {
          await tester.scrollUntilVisible(
            find.byKey(const Key('settings-logs')),
            200,
            scrollable: find
                .descendant(
                  of: find.byKey(const Key('settings-list')),
                  matching: find.byType(Scrollable),
                )
                .first,
          );
          await tester.ensureVisible(find.byKey(const Key('settings-logs')));
          await settle(tester);
          await tester.tap(find.byKey(const Key('settings-logs')));
          await settle(tester);
          expect(find.byType(LogsPage), findsOneWidget);
        }

        await tester.tap(find.byKey(const Key('nav-settings')));
        await settle(tester);
        expect(find.byType(SettingsPage), findsOneWidget);
        await openLogs();
        await tester.tap(find.byKey(const Key('page-back')));
        await settle(tester);
        expect(find.byType(SettingsPage), findsOneWidget);

        // Settings stands for Logs in the bar, and still answers a tap.
        await openLogs();
        await tester.tap(find.byKey(const Key('nav-settings')));
        await settle(tester);
        expect(find.byType(SettingsPage), findsOneWidget);
        await finish(tester);
      },
    );

    testWidgets('the rail says whether the relay answers', (tester) async {
      await pumpApp(tester, FakePbMapperApi(relayUp: false));
      expect(
        find.descendant(
          of: find.byKey(const Key('rail-relay-status')),
          matching: find.text('Relay unreachable'),
        ),
        findsOneWidget,
      );
      // It opens the relay itself, where the reason is.
      await tester.tap(find.byKey(const Key('rail-relay-status')));
      await settle(tester);
      expect(find.byType(RelayPage), findsOneWidget);
      expect(find.textContaining('connection refused'), findsOneWidget);
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
      for (final page in AppPage.values) {
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
          page: page,
          size: const Size(320, 640),
          locale: const Locale('zh'),
        );
        expect(tester.takeException(), isNull, reason: page.name);
        await finish(tester);
      }
    });

    testWidgets('the title bar never overflows while this computer is viewed', (
      tester,
    ) async {
      final messenger = tester.binding.defaultBinaryMessenger;
      const window = MethodChannel('window_manager');
      messenger.setMockMethodCallHandler(
        window,
        (call) async => call.method == 'isMaximized' ? false : null,
      );
      addTearDown(() => messenger.setMockMethodCallHandler(window, null));
      // The test font draws every glyph a full em wide, about twice real
      // text: measure the bar with the real faces instead.
      await loadRealFonts();
      await _loadSegoeUiStandIn();
      debugDefaultTargetPlatformOverride = TargetPlatform.windows;
      try {
        for (final size in const [Size(720, 640), Size(1180, 780)]) {
          for (final page in AppPage.railEntries) {
            final api = FakePbMapperApi();
            api.desktop.hostSnapshot = viewedHost(controlling: true);
            await pumpApp(tester, api, page: page, size: size);
            final at = '${page.name} at ${size.width}';
            expect(tester.takeException(), isNull, reason: at);
            // Remote desktop's own page offers the same controls itself.
            expect(
              find.byKey(const Key('sharing-indicator')),
              page == AppPage.remote ? findsNothing : findsOneWidget,
              reason: at,
            );
            // At the default window the capsule has room to say what is
            // happening.
            if (size.width == 1180 && page != AppPage.remote) {
              expect(find.text('Being controlled'), findsOneWidget, reason: at);
            }
            await finish(tester);
          }
        }
      } finally {
        debugDefaultTargetPlatformOverride = null;
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

    testWidgets('keeps a saved credential that is slow to load', (
      tester,
    ) async {
      final saved = 'k' * 32;
      final api = FakePbMapperApi(
        config: ConfigStatus(
          serverAddress: 'localhost:7666',
          keepAliveEnabled: true,
          msgHeaderKey: saved,
        ),
      )..configDelay = const Duration(milliseconds: 120);
      await pumpApp(tester, api, needsSetup: true);
      final credential = tester.widget<TextField>(
        find.byKey(const Key('setup-credential')),
      );
      expect(credential.controller!.text, saved);

      await tester.enterText(
        find.byKey(const Key('setup-address')),
        'relay.example:7666',
      );
      await tester.tap(find.byKey(const Key('setup-continue')));
      await settle(tester);
      expect(api.config.msgHeaderKey, saved);
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
