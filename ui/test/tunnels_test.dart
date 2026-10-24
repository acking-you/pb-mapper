import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

Future<void> _open(WidgetTester tester, FakePbMapperApi api) =>
    pumpApp(tester, api, page: AppPage.tunnels);

void main() {
  testWidgets('lists shares, connections and the relay\'s other services', (
    tester,
  ) async {
    await _open(
      tester,
      FakePbMapperApi(
        services: [
          fakeService(serviceKey: 'nas', localAddress: '127.0.0.1:5000'),
        ],
        clients: [fakeClient(serviceKey: 'rdp')],
        serverServices: ['nas', 'rdp', 'cache'],
      ),
    );
    expect(find.byKey(const Key('tunnel-share:nas')), findsOneWidget);
    expect(find.byKey(const Key('tunnel-connect:rdp')), findsOneWidget);
    expect(find.text('127.0.0.1:5000  ·  TCP  ·  Encrypted'), findsOneWidget);
    // A key this computer shares is not offered back to it.
    expect(find.byKey(const Key('remote-nas')), findsNothing);
    expect(find.byKey(const Key('remote-cache')), findsOneWidget);
    await finish(tester);
  });

  testWidgets('empty sections say what to do', (tester) async {
    await _open(tester, FakePbMapperApi());
    expect(find.textContaining('Nothing shared yet'), findsOneWidget);
    expect(find.textContaining('No connections yet'), findsOneWidget);
    expect(find.text('The relay lists no other services.'), findsOneWidget);
    await finish(tester);
  });

  testWidgets('a relay that does not answer is said in words', (tester) async {
    await _open(tester, FakePbMapperApi(relayUp: false));
    expect(find.textContaining("isn't answering"), findsOneWidget);
    await finish(tester);
  });

  testWidgets('a failing share can still be stopped', (tester) async {
    final api = FakePbMapperApi(
      services: [fakeService(serviceKey: 'web', status: 'failed')],
    );
    await _open(tester, api);
    expect(find.text('Stop'), findsOneWidget);
    await tester.tap(find.byKey(const Key('toggle-share:web')));
    await settle(tester);
    expect(api.calls, ['unregisterService(web)']);
    expect(find.text('Start'), findsOneWidget);
    await finish(tester);
  });

  testWidgets('a failing connection can still be stopped', (tester) async {
    final api = FakePbMapperApi(
      clients: [fakeClient(serviceKey: 'db', status: 'failed')],
    );
    await _open(tester, api);
    await tester.tap(find.byKey(const Key('toggle-connect:db')));
    await settle(tester);
    expect(api.calls, ['disconnectService(db)']);
    await finish(tester);
  });

  testWidgets('a stopped share starts with what it was saved with', (
    tester,
  ) async {
    final api = FakePbMapperApi(
      services: [fakeService(serviceKey: 'web', status: 'stopped')],
    );
    await _open(tester, api);
    await tester.tap(find.byKey(const Key('toggle-share:web')));
    await settle(tester);
    expect(api.calls, ['registerService(web)']);
    await finish(tester);
  });

  testWidgets('deleting asks first', (tester) async {
    final api = FakePbMapperApi(services: [fakeService(serviceKey: 'web')]);
    await _open(tester, api);
    await tester.tap(find.byKey(const Key('menu-share:web')));
    await settle(tester);
    await tester.tap(find.byKey(const Key('menu-delete')));
    await settle(tester);
    expect(api.calls, isEmpty);

    await tester.tap(find.byKey(const Key('confirm-destructive')));
    await settle(tester);
    expect(api.calls, ['deleteServiceConfig(web)']);
    expect(find.byKey(const Key('tunnel-share:web')), findsNothing);
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
    expect(find.byKey(const Key('tunnel-connect:cache')), findsOneWidget);
    await finish(tester);
  });

  testWidgets('what the relay holds for a key opens on request', (
    tester,
  ) async {
    await _open(
      tester,
      FakePbMapperApi(
        serverServices: ['cache'],
        conns: {
          'cache': [fakeConn(connId: 41), fakeConn(connId: 42, healthy: false)],
        },
      ),
    );
    expect(find.text('#41'), findsNothing);
    await tester.tap(find.byKey(const Key('conns-cache')));
    await settle(tester);
    expect(find.text('#41'), findsOneWidget);
    expect(find.text('#42'), findsOneWidget);
    expect(find.text('Unhealthy'), findsOneWidget);
    await finish(tester);
  });
}
