import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/common/state_change.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

int get _now => DateTime.now().millisecondsSinceEpoch ~/ 1000;

/// A relay with something of everything: a service this computer shares, one
/// it connects to, a `cache` of the administrator's and another `cache` that
/// credential #3 registered, connections, that credential and a direct
/// session.
FakePbMapperApi _busy() {
  final now = _now;
  return FakePbMapperApi(
      services: [fakeService(serviceKey: 'nas')],
      clients: [fakeClient(serviceKey: 'rdp')],
      serverServices: ['nas', 'rdp', 'cache'],
    )
    ..now = now
    ..namespacedServices = [(3, 'cache')]
    ..relayConnections = [
      fakeRelayConnection(service: 'nas', connId: 41),
      fakeRelayConnection(
        service: 'cache',
        connId: 7,
        keyId: 3,
        healthy: false,
      ),
    ]
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

/// The two stores over [api], without the page.
Future<(AppStore, RelayStore)> _stores(FakePbMapperApi api) async {
  final app = AppStore(
    api: api,
    changes: StreamController<StateChange>.broadcast().stream,
  );
  addTearDown(app.dispose);
  await app.start();
  final relay = RelayStore(app: app);
  addTearDown(relay.dispose);
  return (app, relay);
}

Future<void> _section(WidgetTester tester, String name) async {
  await tester.tap(find.byKey(Key('relay-section-$name')));
  await settle(tester);
}

Future<void> _confirm(WidgetTester tester) async {
  await tester.tap(find.byKey(const Key('confirm-destructive')));
  await settle(tester);
}

void main() {
  testWidgets('an administrator sees every section, and every namespace', (
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
    // Two services named `cache`: the administrator's own can be connected
    // to from here; credential #3's is for that credential's computers.
    expect(find.byKey(const Key('relay-service-cache')), findsOneWidget);
    expect(find.byKey(const Key('relay-service-3/cache')), findsOneWidget);
    expect(find.byKey(const Key('connect-cache')), findsOneWidget);
    expect(find.byKey(const Key('connect-3/cache')), findsNothing);
    expect(find.byKey(const Key('relay-namespace-3/cache')), findsOneWidget);
    expect(find.byKey(const Key('connect-nas')), findsNothing);

    // The connections behind a service unfold under it, and only its own.
    expect(find.byKey(const Key('relay-service-open-cache')), findsNothing);
    await tester.tap(find.byKey(const Key('relay-service-open-3/cache')));
    await settle(tester);
    expect(find.byKey(const Key('relay-conn-3/cache-7')), findsOneWidget);
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
    expect(find.byKey(const Key('relay-role-temporary')), findsOneWidget);
    expect(find.byKey(const Key('relay-role-hint')), findsOneWidget);
    expect(find.byKey(const Key('relay-section-credentials')), findsNothing);
    expect(find.byKey(const Key('relay-section-security')), findsNothing);
    expect(find.byKey(const Key('relay-issue')), findsNothing);

    // What the relay holds for a key, asked of the relay when opened.
    expect(find.text('#41'), findsNothing);
    await tester.tap(find.byKey(const Key('conns-cache')));
    await settle(tester);
    expect(find.text('#41'), findsOneWidget);
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
    expect(find.byKey(const Key('relay-error')), findsOneWidget);
    expect(find.textContaining('connection refused'), findsOneWidget);
    await finish(tester);
  });

  test('one listing failing leaves the others, and says why', () async {
    final (_, relay) = await _stores(_busy()..connectionsFail = true);
    await relay.refresh();
    expect(relay.keys, isNotEmpty);
    expect(relay.authStatus, isNotNull);
    expect(relay.serviceRows, hasLength(4));
    expect(relay.adminError, contains('connections'));
  });

  test('switching relays forgets the old one, even mid-answer', () async {
    final api = _busy();
    final (app, relay) = await _stores(api);
    await relay.refresh();
    expect(relay.keys, isNotEmpty);

    // A read of the old relay is still out when the address changes.
    api.adminGate = Completer<void>();
    final stale = relay.refresh();
    await pumpEventQueue();
    await app.saveRelay(
      address: 'other.example:7666',
      credential: 'pbmt1_${'t' * 40}',
      keepAlive: true,
    );
    expect(relay.keys, isEmpty);
    expect(relay.identityLoaded, isFalse);

    api.identity = const RelayIdentity(role: RelayRole.temporary, keyId: 9);
    api.adminGate!.complete();
    await stale;
    expect(relay.keys, isEmpty, reason: 'the old relay answered too late');
    await relay.refresh();
    expect(relay.identity.role, RelayRole.temporary);
  });

  group('credentials', () {
    testWidgets('issuing one shows it once; a refusal leaves the form', (
      tester,
    ) async {
      final api = _busy()..maxTtl = const Duration(days: 7);
      await _open(tester, api);
      await _section(tester, 'credentials');

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
      // Longer than this relay allows: the reason, and the form to fix it.
      expect(find.byKey(const Key('credential-issue-error')), findsOneWidget);

      await tester.tap(find.byKey(const Key('credential-ttl-168')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('credential-issue-submit')));
      await settle(tester);
      expect(api.calls, [
        'adminIssueKey(2592000, phone)',
        'adminIssueKey(604800, phone)',
      ]);
      expect(
        find.descendant(
          of: find.byKey(const Key('credential-secret')),
          matching: find.textContaining('pbmt1_1_'),
        ),
        findsOneWidget,
      );
      expect(find.byKey(const Key('credential-copy-setup')), findsOneWidget);
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
      expect(find.byKey(const Key('credential-3')), findsNothing);

      await tester.tap(find.byKey(const Key('credential-filter-all')));
      await settle(tester);
      expect(find.byKey(const Key('credential-3')), findsOneWidget);
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
      await tester.tap(find.byKey(const Key('credential-renew-720')));
      await settle(tester);
      expect(api.calls, ['adminRenewKey(3, 2592000)']);
      await finish(tester);
    });
  });

  group('connections', () {
    testWidgets('a connection retires in its own namespace', (tester) async {
      final api = _busy();
      await _open(tester, api);
      await _section(tester, 'connections');
      expect(find.byKey(const Key('relay-conn-nas-41')), findsOneWidget);
      await tester.tap(find.byKey(const Key('relay-retire-3/cache-7')));
      await settle(tester);
      await _confirm(tester);
      expect(api.calls, ['adminRetireConnections(3, cache, 7)']);
      expect(find.byKey(const Key('relay-conn-3/cache-7')), findsNothing);
      await finish(tester);
    });

    testWidgets('direct sessions are listed', (tester) async {
      await _open(tester, _busy());
      await _section(tester, 'connections');
      expect(find.byKey(const Key('relay-session-a1b2c3')), findsOneWidget);
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

    testWidgets('a rotation switches to the new key only once confirmed', (
      tester,
    ) async {
      final api = _busy()..rotationAnswers = false;
      final old = api.config.msgHeaderKey;
      await _open(tester, api);
      await _section(tester, 'security');
      Future<void> rotate() async {
        await tester.ensureVisible(find.byKey(const Key('security-rotate')));
        await settle(tester);
        await tester.tap(find.byKey(const Key('security-rotate')));
        await settle(tester);
        await _confirm(tester);
      }

      // The answer was lost: the new key is shown to keep, the old one kept.
      await rotate();
      expect(
        find.byKey(const Key('security-rotation-unknown')),
        findsOneWidget,
      );
      expect(find.text('R0tatedAdminKey-0123456789abcdef'), findsOneWidget);
      expect(api.config.msgHeaderKey, old);
      await tester.tap(find.text('Close'));
      await settle(tester);

      // Closing it, or leaving the section, does not lose it; saying so does.
      await _section(tester, 'services');
      expect(find.byKey(const Key('relay-unsaved-key')), findsOneWidget);
      await tester.tap(find.byKey(const Key('relay-unsaved-key-show')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('security-rotation-kept')));
      await settle(tester);
      expect(find.byKey(const Key('relay-unsaved-key')), findsNothing);
      await _section(tester, 'security');

      api.rotationAnswers = true;
      await rotate();
      expect(find.byKey(const Key('security-rotated')), findsOneWidget);
      expect(api.config.msgHeaderKey, 'R0tatedAdminKey-0123456789abcdef');
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
    testWidgets('runs a relay of its own, and uses it on its own port', (
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
      await tester.enterText(find.byKey(const Key('local-port')), '17000');
      await tester.tap(find.byKey(const Key('local-toggle')));
      await settle(tester);
      expect(api.calls, ['startServer(17000)']);
      expect(find.byKey(const Key('local-stats')), findsOneWidget);

      // Leaving the section and coming back keeps the port it runs on.
      await _section(tester, 'services');
      await _section(tester, 'local');
      await tester.ensureVisible(find.byKey(const Key('local-use-here')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('local-use-here')));
      await settle(tester);
      expect(api.calls.last, 'updateConfig(127.0.0.1:17000)');
      expect(api.config.msgHeaderKey, '0123456789abcdefghijklmnopqrstuv');

      await tester.ensureVisible(find.byKey(const Key('local-toggle')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('local-toggle')));
      await settle(tester);
      expect(api.calls.last, 'stopServer');
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
