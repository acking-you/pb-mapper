import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/common/tray/tray_service.dart';
import 'package:pb_mapper_ui/src/state/desktop_models.dart';

TrayStatus status(int connections, {DesktopHostView? host}) => TrayStatus(
  serverAvailable: true,
  activeConnections: connections,
  registeredServices: 0,
  connectedClients: 0,
  host: host,
);

TrayStrings strings([String language = 'en']) => TrayStrings(
  offline: '$language offline',
  connections: (count) => '$language connections $count',
  services: (count) => '$language services $count',
  onlineIdle: '$language idle',
  open: '$language open',
  refresh: '$language refresh',
  quit: '$language quit',
  hostSharing: '$language sharing',
  hostViewed: '$language viewed',
  hostControlled: '$language controlled',
  hostPaused: '$language paused',
  disconnectViewer: '$language disconnect viewer',
  stopSharing: '$language stop sharing',
);

void main() {
  final binding = TestWidgetsFlutterBinding.ensureInitialized();
  const channel = MethodChannel('tray_manager');
  final service = TrayService.instance;
  final calls = <MethodCall>[];
  Completer<void>? popup;
  Completer<void>? tooltip;
  Map<dynamic, dynamic>? menu;

  setUp(() {
    calls.clear();
    menu = null;
    popup = tooltip = null;
    binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      calls.add(call);
      if (call.method == 'setContextMenu') menu = call.arguments['menu'] as Map;
      if (call.method == 'setToolTip') await tooltip?.future;
      if (call.method == 'popUpContextMenu') {
        popup = Completer<void>();
        await popup!.future;
      }
      return true;
    });
  });
  tearDown(() {
    service.dispose();
    binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, null);
  });

  testWidgets('polling leaves an open menu and its click IDs stable', (
    tester,
  ) async {
    var current = status(0);
    var opened = 0;
    await service.initialize(
      statusProvider: () async => current,
      showApp: () => opened++,
      quitApp: () {},
      onDisconnectViewer: () {},
      onStopSharing: () {},
      strings: strings(),
    );
    await service.refreshStatus();
    calls.clear();
    await service.refreshStatus();
    await service.updateStrings(strings());
    await tester.pump(const Duration(seconds: 18));
    expect(calls, isEmpty);

    final originalOpen =
        (menu!['items'] as List).firstWhere(
              (item) => item['label'] == 'en open',
            )
            as Map;
    service.onTrayIconRightMouseDown();
    await tester.pump();
    service.onTrayIconRightMouseDown();
    current = status(1);
    await service.refreshStatus();
    current = status(3);
    await service.refreshStatus();
    await service.updateStrings(strings('zh'));
    await tester.pump(const Duration(seconds: 18));
    expect(calls.map((call) => call.method), ['popUpContextMenu']);

    popup!.complete();
    await tester.pump();
    expect(calls.map((call) => call.method), [
      'popUpContextMenu',
      'setIcon',
      'setToolTip',
      'setContextMenu',
    ]);
    expect((menu!['items'] as List).first['label'], 'zh connections 3');
    expect((menu!['items'] as List)[2]['label'], 'zh open');

    // WM_COMMAND may arrive after TrackPopupMenu returns and status/locale
    // changes have rendered. The old native ID must still resolve correctly.
    await binding.defaultBinaryMessenger.handlePlatformMessage(
      'tray_manager',
      const StandardMethodCodec().encodeMethodCall(
        MethodCall('onTrayMenuItemClick', {'id': originalOpen['id']}),
      ),
      (_) {},
    );
    expect(opened, 1);

    calls.clear();
    service.onTrayIconRightMouseDown();
    await tester.pump();
    expect(calls.single.method, 'popUpContextMenu');
    popup!.complete();
    await tester.pump();
    service.dispose();
  });

  testWidgets(
    'native updates serialize with opening and stale reads are ignored',
    (tester) async {
      Future<TrayStatus> Function() read = () async => status(1);
      await service.initialize(
        statusProvider: () => read(),
        showApp: () {},
        quitApp: () {},
        onDisconnectViewer: () {},
        onStopSharing: () {},
        strings: strings(),
      );
      await service.refreshStatus();
      calls.clear();
      tooltip = Completer<void>();
      read = () async => status(2);
      final updating = service.refreshStatus();
      await tester.pump();
      service.onTrayIconRightMouseDown();
      await tester.pump();
      expect(calls.map((call) => call.method), ['setToolTip']);
      tooltip!.complete();
      await updating;
      await tester.pump();
      expect(calls.map((call) => call.method), [
        'setToolTip',
        'setContextMenu',
        'popUpContextMenu',
      ]);
      popup!.complete();
      await tester.pump();

      final oldRead = Completer<TrayStatus>();
      read = () => oldRead.future;
      final oldRefresh = service.refreshStatus();
      read = () async => status(5);
      await service.refreshStatus();
      calls.clear();
      oldRead.complete(status(0));
      await oldRefresh;
      expect(calls, isEmpty);
      expect((menu!['items'] as List).first['label'], 'en connections 5');

      final afterDispose = Completer<TrayStatus>();
      read = () => afterDispose.future;
      final pending = service.refreshStatus();
      service.dispose();
      afterDispose.complete(status(0));
      await pending;
      expect(calls, isEmpty);
    },
  );

  testWidgets(
    'a viewed computer offers Disconnect viewer and Stop sharing with stable '
    'ids and shows the active icon',
    (tester) async {
      final watched = DesktopHostView(
        sharing: true,
        viewer: DesktopHostViewer(controlling: true, since: DateTime(2026)),
      );
      var current = status(0, host: watched);
      final actions = <String>[];
      await service.initialize(
        statusProvider: () async => current,
        showApp: () {},
        quitApp: () {},
        onDisconnectViewer: () => actions.add('disconnect'),
        onStopSharing: () => actions.add('stop'),
        strings: strings(),
      );
      await service.refreshStatus();
      List<Map> items() => [
        for (final item in menu!['items'] as List) item as Map,
      ];
      Map item(String label) => items().firstWhere((i) => i['label'] == label);
      expect([for (final i in items()) i['label']].take(4), [
        'en idle',
        'en controlled',
        'en disconnect viewer',
        'en stop sharing',
      ]);
      final stop = item('en stop sharing')['id'];
      expect(
        calls.lastWhere((c) => c.method == 'setIcon').arguments['iconPath'],
        contains('tray_active'),
      );
      expect(
        calls.lastWhere((c) => c.method == 'setToolTip').arguments['toolTip'],
        'en controlled · en idle',
      );

      // The viewer leaves: no Disconnect viewer, and Stop keeps its id.
      current = status(0, host: const DesktopHostView(sharing: true));
      await service.refreshStatus();
      expect(
        items().map((i) => i['label']),
        isNot(contains('en disconnect viewer')),
      );
      expect(item('en stop sharing')['id'], stop);
      await binding.defaultBinaryMessenger.handlePlatformMessage(
        'tray_manager',
        const StandardMethodCodec().encodeMethodCall(
          MethodCall('onTrayMenuItemClick', {'id': stop}),
        ),
        (_) {},
      );
      expect(actions, ['stop']);
      service.dispose();
    },
  );
}
