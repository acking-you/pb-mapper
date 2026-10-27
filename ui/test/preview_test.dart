/// Renders the app with its real typefaces to PNGs, for looking at the design
/// without a device. Opt-in: runs only when `PB_MAPPER_UI_PREVIEW_DIR` names
/// a directory to write into.
///
/// ```sh
/// PB_MAPPER_UI_PREVIEW_DIR=/tmp/pbm flutter test test/preview_test.dart
/// ```
library;

import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

final String? _dir = Platform.environment['PB_MAPPER_UI_PREVIEW_DIR'];

Future<void> _capture(WidgetTester tester, String name) async {
  final boundary = tester.renderObject<RenderRepaintBoundary>(
    find.byKey(appRootKey),
  );
  await tester.runAsync(() async {
    final image = await boundary.toImage(pixelRatio: 1.5);
    final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
    File('$_dir/$name.png').writeAsBytesSync(bytes!.buffer.asUint8List());
  });
}

FakePbMapperApi _busy() => FakePbMapperApi(
  services: [
    fakeService(
      serviceKey: 'nas-files',
      localAddress: '127.0.0.1:5000',
      path: const TunnelPath(activeDirect: 2, directSessions: 5),
    ),
    fakeService(
      serviceKey: 'home-ssh',
      localAddress: '127.0.0.1:22',
      allowDirect: false,
    ),
    fakeService(
      serviceKey: 'grafana',
      localAddress: '127.0.0.1:3000',
      status: 'retrying',
      message: 'local service refused the connection',
    ),
  ],
  clients: [
    fakeClient(
      serviceKey: 'office-rdp',
      localAddress: '127.0.0.1:13389',
      path: const TunnelPath(
        activeDirect: 1,
        directSessions: 3,
        lastDirect: true,
        setupMs: 184,
      ),
    ),
    fakeClient(
      serviceKey: 'media-server',
      localAddress: '127.0.0.1:18096',
      path: const TunnelPath(
        activeRelay: 1,
        relaySessions: 2,
        lastDirect: false,
        fallbackStage: DirectStage.punching,
      ),
    ),
    fakeClient(
      serviceKey: 'lab-db',
      localAddress: '127.0.0.1:15432',
      status: 'stopped',
    ),
  ],
  serverServices: [
    'nas-files',
    'home-ssh',
    'grafana',
    'office-rdp',
    'lab-db',
    'build-cache',
    'media-server',
  ],
  conns: {
    'office-rdp': [fakeConn(connId: 41), fakeConn(connId: 42)],
  },
);

/// [api] reaching its relay over QUIC: the setting, every running tunnel,
/// and a measured connection.
FakePbMapperApi _overQuic(FakePbMapperApi api) => api
  ..config = ConfigStatus(
    serverAddress: api.config.serverAddress,
    keepAliveEnabled: true,
    msgHeaderKey: api.config.msgHeaderKey,
    relayTransport: RelayTransport.quic,
    isolatedRelayAdminKeySet: api.config.isolatedRelayAdminKeySet,
  )
  ..services = [
    for (final s in api.services)
      fakeService(
        serviceKey: s.serviceKey,
        localAddress: s.localAddress,
        status: s.status,
        message: s.statusMessage,
        allowDirect: s.allowDirect,
        path: s.path,
        relayTransport: RelayTransport.quic,
      ),
  ]
  ..clients = [
    for (final c in api.clients)
      fakeClient(
        serviceKey: c.serviceKey,
        localAddress: c.localAddress,
        status: c.status,
        allowDirect: c.allowDirect,
        path: c.path,
        relayTransport: RelayTransport.quic,
      ),
  ]
  ..link = const RelayLink(
    transport: RelayTransport.quic,
    connectionsCreated: 1,
    streamsOpened: 186,
    streamsReused: 185,
    connections: [
      QuicLinkConnection(
        id: 1,
        peer: '203.0.113.7:7667',
        pooled: true,
        rtt: Duration(microseconds: 23800),
        congestionWindow: 1887436,
        mtu: 1452,
        sentPackets: 48210,
        lostPackets: 17,
        congestionEvents: 2,
        sentBytes: 61865984,
        receivedBytes: 208666624,
      ),
    ],
  );

/// [_busy] as the relay's administrator sees it.
FakePbMapperApi _relayBusy({RelayRole role = RelayRole.admin}) {
  final now = DateTime.now().millisecondsSinceEpoch ~/ 1000;
  RelayKey key(int id, String label, {int ageDays = 1, int leftDays = 6}) =>
      RelayKey(
        keyId: id,
        state: RelayKeyState.active,
        issuedAt: now - ageDays * 86400,
        expiresAt: now + leftDays * 86400,
        label: label,
      );
  return _busy()
    ..identity = RelayIdentity(
      role: role,
      keyId: role == RelayRole.temporary ? 3 : null,
    )
    ..now = now
    ..keys = [
      key(3, 'laptop'),
      key(4, 'phone', ageDays: 12, leftDays: 18),
      key(5, 'office-pc', ageDays: 2, leftDays: 1),
      RelayKey(
        keyId: 2,
        state: RelayKeyState.revoked,
        issuedAt: now - 40 * 86400,
        expiresAt: now - 10 * 86400,
        label: 'old-tablet',
      ),
    ]
    ..serviceOwners = {'office-rdp': 3, 'lab-db': 4, 'build-cache': 4}
    ..relayConnections = [
      fakeRelayConnection(service: 'nas-files', connId: 12),
      fakeRelayConnection(service: 'home-ssh', connId: 13),
      fakeRelayConnection(service: 'office-rdp', connId: 41, keyId: 3),
      fakeRelayConnection(service: 'office-rdp', connId: 42, keyId: 3),
      fakeRelayConnection(
        service: 'build-cache',
        connId: 7,
        keyId: 4,
        healthy: false,
        lastRxAgeMs: 48000,
      ),
    ]
    ..sessions = const [
      DirectSession(
        id: 'a1b2c3',
        service: 'office-rdp',
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

Future<void> _relay(
  WidgetTester tester,
  String name, {
  String? section,
  ThemeMode theme = ThemeMode.light,
  Size size = const Size(1280, 820),
  Locale locale = const Locale('en'),
  RelayRole role = RelayRole.admin,
  bool quic = false,
}) async {
  final api = _relayBusy(role: role);
  await pumpApp(
    tester,
    quic ? _overQuic(api) : api,
    themeMode: theme,
    page: AppPage.relay,
    size: size,
    locale: locale,
  );
  if (section != null) {
    final tab = find.byKey(Key('relay-section-$section'));
    await tester.ensureVisible(tab);
    await tester.tap(tab);
    await settle(tester);
  }
  await _capture(tester, name);
  await finish(tester);
}

void main() {
  if (_dir == null) {
    test('previews are opt-in', () {}, skip: 'set PB_MAPPER_UI_PREVIEW_DIR');
    return;
  }

  setUpAll(() async {
    Directory(_dir!).createSync(recursive: true);
    await loadRealFonts();
  });

  for (final (name, theme) in [
    ('light', ThemeMode.light),
    ('dark', ThemeMode.dark),
  ]) {
    testWidgets('overview $name', (tester) async {
      await pumpApp(tester, _busy(), themeMode: theme);
      await _capture(tester, 'overview_$name');
      await finish(tester);
    });

    testWidgets('tunnels $name', (tester) async {
      await pumpApp(tester, _busy(), themeMode: theme, page: AppPage.tunnels);
      await _capture(tester, 'tunnels_$name');
      await finish(tester);
    });

    testWidgets('settings $name', (tester) async {
      await pumpApp(tester, _busy(), themeMode: theme, page: AppPage.settings);
      await _capture(tester, 'settings_$name');
      await finish(tester);
    });

    testWidgets('editor $name', (tester) async {
      await pumpApp(tester, _busy(), themeMode: theme, page: AppPage.tunnels);
      await tester.tap(find.byKey(const Key('new-tunnel')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('tunnel-kind-connect')));
      await settle(tester);
      await _capture(tester, 'editor_$name');
      await finish(tester);
    });

    testWidgets('setup $name', (tester) async {
      await pumpApp(
        tester,
        FakePbMapperApi(
          config: const ConfigStatus(
            serverAddress: 'localhost:7666',
            keepAliveEnabled: true,
            msgHeaderKey: '',
          ),
        ),
        themeMode: theme,
        needsSetup: true,
      );
      await _capture(tester, 'setup_$name');
      await finish(tester);
    });
  }

  testWidgets('overview selected', (tester) async {
    await pumpApp(tester, _busy(), themeMode: ThemeMode.dark);
    await tester.tap(find.byKey(const Key('galaxy-canvas')));
    await tester.sendKeyEvent(LogicalKeyboardKey.arrowRight);
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await settle(tester);
    await _capture(tester, 'overview_selected_dark');
    await finish(tester);
  });

  for (final key in ['office-rdp', 'media-server']) {
    testWidgets('overview path $key', (tester) async {
      await pumpApp(tester, _busy(), themeMode: ThemeMode.dark);
      await tester.tap(find.byKey(const Key('galaxy-canvas')));
      await tester.sendKeyEvent(LogicalKeyboardKey.arrowRight);
      await tester.sendKeyEvent(LogicalKeyboardKey.enter);
      await settle(tester);
      final row = find.byKey(Key('galaxy-row-connect:$key'));
      if (row.evaluate().isNotEmpty) {
        await tester.tap(row);
        await settle(tester);
      }
      await _capture(tester, 'overview_path_$key');
      await finish(tester);
    });
  }

  testWidgets('empty overview', (tester) async {
    await pumpApp(
      tester,
      FakePbMapperApi(serverServices: ['build-cache']),
      themeMode: ThemeMode.light,
    );
    await _capture(tester, 'overview_empty_light');
    await finish(tester);
  });

  testWidgets('phone overview', (tester) async {
    await pumpApp(tester, _busy(), size: const Size(393, 852));
    await _capture(tester, 'phone_overview');
    await finish(tester);
  });

  testWidgets('relay services light', (tester) async {
    await _relay(tester, 'relay_services_light');
  });

  testWidgets('relay services dark', (tester) async {
    await _relay(tester, 'relay_services_dark', theme: ThemeMode.dark);
  });

  testWidgets('relay connections', (tester) async {
    await _relay(tester, 'relay_connections_light', section: 'connections');
  });

  testWidgets('relay credentials', (tester) async {
    await _relay(
      tester,
      'relay_credentials_dark',
      section: 'credentials',
      theme: ThemeMode.dark,
    );
  });

  testWidgets('relay security', (tester) async {
    await _relay(tester, 'relay_security_light', section: 'security');
  });

  testWidgets('relay this computer', (tester) async {
    await _relay(tester, 'relay_local_light', section: 'local');
  });

  testWidgets('relay over quic light', (tester) async {
    await _relay(tester, 'relay_quic_light', quic: true);
  });

  testWidgets('relay over quic dark', (tester) async {
    await _relay(tester, 'relay_quic_dark', quic: true, theme: ThemeMode.dark);
  });

  testWidgets('relay offering quic to tcp', (tester) async {
    await _relay(tester, 'relay_offers_quic_light');
  });

  testWidgets('phone relay quic zh', (tester) async {
    await _relay(
      tester,
      'phone_relay_quic_zh',
      quic: true,
      size: const Size(393, 852),
      locale: const Locale('zh'),
    );
  });

  testWidgets('settings quic', (tester) async {
    await pumpApp(tester, _overQuic(_busy()), page: AppPage.settings);
    await _capture(tester, 'settings_quic_light');
    await finish(tester);
  });

  testWidgets('settings quic on a relay without it', (tester) async {
    await pumpApp(
      tester,
      _overQuic(_busy())..relayQuicPort = null,
      page: AppPage.settings,
      themeMode: ThemeMode.dark,
    );
    await _capture(tester, 'settings_no_quic_dark');
    await finish(tester);
  });

  testWidgets('tunnels over quic', (tester) async {
    await pumpApp(tester, _overQuic(_busy()), page: AppPage.tunnels);
    await _capture(tester, 'tunnels_quic_light');
    await finish(tester);
  });

  testWidgets('relay this computer with quic', (tester) async {
    final api = _relayBusy()
      ..localRelayPort = 7666
      ..localRelayQuicPort = 7667;
    await pumpApp(tester, api, page: AppPage.relay);
    final tab = find.byKey(const Key('relay-section-local'));
    await tester.ensureVisible(tab);
    await tester.tap(tab);
    await settle(tester);
    await _capture(tester, 'relay_local_quic_light');
    await finish(tester);
  });

  testWidgets('carrier move dialog', (tester) async {
    final api = _relayBusy()
      ..clients = [
        for (final c in _busy().clients)
          fakeClient(
            serviceKey: c.serviceKey,
            localAddress: c.localAddress,
            status: c.status,
            relayTransport: RelayTransport.tcp,
          ),
      ];
    await pumpApp(tester, api, page: AppPage.relay);
    await tester.tap(find.byKey(const Key('relay-use-quic')));
    await settle(tester);
    await _capture(tester, 'carrier_move_light');
    await finish(tester);
  });

  testWidgets('relay as a temporary credential', (tester) async {
    await _relay(tester, 'relay_temporary_light', role: RelayRole.temporary);
  });

  testWidgets('relay issue dialog', (tester) async {
    await pumpApp(tester, _relayBusy(), page: AppPage.relay);
    await tester.tap(find.byKey(const Key('relay-issue')));
    await settle(tester);
    await _capture(tester, 'relay_issue_light');
    await finish(tester);
  });

  testWidgets('phone relay zh', (tester) async {
    await _relay(
      tester,
      'phone_relay_zh',
      size: const Size(393, 852),
      locale: const Locale('zh'),
    );
  });

  testWidgets('phone relay credentials', (tester) async {
    await _relay(
      tester,
      'phone_relay_credentials',
      section: 'credentials',
      size: const Size(393, 852),
    );
  });

  testWidgets('phone tunnels zh', (tester) async {
    await pumpApp(
      tester,
      _busy(),
      size: const Size(393, 852),
      page: AppPage.tunnels,
      locale: const Locale('zh'),
    );
    await _capture(tester, 'phone_tunnels_zh');
    await finish(tester);
  });
}
