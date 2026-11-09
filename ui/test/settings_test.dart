import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart'
    show LanTransport, RelayTransport;
import 'package:pb_mapper_ui/src/shell/app_scope.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

Future<void> _open(WidgetTester tester, FakePbMapperApi api) =>
    pumpApp(tester, api, page: AppPage.settings);

/// Scroll the settings list until [target], perhaps not built yet, is shown
/// whole: a scroll that stops with it on the edge leaves a tap off screen.
Future<void> _reveal(WidgetTester tester, Finder target) async {
  await tester.scrollUntilVisible(
    target,
    200,
    scrollable: find
        .descendant(
          of: find.byKey(const Key('settings-list')),
          matching: find.byType(Scrollable),
        )
        .first,
  );
  await tester.ensureVisible(target);
  await tester.pump();
}

void main() {
  testWidgets('the relay form is filled from the saved config', (tester) async {
    await _open(tester, FakePbMapperApi());
    final field = tester.widget<TextField>(
      find.byKey(const Key('settings-address')),
    );
    expect(field.controller?.text, 'lb7666.top:7666');
    await finish(tester);
  });

  testWidgets('saving checks the address, then writes it', (tester) async {
    final api = FakePbMapperApi();
    await _open(tester, api);
    await tester.enterText(find.byKey(const Key('settings-address')), 'nope');
    await tester.tap(find.byKey(const Key('settings-save')));
    await settle(tester);
    expect(api.calls, isEmpty);

    await tester.enterText(
      find.byKey(const Key('settings-address')),
      'relay.example:7666',
    );
    await tester.enterText(
      find.byKey(const Key('settings-credential')),
      'pbmt1_token',
    );
    await tester.tap(find.byKey(const Key('settings-save')));
    await settle(tester);
    expect(api.calls, ['updateConfig(relay.example:7666)']);
    expect(api.config.msgHeaderKey, 'pbmt1_token');
    await finish(tester);
  });

  testWidgets('testing the relay says what it answered', (tester) async {
    final api = FakePbMapperApi(serverServices: ['nas', 'cache']);
    await _open(tester, api);
    await tester.tap(find.byKey(const Key('settings-test')));
    await settle(tester);
    expect(
      find.text('The relay answered in 18 ms · 2 services'),
      findsOneWidget,
    );
    await finish(tester);

    final down = FakePbMapperApi(relayUp: false);
    await _open(tester, down);
    await tester.tap(find.byKey(const Key('settings-test')));
    await settle(tester);
    expect(
      find.text("The relay didn't answer: connection refused"),
      findsOneWidget,
    );
    await finish(tester);
  });

  testWidgets('testing an edited relay saves it first', (tester) async {
    final api = FakePbMapperApi();
    await _open(tester, api);
    await tester.enterText(
      find.byKey(const Key('settings-address')),
      'relay.example:7666',
    );
    await tester.tap(find.byKey(const Key('settings-test')));
    await settle(tester);
    // Not the old address checked while the new one sits unsaved.
    expect(api.calls, ['updateConfig(relay.example:7666)']);
    expect(
      find.text('The relay answered in 18 ms · 0 services'),
      findsOneWidget,
    );
    await finish(tester);
  });

  testWidgets('a setup string from another computer applies', (tester) async {
    final api = FakePbMapperApi();
    await _open(tester, api);
    final payload = base64Encode(
      utf8.encode(
        jsonEncode({
          'version': 1,
          'serverAddress': 'other.example:7666',
          'keepAliveEnabled': false,
          'msgHeaderKey': 'a' * 32,
          'relayTransport': 'quic',
        }),
      ),
    );
    await tester.tap(find.byKey(const Key('settings-paste-setup')));
    await settle(tester);
    await tester.enterText(find.byKey(const Key('setup-string')), payload);
    await tester.tap(find.byKey(const Key('setup-string-apply')));
    await settle(tester);
    expect(api.calls, ['updateConfig(other.example:7666, quic)']);
    expect(api.config.keepAliveEnabled, isFalse);
    await finish(tester);
  });

  testWidgets('the carrier says what the relay offers, and moves tunnels', (
    tester,
  ) async {
    final api = FakePbMapperApi(
      services: [
        fakeService(serviceKey: 'nas', relayTransport: RelayTransport.tcp),
      ],
    );
    await _open(tester, api);
    expect(find.text('This relay offers QUIC on UDP 7667.'), findsOneWidget);
    await tester.tap(find.byKey(const Key('settings-carrier-quic')));
    await tester.tap(find.byKey(const Key('settings-save')));
    await settle(tester);
    expect(api.calls, ['updateConfig(lb7666.top:7666, quic)']);
    // Kept on TCP when the answer is later.
    await tester.tap(find.text('Later'));
    await settle(tester);
    expect(api.calls, hasLength(1));
    expect(api.services.single.relayTransport, RelayTransport.tcp);
    await finish(tester);

    final bare = FakePbMapperApi()..relayQuicPort = null;
    await _open(tester, bare);
    expect(find.text('This relay offers TCP only.'), findsOneWidget);
    await tester.tap(find.byKey(const Key('settings-carrier-quic')));
    await settle(tester);
    expect(find.textContaining("doesn't offer QUIC"), findsOneWidget);
    await finish(tester);
  });

  testWidgets('LAN-first applies at once and restarts what it reaches', (
    tester,
  ) async {
    final api = FakePbMapperApi(
      services: [
        fakeService(serviceKey: 'nas'),
        fakeService(serviceKey: 'cam', allowDirect: false),
        fakeService(serviceKey: 'old', status: 'stopped'),
      ],
    );
    await _open(tester, api);
    final lanFirst = find.byKey(const Key('settings-lan-first'));
    await _reveal(tester, lanFirst);
    await tester.tap(lanFirst);
    await settle(tester);
    // Saved with the relay settings as they were, without pressing Save.
    expect(api.calls, ['updateConfig(lb7666.top:7666, lan-first off)']);
    expect(api.config.lanFirst, isFalse);
    // Only the running share that may go direct is offered a restart.
    await tester.tap(find.byKey(const Key('lan-first-restart-now')));
    await settle(tester);
    expect(api.calls.skip(1), ['registerService(nas)']);
    await finish(tester);
  });

  testWidgets(
    'the LAN transport applies at once to the connections it reaches',
    (tester) async {
      final api = FakePbMapperApi(
        services: [fakeService(serviceKey: 'nas')],
        clients: [
          fakeClient(serviceKey: 'desk'),
          fakeClient(serviceKey: 'relayed', allowDirect: false),
        ],
      );
      await _open(tester, api);
      final tcp = find.byKey(const Key('settings-lan-transport-tcp'));
      await _reveal(tester, tcp);
      await tester.tap(tcp);
      await settle(tester);
      expect(api.calls, ['updateConfig(lb7666.top:7666, lan tcp)']);
      expect(api.config.lanTransport, LanTransport.tcp);
      // A share serves whatever it is asked for: only the connection that may
      // go direct is offered a restart.
      await tester.tap(find.byKey(const Key('lan-first-restart-now')));
      await settle(tester);
      expect(api.calls.skip(1), [
        'disconnectService(desk)',
        'connectService(desk)',
      ]);
      await finish(tester);
    },
  );

  testWidgets('a string that is not a setup string is refused', (tester) async {
    final api = FakePbMapperApi();
    await _open(tester, api);
    await tester.tap(find.byKey(const Key('settings-paste-setup')));
    await settle(tester);
    await tester.enterText(find.byKey(const Key('setup-string')), 'garbage!');
    await tester.tap(find.byKey(const Key('setup-string-apply')));
    await settle(tester);
    expect(api.calls, isEmpty);
    expect(find.textContaining("isn't a setup string"), findsOneWidget);
    await finish(tester);
  });

  testWidgets('the theme switch repaints the app dark', (tester) async {
    await _open(tester, FakePbMapperApi());
    final dark = find.byKey(const Key('settings-theme-dark'));
    await _reveal(tester, dark);
    await tester.tap(dark);
    await settle(tester);
    final context = tester.element(find.byKey(const Key('settings-list')));
    expect(Theme.of(context).brightness, Brightness.dark);
    await finish(tester);
  });

  testWidgets('the language switch speaks Chinese at once', (tester) async {
    await _open(tester, FakePbMapperApi());
    final chinese = find.byKey(const Key('settings-language-zh'));
    await _reveal(tester, chinese);
    await tester.tap(chinese);
    await settle(tester);
    expect(find.text('中继服务器'), findsOneWidget);
    await finish(tester);
  });
}
