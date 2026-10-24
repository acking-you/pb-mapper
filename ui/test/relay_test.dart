import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

int get _now => DateTime.now().millisecondsSinceEpoch ~/ 1000;

/// A relay with something of everything: a service this computer shares, one
/// it connects to, one registered by a temporary credential, connections, a
/// credential and a direct session.
FakePbMapperApi _busy() {
  final now = _now;
  return FakePbMapperApi(
      services: [fakeService(serviceKey: 'nas')],
      clients: [fakeClient(serviceKey: 'rdp')],
      serverServices: ['nas', 'rdp', 'cache'],
    )
    ..now = now
    ..relayConnections = [
      fakeRelayConnection(service: 'nas', connId: 41),
      fakeRelayConnection(
        service: 'cache',
        connId: 7,
        keyId: 3,
        healthy: false,
      ),
    ]
    ..serviceOwners = {'cache': 3}
    ..keys = [
      RelayKey(
        keyId: 3,
        state: RelayKeyState.active,
        issuedAt: now - 86400,
        expiresAt: now + 6 * 86400,
        label: 'laptop',
      ),
    ]
    ..sessions = const [
      DirectSession(
        id: 'a1b2c3',
        service: 'rdp',
        transport: 'TCP',
        path: 'direct',
        state: 'active',
        ageMs: 2000,
        consumerSeen: true,
        providerSeen: true,
        stale: false,
        conflicting: false,
      ),
    ];
}

Future<void> _open(WidgetTester tester, FakePbMapperApi api) =>
    pumpApp(tester, api, page: AppPage.relay);

Future<void> _section(WidgetTester tester, String name) async {
  await tester.tap(find.byKey(Key('relay-section-$name')));
  await settle(tester);
}

Future<void> _confirm(WidgetTester tester) async {
  await tester.tap(find.byKey(const Key('confirm-destructive')));
  await settle(tester);
}

void main() {
  testWidgets('an administrator sees every section, and who owns what', (
    tester,
  ) async {
    await _open(tester, _busy());
    expect(find.byKey(const Key('relay-role-admin')), findsOneWidget);
    for (final section in [
      'services',
      'connections',
      'credentials',
      'security',
      'local',
    ]) {
      expect(find.byKey(Key('relay-section-$section')), findsOneWidget);
    }
    expect(find.text('18 ms'), findsOneWidget);
    expect(find.textContaining('Shared from this computer'), findsOneWidget);
    expect(find.textContaining('credential #3 · laptop'), findsOneWidget);
    expect(find.text('Open at 127.0.0.1:9090'), findsOneWidget);
    expect(find.byKey(const Key('connect-cache')), findsOneWidget);
    expect(find.byKey(const Key('connect-nas')), findsNothing);

    // The connections behind a service unfold under it.
    await tester.tap(find.byKey(const Key('relay-service-open-cache')));
    await settle(tester);
    expect(find.byKey(const Key('relay-conn-cache-7')), findsOneWidget);
    await finish(tester);
  });

  testWidgets('a temporary credential sees its own, and is told why', (
    tester,
  ) async {
    final api = _busy()
      ..identity = const RelayIdentity(role: RelayRole.temporary, keyId: 5)
      ..conns = {
        'cache': [fakeConn(connId: 41), fakeConn(connId: 42, healthy: false)],
      };
    await _open(tester, api);
    expect(find.text('Temporary credential #5'), findsOneWidget);
    expect(find.byKey(const Key('relay-role-hint')), findsOneWidget);
    expect(find.byKey(const Key('relay-section-credentials')), findsNothing);
    expect(find.byKey(const Key('relay-section-security')), findsNothing);
    expect(find.byKey(const Key('relay-issue')), findsNothing);

    // What the relay holds for a key, asked of the relay when opened.
    expect(find.text('#41'), findsNothing);
    await tester.tap(find.byKey(const Key('conns-cache')));
    await settle(tester);
    expect(find.text('#41'), findsOneWidget);
    expect(find.text('Unhealthy'), findsOneWidget);
    await finish(tester);
  });

  testWidgets('a relay service connects with its key filled in', (
    tester,
  ) async {
    final api = FakePbMapperApi(serverServices: ['cache']);
    await _open(tester, api);
    await tester.tap(find.byKey(const Key('connect-cache')));
    await settle(tester);
    final field = tester.widget<TextField>(
      find.descendant(
        of: find.byKey(const Key('tunnel-key')),
        matching: find.byType(TextField),
      ),
    );
    expect(field.controller?.text, 'cache');
    await tester.tap(find.byKey(const Key('tunnel-submit')));
    await settle(tester);
    expect(api.calls, ['connectService(cache)']);
    await finish(tester);
  });

  testWidgets('a relay that does not answer says why', (tester) async {
    await _open(tester, FakePbMapperApi(relayUp: false));
    expect(
      find.text("The relay isn't answering: connection refused"),
      findsOneWidget,
    );
    await finish(tester);
  });

  group('credentials', () {
    testWidgets('issuing one shows it once, with its setup string', (
      tester,
    ) async {
      final api = _busy();
      await _open(tester, api);
      await _section(tester, 'credentials');
      expect(find.text('laptop'), findsOneWidget);

      await tester.tap(find.byKey(const Key('relay-issue')));
      await settle(tester);
      await tester.enterText(
        find.byKey(const Key('credential-label')),
        'phone',
      );
      await tester.tap(find.byKey(const Key('credential-ttl-720')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('credential-issue-submit')));
      await settle(tester);

      expect(api.calls, ['adminIssueKey(2592000, phone)']);
      expect(find.text('phone is ready'), findsOneWidget);
      expect(
        find.descendant(
          of: find.byKey(const Key('credential-secret')),
          matching: find.textContaining('pbmt1_1_'),
        ),
        findsOneWidget,
      );
      expect(find.byKey(const Key('credential-copy-setup')), findsOneWidget);
      await tester.tap(find.text('Close'));
      await settle(tester);
      expect(find.text('phone'), findsOneWidget);
      await finish(tester);
    });

    testWidgets('revoking asks first, then takes it off the active list', (
      tester,
    ) async {
      final api = _busy();
      await _open(tester, api);
      await _section(tester, 'credentials');
      await tester.tap(find.byKey(const Key('credential-menu-3')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('credential-revoke-3')));
      await settle(tester);
      expect(api.calls, isEmpty);
      await _confirm(tester);
      expect(api.calls, ['adminRevokeKey(3)']);
      expect(find.text('laptop'), findsNothing);

      await tester.tap(find.byKey(const Key('credential-filter-all')));
      await settle(tester);
      expect(find.text('laptop'), findsOneWidget);
      expect(find.text('Revoked'), findsOneWidget);
      await finish(tester);
    });

    testWidgets('renewing gives it the lifetime chosen', (tester) async {
      final api = _busy();
      await _open(tester, api);
      await _section(tester, 'credentials');
      await tester.tap(find.byKey(const Key('credential-menu-3')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('credential-renew-3')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('credential-renew-2160')));
      await settle(tester);
      expect(api.calls, ['adminRenewKey(3, 7776000)']);
      await finish(tester);
    });
  });

  group('connections', () {
    testWidgets('the relay\'s connections retire one at a time', (
      tester,
    ) async {
      final api = _busy();
      await _open(tester, api);
      await _section(tester, 'connections');
      expect(find.byKey(const Key('relay-conn-nas-41')), findsOneWidget);
      expect(find.byKey(const Key('relay-conn-cache-7')), findsOneWidget);
      await tester.tap(find.byKey(const Key('relay-retire-cache-7')));
      await settle(tester);
      await _confirm(tester);
      expect(api.calls, ['adminRetireConnections(cache, 7)']);
      expect(find.byKey(const Key('relay-conn-cache-7')), findsNothing);
      expect(find.text('Retired 1 connection'), findsOneWidget);
      await finish(tester);
    });

    testWidgets('direct sessions say which path they take', (tester) async {
      await _open(tester, _busy());
      await _section(tester, 'connections');
      expect(find.byKey(const Key('relay-session-a1b2c3')), findsOneWidget);
      expect(find.text('Direct'), findsOneWidget);
      await finish(tester);
    });
  });

  group('security', () {
    testWidgets('older clients are allowed or refused', (tester) async {
      final api = _busy();
      await _open(tester, api);
      await _section(tester, 'security');
      await tester.tap(find.byKey(const Key('security-legacy')));
      await settle(tester);
      expect(api.calls, ['adminSetLegacyProtocol(true)']);
      expect(api.legacyAllowed, isTrue);
      await finish(tester);
    });

    testWidgets('rotating the key switches this computer to it', (
      tester,
    ) async {
      final api = _busy();
      await _open(tester, api);
      await _section(tester, 'security');
      await tester.ensureVisible(find.byKey(const Key('security-rotate')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('security-rotate')));
      await settle(tester);
      expect(api.calls, isEmpty);
      await _confirm(tester);
      expect(api.calls, ['adminRotateKey']);
      expect(find.byKey(const Key('security-rotated')), findsOneWidget);
      expect(find.text(api.config.msgHeaderKey), findsOneWidget);
      await finish(tester);
    });

    testWidgets('resetting every credential takes a deliberate yes', (
      tester,
    ) async {
      final api = _busy();
      await _open(tester, api);
      await _section(tester, 'security');
      await tester.ensureVisible(find.byKey(const Key('security-reset')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('security-reset')));
      await settle(tester);
      final confirm = find.byKey(const Key('security-reset-confirm'));
      expect(tester.widget<FilledButton>(confirm).onPressed, isNull);
      await tester.tap(find.byKey(const Key('security-reset-understood')));
      await settle(tester);
      await tester.tap(confirm);
      await settle(tester);
      expect(api.calls, ['adminResetCredentials']);
      expect(api.keys, isEmpty);
      await finish(tester);
    });
  });

  group('this computer', () {
    testWidgets('runs a relay of its own', (tester) async {
      final api = FakePbMapperApi();
      await _open(tester, api);
      await _section(tester, 'local');
      expect(find.text('Stopped'), findsOneWidget);
      await tester.tap(find.byKey(const Key('local-toggle')));
      await settle(tester);
      expect(api.calls, ['startServer(7666)']);
      expect(find.text('Running'), findsOneWidget);
      expect(find.byKey(const Key('local-stats')), findsOneWidget);

      await tester.tap(find.byKey(const Key('local-toggle')));
      await settle(tester);
      expect(api.calls.last, 'stopServer');
      expect(find.text('Stopped'), findsOneWidget);
      await finish(tester);
    });

    testWidgets('its key shows on request, and can be used here', (
      tester,
    ) async {
      final api = FakePbMapperApi(
        config: const ConfigStatus(
          serverAddress: 'lb7666.top:7666',
          keepAliveEnabled: true,
          msgHeaderKey: '',
          isolatedRelayAdminKeySet: true,
          isolatedRelayAdminKey: '0123456789abcdefghijklmnopqrstuv',
        ),
      );
      await _open(tester, api);
      await _section(tester, 'local');
      await tester.ensureVisible(find.byKey(const Key('settings-reveal')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('settings-reveal')));
      await settle(tester);
      expect(find.text('0123456789abcdefghijklmnopqrstuv'), findsOneWidget);

      await tester.tap(find.byKey(const Key('local-use-here')));
      await settle(tester);
      expect(api.calls.last, 'updateConfig(127.0.0.1:7666)');
      expect(api.config.msgHeaderKey, '0123456789abcdefghijklmnopqrstuv');
      await finish(tester);
    });

    testWidgets('with no relay key, the key card stays away', (tester) async {
      await _open(tester, FakePbMapperApi());
      await _section(tester, 'local');
      expect(find.byKey(const Key('settings-reveal')), findsNothing);
      await finish(tester);
    });
  });
}
