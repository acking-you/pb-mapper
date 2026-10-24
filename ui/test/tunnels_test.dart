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
    // What else the relay offers is the Relay page's; this counts it (a key
    // this computer shares is not offered back to it) and leads there.
    expect(
      find.text('2 services on the relay you can connect to.'),
      findsOneWidget,
    );
    await tester.tap(find.byKey(const Key('tunnels-open-relay')));
    await settle(tester);
    expect(find.byKey(const Key('relay-list')), findsOneWidget);
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
}
