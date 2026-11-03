import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/common/setup_string.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/pages/overview_page.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

FakePbMapperApi _fresh() => FakePbMapperApi(
  config: const ConfigStatus(
    serverAddress: 'localhost:7666',
    keepAliveEnabled: true,
    msgHeaderKey: '',
  ),
);

void _clipboard(WidgetTester tester, Future<Object?> Function() read) {
  tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
    SystemChannels.platform,
    (call) async => call.method == 'Clipboard.getData' ? read() : null,
  );
  addTearDown(() {
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      null,
    );
  });
}

String _field(WidgetTester tester, String name) =>
    tester.widget<TextField>(find.byKey(Key('setup-$name'))).controller!.text;

void main() {
  testWidgets('imports all options and saves only when continuing', (
    tester,
  ) async {
    final api = _fresh();
    final text = encodeSetupString(
      const SetupPayload(
        address: 'relay.example:7666',
        credential: 'pbmt1_test_credential',
        keepAlive: false,
        relayTransport: RelayTransport.quic,
      ),
    );
    _clipboard(tester, () async => {'text': '  $text\n'});
    await pumpApp(tester, api, needsSetup: true);
    await tester.tap(find.byKey(const Key('setup-import')));
    await settle(tester);

    expect(_field(tester, 'address'), 'relay.example:7666');
    expect(_field(tester, 'credential'), 'pbmt1_test_credential');
    expect(find.textContaining('QUIC · keep-alive off'), findsOneWidget);
    expect(api.calls, isEmpty);

    await tester.tap(find.byKey(const Key('setup-continue')));
    await settle(tester);
    expect(api.calls, ['updateConfig(relay.example:7666, quic)']);
    expect(api.config.msgHeaderKey, 'pbmt1_test_credential');
    expect(api.config.keepAliveEnabled, isFalse);
    expect(api.config.relayTransport, RelayTransport.quic);
    expect(find.byKey(const Key('setup-choose-share')), findsOneWidget);
    await finish(tester);
  });

  testWidgets('imports older setup strings with their original defaults', (
    tester,
  ) async {
    final api = _fresh();
    final text = base64Encode(
      utf8.encode(
        jsonEncode({'serverAddress': '[::1]:7666', 'msgHeaderKey': 'k' * 32}),
      ),
    );
    _clipboard(tester, () async => {'text': text});
    await pumpApp(tester, api, needsSetup: true);
    await tester.tap(find.byKey(const Key('setup-import')));
    await settle(tester);
    await tester.tap(find.byKey(const Key('setup-continue')));
    await settle(tester);
    expect(api.config.serverAddress, '[::1]:7666');
    expect(api.config.msgHeaderKey, 'k' * 32);
    expect(api.config.keepAliveEnabled, isTrue);
    expect(api.config.relayTransport, RelayTransport.tcp);
    await finish(tester);
  });

  testWidgets('rejects malformed and invalid data without replacing edits', (
    tester,
  ) async {
    final api = _fresh();
    var text = 'not a setup string';
    _clipboard(tester, () async => {'text': text});
    await pumpApp(tester, api, needsSetup: true);
    await tester.enterText(find.byKey(const Key('setup-address')), 'my-edit');
    await tester.enterText(find.byKey(const Key('setup-credential')), 'my-key');
    await tester.tap(find.byKey(const Key('setup-import')));
    await settle(tester);
    expect(find.text("That isn't a valid setup string."), findsOneWidget);
    expect(find.textContaining(text), findsNothing);
    expect(_field(tester, 'address'), 'my-edit');
    expect(_field(tester, 'credential'), 'my-key');

    text = encodeSetupString(
      const SetupPayload(address: 'invalid', credential: 'pbmt1_test'),
    );
    await tester.tap(find.byKey(const Key('setup-import')));
    await settle(tester);
    expect(find.textContaining('host:port'), findsWidgets);
    expect(_field(tester, 'address'), 'my-edit');
    expect(_field(tester, 'credential'), 'my-key');

    text = encodeSetupString(
      const SetupPayload(address: 'example.com:7666', credential: 'bad'),
    );
    await tester.tap(find.byKey(const Key('setup-import')));
    await settle(tester);
    expect(_field(tester, 'address'), 'my-edit');
    expect(_field(tester, 'credential'), 'my-key');
    expect(api.calls, isEmpty);
    await finish(tester);
  });

  testWidgets('reports an empty or unavailable clipboard', (tester) async {
    var unavailable = false;
    _clipboard(tester, () async {
      if (unavailable) throw PlatformException(code: 'unavailable');
      return null;
    });
    await pumpApp(tester, _fresh(), needsSetup: true);
    await tester.tap(find.byKey(const Key('setup-import')));
    await settle(tester);
    expect(find.textContaining('clipboard is empty'), findsOneWidget);
    unavailable = true;
    await tester.tap(find.byKey(const Key('setup-import')));
    await settle(tester);
    expect(find.textContaining("Couldn't read the clipboard"), findsOneWidget);
    await finish(tester);
  });

  testWidgets('a late clipboard read cannot change a skipped setup', (
    tester,
  ) async {
    final read = Completer<Object?>();
    final api = _fresh();
    _clipboard(tester, () => read.future);
    await pumpApp(tester, api, needsSetup: true);
    await tester.tap(find.byKey(const Key('setup-import')));
    await tester.pump();
    await tester.tap(find.byKey(const Key('setup-skip')));
    await settle(tester);
    read.complete({'text': 'ignored'});
    await settle(tester);
    expect(find.byType(OverviewPage), findsOneWidget);
    expect(api.calls, isEmpty);
    expect(tester.takeException(), isNull);
    await finish(tester);
  });

  testWidgets('the import fits a small Chinese phone', (tester) async {
    final text = encodeSetupString(
      const SetupPayload(
        address: 'relay.example:7666',
        credential: 'pbmt1_test',
        keepAlive: false,
        relayTransport: RelayTransport.quic,
      ),
    );
    _clipboard(tester, () async => {'text': text});
    await pumpApp(
      tester,
      _fresh(),
      needsSetup: true,
      size: const Size(320, 640),
      locale: const Locale('zh'),
    );
    expect(find.text('从剪贴板导入配置'), findsOneWidget);
    await tester.tap(find.byKey(const Key('setup-import')));
    await settle(tester);
    expect(find.textContaining('QUIC · 保活关闭'), findsOneWidget);
    expect(tester.takeException(), isNull);
    await finish(tester);
  });
}
