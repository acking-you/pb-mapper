import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart' show PortMappingMode;
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

Future<void> _openEditor(WidgetTester tester, FakePbMapperApi api) async {
  await pumpApp(tester, api, page: AppPage.tunnels);
  await tester.tap(find.byKey(const Key('new-tunnel')));
  await settle(tester);
}

TextEditingController _controller(WidgetTester tester, String key) => tester
    .widget<TextField>(
      find.descendant(
        of: find.byKey(Key(key)),
        matching: find.byType(TextField),
      ),
    )
    .controller!;

void main() {
  testWidgets(
    'port mapping defaults to when needed, and a choice survives relay-only and editing',
    (tester) async {
      final api = FakePbMapperApi();
      await _openEditor(tester, api);
      await tester.enterText(find.byKey(const Key('tunnel-key')), 'mapped');
      await tester.tap(find.byKey(const Key('tunnel-more')));
      await settle(tester);
      final control = find.byKey(const Key('tunnel-port-mapping'));
      PortMappingMode mode() =>
          tester.widget<GlassSegmented<PortMappingMode>>(control).selected;
      bool held() => tester
          .widget<IgnorePointer>(
            find.ancestor(of: control, matching: find.byType(IgnorePointer)).first,
          )
          .ignoring;
      expect(mode(), PortMappingMode.auto);
      await tester.ensureVisible(control);
      await tester.tap(
        find.descendant(of: control, matching: find.text('Always')),
      );
      await tester.tap(find.byKey(const Key('tunnel-direct')));
      await settle(tester);
      // Relay-only holds the choice rather than dropping it.
      expect((mode(), held()), (PortMappingMode.always, true));
      await tester.ensureVisible(find.byKey(const Key('tunnel-submit')));
      await tester.tap(find.byKey(const Key('tunnel-submit')));
      await settle(tester);
      expect(api.services.single.portMapping, PortMappingMode.always);
      expect(api.services.single.allowDirect, isFalse);
      await tester.tap(find.byKey(const Key('menu-share:mapped')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('menu-edit')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('tunnel-submit')));
      await settle(tester);
      expect(api.services.single.portMapping, PortMappingMode.always);
      await finish(tester);
    },
  );

  testWidgets('an empty key is refused before anything reaches Rust', (
    tester,
  ) async {
    final api = FakePbMapperApi();
    await _openEditor(tester, api);
    await tester.tap(find.byKey(const Key('tunnel-submit')));
    await settle(tester);
    expect(find.text('Enter a service key.'), findsOneWidget);
    expect(api.calls, isEmpty);
    await finish(tester);
  });

  testWidgets('an address without a port is refused', (tester) async {
    final api = FakePbMapperApi();
    await _openEditor(tester, api);
    await tester.enterText(find.byKey(const Key('tunnel-key')), 'web');
    await tester.enterText(
      find.byKey(const Key('tunnel-address')),
      'localhost',
    );
    await tester.tap(find.byKey(const Key('tunnel-submit')));
    await settle(tester);
    expect(find.textContaining('host:port'), findsOneWidget);
    expect(api.calls, isEmpty);
    await finish(tester);
  });

  testWidgets('a key this computer already shares is refused', (tester) async {
    final api = FakePbMapperApi(services: [fakeService(serviceKey: 'web')]);
    await _openEditor(tester, api);
    await tester.enterText(find.byKey(const Key('tunnel-key')), 'web');
    await tester.tap(find.byKey(const Key('tunnel-submit')));
    await settle(tester);
    expect(find.textContaining('already shares'), findsOneWidget);
    expect(api.calls, isEmpty);
    await finish(tester);
  });

  testWidgets('switching to connect moves an untouched default address', (
    tester,
  ) async {
    await _openEditor(tester, FakePbMapperApi());
    expect(_controller(tester, 'tunnel-address').text, '127.0.0.1:8080');
    await tester.tap(find.byKey(const Key('tunnel-kind-connect')));
    await settle(tester);
    expect(_controller(tester, 'tunnel-address').text, '127.0.0.1:9090');

    // A typed address is the user's, and stays.
    await tester.enterText(
      find.byKey(const Key('tunnel-address')),
      '127.0.0.1:7000',
    );
    await tester.tap(find.byKey(const Key('tunnel-kind-share')));
    await settle(tester);
    expect(_controller(tester, 'tunnel-address').text, '127.0.0.1:7000');
    await finish(tester);
  });

  testWidgets('a connection is offered the relay\'s keys', (tester) async {
    final api = FakePbMapperApi(serverServices: ['cache', 'media']);
    await _openEditor(tester, api);
    expect(find.byKey(const Key('suggestion-cache')), findsNothing);
    await tester.tap(find.byKey(const Key('tunnel-kind-connect')));
    await settle(tester);

    await tester.tap(find.byKey(const Key('suggestion-media')));
    await settle(tester);
    expect(_controller(tester, 'tunnel-key').text, 'media');
    await tester.tap(find.byKey(const Key('tunnel-submit')));
    await settle(tester);
    expect(api.calls, ['connectService(media)']);
    await finish(tester);
  });

  testWidgets('a refusal is shown in the form, which stays open', (
    tester,
  ) async {
    final api = FakePbMapperApi()..failNext = 'Failed to bind 127.0.0.1:8080';
    await _openEditor(tester, api);
    await tester.enterText(find.byKey(const Key('tunnel-key')), 'web');
    await tester.tap(find.byKey(const Key('tunnel-submit')));
    await settle(tester);
    expect(find.byKey(const Key('tunnel-error')), findsOneWidget);
    expect(find.text('Failed to bind 127.0.0.1:8080'), findsOneWidget);
    expect(find.byKey(const Key('tunnel-submit')), findsOneWidget);
    await finish(tester);
  });

  testWidgets('the advanced switches wait behind More options', (tester) async {
    final api = FakePbMapperApi();
    await _openEditor(tester, api);
    expect(find.byKey(const Key('tunnel-encryption')), findsNothing);
    await tester.tap(find.byKey(const Key('tunnel-more')));
    await settle(tester);
    expect(find.byKey(const Key('tunnel-encryption')), findsOneWidget);
    expect(find.byKey(const Key('tunnel-keepalive')), findsOneWidget);

    // Encryption is the share's choice; a connection follows it.
    await tester.tap(find.byKey(const Key('tunnel-kind-connect')));
    await settle(tester);
    expect(find.byKey(const Key('tunnel-encryption')), findsNothing);
    await finish(tester);
  });

  testWidgets('a connection kept on the relay stays there through an edit', (
    tester,
  ) async {
    final api = FakePbMapperApi();
    await _openEditor(tester, api);
    await tester.tap(find.byKey(const Key('tunnel-kind-connect')));
    await settle(tester);
    await tester.enterText(find.byKey(const Key('tunnel-key')), 'db');
    await tester.tap(find.byKey(const Key('tunnel-more')));
    await settle(tester);
    await tester.tap(find.byKey(const Key('tunnel-direct')));
    await tester.tap(find.byKey(const Key('tunnel-submit')));
    await settle(tester);
    expect(api.calls, ['connectService(db, relay only)']);
    // The list says which way it goes.
    expect(find.byKey(const Key('path-connect:db')), findsOneWidget);

    api.calls.clear();
    await tester.tap(find.byKey(const Key('menu-connect:db')));
    await settle(tester);
    await tester.tap(find.byKey(const Key('menu-edit')));
    await settle(tester);
    await tester.tap(find.byKey(const Key('tunnel-submit')));
    await settle(tester);
    expect(api.calls.last, 'connectService(db, relay only)');
    await finish(tester);
  });

  testWidgets('editing keeps the kind and starts from what was saved', (
    tester,
  ) async {
    final api = FakePbMapperApi(
      services: [
        fakeService(serviceKey: 'web', localAddress: '127.0.0.1:3000'),
      ],
    );
    await pumpApp(tester, api, page: AppPage.tunnels);
    await tester.tap(find.byKey(const Key('menu-share:web')));
    await settle(tester);
    await tester.tap(find.byKey(const Key('menu-edit')));
    await settle(tester);

    expect(find.byKey(const Key('tunnel-kind')), findsNothing);
    expect(_controller(tester, 'tunnel-address').text, '127.0.0.1:3000');
    await tester.enterText(
      find.byKey(const Key('tunnel-address')),
      '127.0.0.1:3001',
    );
    await tester.tap(find.byKey(const Key('tunnel-submit')));
    await settle(tester);
    expect(api.calls, ['registerService(web)']);
    expect(api.services.single.localAddress, '127.0.0.1:3001');
    await finish(tester);
  });
}
