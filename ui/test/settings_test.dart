import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

Future<void> _open(WidgetTester tester, FakePbMapperApi api) =>
    pumpApp(tester, api, page: AppPage.settings);

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
        }),
      ),
    );
    await tester.tap(find.byKey(const Key('settings-paste-setup')));
    await settle(tester);
    await tester.enterText(find.byKey(const Key('setup-string')), payload);
    await tester.tap(find.byKey(const Key('setup-string-apply')));
    await settle(tester);
    expect(api.calls, ['updateConfig(other.example:7666)']);
    expect(api.config.keepAliveEnabled, isFalse);
    await finish(tester);
  });

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
    await tester.tap(find.byKey(const Key('settings-theme-dark')));
    await settle(tester);
    final context = tester.element(find.byKey(const Key('settings-list')));
    expect(Theme.of(context).brightness, Brightness.dark);
    await finish(tester);
  });

  testWidgets('the language switch speaks Chinese at once', (tester) async {
    await _open(tester, FakePbMapperApi());
    await tester.tap(find.byKey(const Key('settings-language-zh')));
    await settle(tester);
    expect(find.text('中继服务器'), findsOneWidget);
    await finish(tester);
  });

  testWidgets('the embedded relay key shows only when there is one', (
    tester,
  ) async {
    await _open(tester, FakePbMapperApi());
    expect(find.byKey(const Key('settings-reveal')), findsNothing);
    await finish(tester);

    final api = FakePbMapperApi(
      config: const ConfigStatus(
        serverAddress: 'lb7666.top:7666',
        keepAliveEnabled: true,
        msgHeaderKey: '',
        isolatedRelayAdminKeySet: true,
        isolatedRelayAdminKey: 'k3y',
      ),
    );
    await _open(tester, api);
    // Built already (inside the list's cache extent), but below the fold.
    await tester.ensureVisible(find.byKey(const Key('settings-reveal')));
    await settle(tester);
    await tester.tap(find.byKey(const Key('settings-reveal')));
    await settle(tester);
    expect(find.text('k3y'), findsOneWidget);
    expect(api.calls, ['revealIsolatedRelayAdminKey']);
    await finish(tester);
  });
}
