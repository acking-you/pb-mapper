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

Future<void> _loadFonts() async {
  Future<void> family(String name, List<String> assets) async {
    final loader = FontLoader(name);
    for (final asset in assets) {
      loader.addFont(rootBundle.load(asset));
    }
    await loader.load();
  }

  await family('GoogleSansFlex', [
    'assets/fonts/GoogleSansFlex-Regular.ttf',
    'assets/fonts/GoogleSansFlex-Medium.ttf',
    'assets/fonts/GoogleSansFlex-SemiBold.ttf',
    'assets/fonts/GoogleSansFlex-Bold.ttf',
  ]);
  await family('GeistMono', [
    'assets/fonts/GeistMono-Regular.ttf',
    'assets/fonts/GeistMono-Medium.ttf',
    'assets/fonts/GeistMono-SemiBold.ttf',
  ]);
  await family('NotoSansSC', [
    'assets/fonts/NotoSansSC-Regular.subset.ttf',
    'assets/fonts/NotoSansSC-Medium.subset.ttf',
    'assets/fonts/NotoSansSC-SemiBold.subset.ttf',
  ]);
  await family('packages/lucide_icons_flutter/Lucide300', [
    'packages/lucide_icons_flutter/assets/build_font/LucideVariable-w300.ttf',
  ]);
  final flutterRoot = Platform.environment['FLUTTER_ROOT'];
  if (flutterRoot != null) {
    final icons = File(
      '$flutterRoot/bin/cache/artifacts/material_fonts/materialicons-regular.otf',
    );
    if (icons.existsSync()) {
      final loader = FontLoader('MaterialIcons')
        ..addFont(Future.value(ByteData.sublistView(icons.readAsBytesSync())));
      await loader.load();
    }
  }
}

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
    fakeService(serviceKey: 'nas-files', localAddress: '127.0.0.1:5000'),
    fakeService(serviceKey: 'home-ssh', localAddress: '127.0.0.1:22'),
    fakeService(
      serviceKey: 'grafana',
      localAddress: '127.0.0.1:3000',
      status: 'retrying',
      message: 'local service refused the connection',
    ),
  ],
  clients: [
    fakeClient(serviceKey: 'office-rdp', localAddress: '127.0.0.1:13389'),
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

void main() {
  if (_dir == null) {
    test('previews are opt-in', () {}, skip: 'set PB_MAPPER_UI_PREVIEW_DIR');
    return;
  }

  setUpAll(() async {
    Directory(_dir!).createSync(recursive: true);
    await _loadFonts();
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
