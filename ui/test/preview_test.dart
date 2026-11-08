/// Renders the app with its real typefaces to PNGs, for looking at the design
/// without a device. Opt-in: runs only when `PB_MAPPER_UI_PREVIEW_DIR` names
/// a directory to write into.
///
/// ```sh
/// PB_MAPPER_UI_PREVIEW_DIR=/tmp/pbm flutter test test/preview_test.dart
/// ```
///
/// The remote desktop previews show [RemoteScreenFixture] as the remote
/// computer's picture, delivered through the fake JPEG path.
library;

import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/pages/remote/invite_scanner.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_page.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';
import 'package:pb_mapper_ui/src/state/remote_desktop.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';

import 'fake_desktop_api.dart';
import 'fake_pb_mapper_api.dart';
import 'fake_remote_computer_vault.dart';
import 'remote_screen_fixture.dart';
import 'support.dart';

final String? _dir = Platform.environment['PB_MAPPER_UI_PREVIEW_DIR'];

Future<void> _capture(
  WidgetTester tester,
  String name, {
  double pixelRatio = 1.5,
}) async {
  final boundary = tester.renderObject<RenderRepaintBoundary>(
    find.byKey(appRootKey),
  );
  await tester.runAsync(() async {
    final image = await boundary.toImage(pixelRatio: pixelRatio);
    final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
    File('$_dir/$name.png').writeAsBytesSync(bytes!.buffer.asUint8List());
  });
}

/// A preview, drawn with real shadows: `flutter test` otherwise paints an
/// elevation's shadow as a solid block.
void _preview(String description, WidgetTesterCallback body) =>
    testWidgets(description, (tester) async {
      debugDisableShadows = false;
      try {
        await body(tester);
      } finally {
        debugDisableShadows = true;
      }
    });

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

const _relayAddress = 'lb7666.top:7666';

/// Four remembered computers: one online and used often, one online over
/// the relay, one not sharing, one saved under another relay.
FakeRemoteComputerVault _computers() =>
    FakeRemoteComputerVault()
      ..records.addAll([
        RemoteComputer(
          id: '${'a' * 31}1',
          name: 'Office PC',
          service: 'desktop:office-pc',
          fingerprint: 'AB12-CD34-EF56-7890',
          relay: _relayAddress,
          namespace: 0,
          lastConnectedAt: DateTime.now().subtract(const Duration(hours: 2)),
          lastPath: 'direct',
          lastControlAllowed: true,
        ),
        RemoteComputer(
          id: '${'b' * 31}2',
          name: 'Home server',
          service: 'desktop:home-pc',
          fingerprint: 'FFFF-0000-FFFF-0000',
          relay: _relayAddress,
          namespace: 0,
          lastConnectedAt: DateTime.now().subtract(const Duration(days: 1)),
          lastPath: 'relay',
        ),
        RemoteComputer(
          id: '${'c' * 31}3',
          name: 'Lab workstation',
          service: 'desktop:lab-ws',
          fingerprint: '1111-2222-3333-4444',
          relay: _relayAddress,
          namespace: 0,
          lastConnectedAt: DateTime(2026, 11, 2, 10),
        ),
        const RemoteComputer(
          id: 'dddddddddddddddddddddddddddddd04',
          name: 'Parents laptop',
          service: 'desktop:parents',
          fingerprint: '5555-6666-7777-8888',
          relay: 'other.example:7666',
          namespace: 0,
        ),
      ]);

/// A relay with two of [_computers] sharing. This computer can share its
/// own desktop when [host], and a viewer is watching it when [viewed].
FakePbMapperApi _remoteApi({bool host = true, bool viewed = false}) {
  final api = FakePbMapperApi(
    serverServices: ['desktop:office-pc', 'desktop:home-pc', 'nas-files'],
  );
  api.desktop.available = DesktopCapabilities(
    host: host,
    displays: const [
      DesktopDisplay(1, 'Display 1', 2560, 1440),
      DesktopDisplay(2, 'Display 2', 1920, 1080),
    ],
    helper: const DesktopHelper(
      installed: true,
      running: true,
      authorized: true,
      identityVerified: true,
    ),
  );
  if (viewed) api.desktop.hostSnapshot = viewedHost(controlling: true);
  return api;
}

/// Lets the fake's picture decode: that takes real time, which a widget
/// test's clock does not pass by itself.
Future<void> _decode(WidgetTester tester) async {
  for (var i = 0; i < 30; i++) {
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 10)),
    );
    await tester.pump(const Duration(milliseconds: 50));
  }
}

/// Viewer settings whose gesture sheet was already seen, so a touch viewer
/// does not open it by itself.
Future<ViewerPrefs> _sheetSeen() async {
  final prefs = ViewerPrefs(persist: false);
  await prefs.setGestureSheetSeen(true);
  return prefs;
}

/// Opens Office PC from the Remote page. Unless [live] is false, the
/// session goes live with [RemoteScreenFixture] on screen, in control when
/// the host allows it.
Future<RemoteDesktop> _view(
  WidgetTester tester,
  FakePbMapperApi api, {
  bool live = true,
  bool controlAllowed = true,
}) async {
  final png = await RemoteScreenFixture.png(tester);
  final scope = AppScope.of(tester.element(find.byType(RemotePage)));
  await scope.remote.open(scope.store.computers.computers.first);
  await tester.pump(const Duration(milliseconds: 100));
  if (live) {
    api.desktop.viewerStatusValue = liveStatus(
      controlling: controlAllowed,
      controlAllowed: controlAllowed,
      stats: const DesktopStats(
        path: 'direct',
        codec: 'h264',
        presentedFps: 60,
        rttMs: 18,
      ),
    );
    await tester.pump(const Duration(milliseconds: 300));
    api.desktop.emitJpeg(png, generation: 1, frameId: 1);
  }
  await _decode(tester);
  return scope.remote;
}

const _windowManager = MethodChannel('window_manager');

/// Runs [body] as Windows, where a computer can share its desktop: the app
/// draws its own caption buttons, and the viewer its dark ones. Its window
/// answers as a restored one.
Future<void> _onWindows(
  WidgetTester tester,
  Future<void> Function() body,
) async {
  final messenger = tester.binding.defaultBinaryMessenger;
  messenger.setMockMethodCallHandler(
    _windowManager,
    (call) async => switch (call.method) {
      'isMaximized' || 'isFullScreen' => false,
      _ => null,
    },
  );
  debugDefaultTargetPlatformOverride = TargetPlatform.windows;
  try {
    await body();
  } finally {
    debugDefaultTargetPlatformOverride = null;
    messenger.setMockMethodCallHandler(_windowManager, null);
  }
}

/// The app's face on Windows, Segoe UI, from this machine's own fonts: no
/// other machine has it, and there a Windows preview's words are boxes.
Future<void> _loadSegoeUi() async {
  final fonts = Directory(
    '${Platform.environment['SystemRoot'] ?? r'C:\Windows'}/Fonts',
  );
  final files = [
    for (final name in ['segoeui.ttf', 'seguisb.ttf', 'segoeuib.ttf'])
      File('${fonts.path}/$name'),
  ].where((file) => file.existsSync());
  if (files.isEmpty) return;
  final loader = FontLoader('Segoe UI');
  for (final file in files) {
    loader.addFont(Future.value(ByteData.sublistView(file.readAsBytesSync())));
  }
  await loader.load();
}

void main() {
  if (_dir == null) {
    test('previews are opt-in', () {}, skip: 'set PB_MAPPER_UI_PREVIEW_DIR');
    return;
  }

  setUpAll(() async {
    Directory(_dir!).createSync(recursive: true);
    await loadRealFonts();
    await _loadSegoeUi();
  });

  for (final (name, size) in [
    ('desktop', const Size(1280, 820)),
    ('phone', const Size(390, 844)),
  ]) {
    _preview('port mapping $name', (tester) async {
      final api =
          FakePbMapperApi(
              services: [
                fakeService(
                  serviceKey: 'nas-files',
                  portMapping: PortMappingMode.always,
                ),
              ],
            )
            ..tunnelEventFeed = Stream.fromIterable([
              for (final (seq, kind) in [
                (1, {'kind': 'relay_ready', 'latency_ms': 38}),
                (2, {'kind': 'direct_stage_changed', 'stage': 'punching'}),
                (3, {'kind': 'direct_fallback', 'stage': 'punching'}),
                (
                  4,
                  {
                    'kind': 'port_mapping_changed',
                    'mapping': {'stage': 'discovering'},
                  },
                ),
                (
                  5,
                  {
                    'kind': 'port_mapping_changed',
                    'mapping': {'stage': 'requesting', 'protocol': 'pcp'},
                  },
                ),
                (
                  6,
                  {'kind': 'port_mapping_changed', 'mapping': _activeMapping},
                ),
                (7, {'kind': 'direct_established', 'setup_ms': 184}),
              ])
                TunnelEventsUpdate.fromMap({
                  'type': 'event',
                  'event': {
                    'seq': seq,
                    'age_ms': (8 - seq) * 41000,
                    'repeats': 1,
                    'kind': kind,
                  },
                  'diagnostics': {
                    'relay_transport': 'quic',
                    'last_setup_latency_ms': 38,
                    'direct': {
                      'direct_sessions': 3,
                      'relay_sessions': 0,
                      'fallbacks': 1,
                    },
                    'port_mapping': {
                      'current': _activeMapping,
                      'attempts': 1,
                      'renewals': 2,
                      'failures': 0,
                    },
                  },
                }),
            ]);
      await pumpApp(
        tester,
        api,
        page: AppPage.tunnels,
        size: size,
        locale: const Locale('zh'),
      );
      await tester.tap(find.byKey(const Key('menu-share:nas-files')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('menu-diagnostics')));
      await settle(tester);
      await _capture(tester, 'port_mapping_diagnostics_$name');
      await tester.tap(find.byTooltip('关闭').last);
      await settle(tester);
      await tester.tap(find.byKey(const Key('menu-share:nas-files')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('menu-edit')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('tunnel-more')));
      await settle(tester);
      await tester.ensureVisible(find.byKey(const Key('tunnel-port-mapping')));
      await settle(tester);
      await _capture(tester, 'port_mapping_editor_$name');
      expect(tester.takeException(), isNull);
      await finish(tester);
    });
  }

  for (final (name, theme) in [
    ('light', ThemeMode.light),
    ('dark', ThemeMode.dark),
  ]) {
    _preview('overview $name', (tester) async {
      await pumpApp(tester, _busy(), themeMode: theme);
      await _capture(tester, 'overview_$name');
      await finish(tester);
    });

    _preview('tunnels $name', (tester) async {
      await pumpApp(tester, _busy(), themeMode: theme, page: AppPage.tunnels);
      await _capture(tester, 'tunnels_$name');
      await finish(tester);
    });

    _preview('settings $name', (tester) async {
      await pumpApp(tester, _busy(), themeMode: theme, page: AppPage.settings);
      await _capture(tester, 'settings_$name');
      await finish(tester);
    });

    _preview('editor $name', (tester) async {
      await pumpApp(tester, _busy(), themeMode: theme, page: AppPage.tunnels);
      await tester.tap(find.byKey(const Key('new-tunnel')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('tunnel-kind-connect')));
      await settle(tester);
      await _capture(tester, 'editor_$name');
      await finish(tester);
    });

    _preview('setup $name', (tester) async {
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

  _preview('overview selected', (tester) async {
    await pumpApp(tester, _busy(), themeMode: ThemeMode.dark);
    await tester.tap(find.byKey(const Key('galaxy-canvas')));
    await tester.sendKeyEvent(LogicalKeyboardKey.arrowRight);
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await settle(tester);
    await _capture(tester, 'overview_selected_dark');
    await finish(tester);
  });

  for (final key in ['office-rdp', 'media-server']) {
    _preview('overview path $key', (tester) async {
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

  _preview('empty overview', (tester) async {
    await pumpApp(
      tester,
      FakePbMapperApi(serverServices: ['build-cache']),
      themeMode: ThemeMode.light,
    );
    await _capture(tester, 'overview_empty_light');
    await finish(tester);
  });

  _preview('phone overview', (tester) async {
    await pumpApp(tester, _busy(), size: const Size(393, 852));
    await _capture(tester, 'phone_overview');
    await finish(tester);
  });

  _preview('relay services light', (tester) async {
    await _relay(tester, 'relay_services_light');
  });

  _preview('relay services dark', (tester) async {
    await _relay(tester, 'relay_services_dark', theme: ThemeMode.dark);
  });

  _preview('relay connections', (tester) async {
    await _relay(tester, 'relay_connections_light', section: 'connections');
  });

  _preview('relay credentials', (tester) async {
    await _relay(
      tester,
      'relay_credentials_dark',
      section: 'credentials',
      theme: ThemeMode.dark,
    );
  });

  _preview('relay security', (tester) async {
    await _relay(tester, 'relay_security_light', section: 'security');
  });

  _preview('relay this computer', (tester) async {
    await _relay(tester, 'relay_local_light', section: 'local');
  });

  _preview('relay over quic light', (tester) async {
    await _relay(tester, 'relay_quic_light', quic: true);
  });

  _preview('relay over quic dark', (tester) async {
    await _relay(tester, 'relay_quic_dark', quic: true, theme: ThemeMode.dark);
  });

  _preview('relay offering quic to tcp', (tester) async {
    await _relay(tester, 'relay_offers_quic_light');
  });

  _preview('phone relay quic zh', (tester) async {
    await _relay(
      tester,
      'phone_relay_quic_zh',
      quic: true,
      size: const Size(393, 852),
      locale: const Locale('zh'),
    );
  });

  _preview('settings quic', (tester) async {
    await pumpApp(tester, _overQuic(_busy()), page: AppPage.settings);
    await _capture(tester, 'settings_quic_light');
    await finish(tester);
  });

  _preview('settings lan first', (tester) async {
    Future<void> reveal() => tester.scrollUntilVisible(
      find.byKey(const Key('settings-lan-first')),
      200,
      scrollable: find
          .descendant(
            of: find.byKey(const Key('settings-list')),
            matching: find.byType(Scrollable),
          )
          .first,
    );
    await pumpApp(tester, _busy(), page: AppPage.settings);
    await reveal();
    await _capture(tester, 'settings_lan_first_light');
    // Turned off with tunnels running: the offer to restart them.
    await tester.tap(find.byKey(const Key('settings-lan-first')));
    await settle(tester);
    await _capture(tester, 'settings_lan_first_restart_light');
    await finish(tester);

    await pumpApp(
      tester,
      _busy(),
      page: AppPage.settings,
      themeMode: ThemeMode.dark,
      size: const Size(393, 852),
      locale: const Locale('zh'),
    );
    await reveal();
    // Mid-screen, clear of the bottom bar.
    await Scrollable.ensureVisible(
      tester.element(find.byKey(const Key('settings-lan-first'))),
      alignment: 0.5,
    );
    await settle(tester);
    await _capture(tester, 'settings_lan_first_phone_zh_dark');
    await finish(tester);
  });

  _preview('settings quic on a relay without it', (tester) async {
    await pumpApp(
      tester,
      _overQuic(_busy())..relayQuicPort = null,
      page: AppPage.settings,
      themeMode: ThemeMode.dark,
    );
    await _capture(tester, 'settings_no_quic_dark');
    await finish(tester);
  });

  _preview('tunnels over quic', (tester) async {
    await pumpApp(tester, _overQuic(_busy()), page: AppPage.tunnels);
    await _capture(tester, 'tunnels_quic_light');
    await finish(tester);
  });

  _preview('relay this computer with quic', (tester) async {
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

  _preview('carrier move dialog', (tester) async {
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

  _preview('relay as a temporary credential', (tester) async {
    await _relay(tester, 'relay_temporary_light', role: RelayRole.temporary);
  });

  _preview('relay issue dialog', (tester) async {
    await pumpApp(tester, _relayBusy(), page: AppPage.relay);
    await tester.tap(find.byKey(const Key('relay-issue')));
    await settle(tester);
    await _capture(tester, 'relay_issue_light');
    await finish(tester);
  });

  _preview('phone relay zh', (tester) async {
    await _relay(
      tester,
      'phone_relay_zh',
      size: const Size(393, 852),
      locale: const Locale('zh'),
    );
  });

  _preview('phone relay credentials', (tester) async {
    await _relay(
      tester,
      'phone_relay_credentials',
      section: 'credentials',
      size: const Size(393, 852),
    );
  });

  _preview('phone tunnels zh', (tester) async {
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

  group('remote desktop', () {
    tearDown(() {
      InviteScanner.debugSupportedOverride = null;
      InviteScanner.debugFakeCamera = false;
    });

    _preview('remote setup', (tester) async {
      await _onWindows(tester, () async {
        await pumpApp(
          tester,
          _remoteApi(),
          page: AppPage.remote,
          size: const Size(1440, 900),
        );
        await _capture(tester, 'remote_setup_light');
        await finish(tester);
      });
    });

    for (final (name, theme) in [
      ('light', ThemeMode.light),
      ('dark', ThemeMode.dark),
    ]) {
      _preview('remote sharing $name', (tester) async {
        await _onWindows(tester, () async {
          final api = _remoteApi(viewed: true);
          await pumpApp(
            tester,
            api,
            page: AppPage.remote,
            themeMode: theme,
            size: const Size(1440, 900),
            vault: _computers(),
          );
          // A session left paused offers Back to picture, and its last
          // picture becomes the row's thumbnail: let that write finish.
          final remote = await _view(tester, api);
          remote.back();
          await tester.runAsync(
            () => Future<void>.delayed(const Duration(milliseconds: 500)),
          );
          await _decode(tester);
          await tester.tap(find.byKey(const Key('host-qr')));
          await settle(tester);
          await _capture(tester, 'remote_sharing_$name');
          await finish(tester);
        });
      });
    }

    _preview('remote add dialog', (tester) async {
      await _onWindows(tester, () async {
        await pumpApp(
          tester,
          _remoteApi(),
          page: AppPage.remote,
          size: const Size(1440, 900),
          vault: _computers(),
        );
        await tester.tap(find.byKey(const Key('remote-add')));
        await settle(tester);
        await tester.enterText(
          find.byKey(const Key('add-invite')),
          'pbd1.studio-pc.k7Hq-SECRET-Zx2Q',
        );
        await tester.pump(const Duration(milliseconds: 300));
        await settle(tester);
        await tester.tap(find.byKey(const Key('add-more')));
        await settle(tester);
        await _capture(tester, 'remote_add_dialog_light');
        await finish(tester);
      });
    });

    /// The desktop viewer over Office PC at 1440×900, as Windows: [act]
    /// brings it to the state captured as [name].
    Future<void> desktopViewer(
      WidgetTester tester,
      String name,
      Future<void> Function(FakePbMapperApi api) act, {
      bool live = true,
      bool controlAllowed = true,
      ThemeMode theme = ThemeMode.light,
    }) => _onWindows(tester, () async {
      final api = _remoteApi();
      // Held mid-connect, it stays on the Connecting overlay.
      if (!live) api.desktop.connectGate = Completer<void>();
      await pumpApp(
        tester,
        api,
        page: AppPage.remote,
        themeMode: theme,
        size: const Size(1440, 900),
        vault: _computers(),
      );
      await _view(tester, api, live: live, controlAllowed: controlAllowed);
      await act(api);
      await _capture(tester, name);
      api.desktop.connectGate?.complete();
      await finish(tester);
    });

    _preview('viewer connecting', (tester) async {
      await desktopViewer(
        tester,
        'viewer_connecting',
        live: false,
        (_) async {},
      );
    });

    _preview('viewer shortcuts', (tester) async {
      await desktopViewer(
        tester,
        'viewer_shortcuts_dark',
        theme: ThemeMode.dark,
        (_) async {
          await tester.tap(find.byKey(const Key('viewer-shortcuts')));
          await tester.pump(const Duration(milliseconds: 400));
        },
      );
    });

    _preview('viewer reconnect', (tester) async {
      await desktopViewer(tester, 'viewer_reconnect', (api) async {
        api.desktop.viewerStatusValue = stoppedStatus('transport');
        await tester.pump(const Duration(milliseconds: 500));
      });
    });

    _preview('viewer identity', (tester) async {
      await desktopViewer(tester, 'viewer_identity', (api) async {
        // The connection drops, and the computer that answers the retry is
        // not the one remembered.
        api.desktop
          ..viewerStatusValue = stoppedStatus('transport')
          ..failNext('connect', 'IDENTITY_CHANGED');
        await tester.pump(const Duration(seconds: 3));
        await settle(tester);
      });
    });

    _preview('viewer collapsed, view only', (tester) async {
      final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await desktopViewer(
        tester,
        'viewer_collapsed_view_only',
        controlAllowed: false,
        (_) async {
          // Unpinned, the toolbar folds away once the pointer leaves it.
          await tester.tap(find.byKey(const Key('viewer-pin')));
          await mouse.addPointer(location: const Offset(700, 500));
          await tester.pump(const Duration(milliseconds: 2600));
          await tester.pump(const Duration(milliseconds: 400));
        },
      );
      await mouse.removePointer();
    });

    _preview('sharing fallback over a dialog', (tester) async {
      await _onWindows(tester, () async {
        await pumpApp(tester, _remoteApi(viewed: true), page: AppPage.tunnels);
        await tester.tap(find.byKey(const Key('new-tunnel')));
        await settle(tester);
        await _capture(tester, 'sharing_fallback_dialog');
        await finish(tester);
      });
    });

    _preview('title bar on Windows at 125 %', (tester) async {
      await _onWindows(tester, () async {
        const size = Size(1180, 780), dpr = 1.25;
        for (final (suffix, theme) in const [
          ('', ThemeMode.light),
          ('_dark', ThemeMode.dark),
        ]) {
          await pumpApp(
            tester,
            _remoteApi(viewed: true),
            page: AppPage.tunnels,
            size: size,
            themeMode: theme,
          );
          tester.view
            ..devicePixelRatio = dpr
            ..physicalSize = size * dpr;
          await settle(tester);
          final mouse = await tester.createGesture(
            kind: PointerDeviceKind.mouse,
          );
          await mouse.addPointer(
            location: tester.getCenter(find.byKey(const Key('caption-close'))),
          );
          await tester.pump();
          await _capture(
            tester,
            'title_bar_windows_125$suffix',
            pixelRatio: dpr,
          );
          await mouse.removePointer();
          await finish(tester);
        }
      });
    });

    _preview('phone remote tab', (tester) async {
      InviteScanner.debugSupportedOverride = true;
      await pumpApp(
        tester,
        _remoteApi(host: false),
        page: AppPage.remote,
        size: const Size(390, 844),
        vault: _computers(),
      );
      await _capture(tester, 'phone_remote_tab');
      await finish(tester);
    });

    _preview('phone add from the clipboard', (tester) async {
      InviteScanner.debugSupportedOverride = true;
      await pumpApp(
        tester,
        _remoteApi(host: false),
        page: AppPage.remote,
        size: const Size(390, 844),
        vault: _computers(),
      );
      final messenger = tester.binding.defaultBinaryMessenger;
      messenger.setMockMethodCallHandler(
        SystemChannels.platform,
        (call) async => call.method == 'Clipboard.getData'
            ? {'text': 'pbd1.studio-pc.k7Hq-SECRET-Zx2Q'}
            : null,
      );
      addTearDown(
        () => messenger.setMockMethodCallHandler(SystemChannels.platform, null),
      );
      await tester.tap(find.byKey(const Key('remote-paste')));
      await settle(tester);
      await _capture(tester, 'phone_add_paste');
      await finish(tester);
    });

    /// The touch viewer over Office PC on a landscape phone.
    Future<void> phoneViewer(WidgetTester tester, ViewerPrefs prefs) async {
      final api = _remoteApi(host: false);
      await pumpApp(
        tester,
        api,
        page: AppPage.remote,
        themeMode: ThemeMode.dark,
        size: const Size(844, 390),
        viewerPrefs: prefs,
        vault: _computers(),
      );
      await _view(tester, api);
    }

    _preview('phone viewer, trackpad', (tester) async {
      await phoneViewer(tester, await _sheetSeen());
      // Past the mode hint.
      await tester.pump(const Duration(seconds: 4));
      await _capture(tester, 'phone_viewer_trackpad');
      await finish(tester);
    });

    _preview('phone viewer, keyboard with Ctrl locked', (tester) async {
      await phoneViewer(tester, await _sheetSeen());
      await tester.pump(const Duration(seconds: 4));
      await tester.tap(find.byKey(const Key('touch-keyboard')));
      tester.view.viewInsets = const FakeViewPadding(bottom: 190);
      addTearDown(tester.view.resetViewInsets);
      await tester.pump(const Duration(milliseconds: 300));
      // Tapped twice: locked.
      for (var i = 0; i < 2; i++) {
        await tester.tap(find.byKey(const Key('modifier-control')));
        await tester.pump();
      }
      await tester.pump(const Duration(milliseconds: 300));
      await _capture(tester, 'phone_viewer_keyboard');
      await finish(tester);
    });

    _preview('phone gesture sheet', (tester) async {
      // The first session opens the sheet by itself.
      await phoneViewer(tester, ViewerPrefs(persist: false));
      await tester.pump(const Duration(milliseconds: 500));
      await _capture(tester, 'phone_gesture_sheet');
      await finish(tester);
    });
  });
}

const _activeMapping = {
  'stage': 'active',
  'protocol': 'pcp',
  'local_port': 52183,
  'gateway': '192.168.1.1',
  'external_addr': '203.0.113.7:52183',
  'active': true,
  'lease_lifetime_ms': 7200000,
};
