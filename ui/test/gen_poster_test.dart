/// Derives the README posters from the app itself, rendered with the bundled
/// fonts and fixture data: in front, the galaxy of this computer and its
/// relay; behind it, the Tunnels page; on the phone, a connection being made
/// in one step. Beside them: the mark, the name and what pb-mapper does.
///
/// Opt-in, since PNG output differs a little across platforms:
///
/// ```sh
/// PB_MAPPER_REGEN_POSTER=1 flutter test test/gen_poster_test.dart
/// ```
///
/// Writes `docs/assets/poster.png` and `docs/assets/poster.zh-CN.png`.
library;

import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/widgets/brand_mark.dart';

import 'fake_pb_mapper_api.dart';
import 'support.dart';

/// The poster's layout size; it is written at [_outputScale] times this.
const _poster = Size(1536, 1024);
const _outputScale = 1.25;

/// Screens are captured at twice their logical size, so they stay sharp once
/// placed in the poster.
const _captureScale = 2.0;

/// The desktop screens' logical size, and the phone's.
const _desktop = Size(1040, 720);
const _phone = Size(393, 852);

/// Where the screens sit in the poster. Windows keep the desktop screens'
/// proportions; the phone's frame adds its bezel around the screen's.
const _back = Rect.fromLTWH(890, 62, 600, 415.4);
const _front = Rect.fromLTWH(640, 284, 850, 588.5);
const _bezel = 9.0;
const _phoneWidth = 262.0;
final _phoneRect = Rect.fromLTWH(
  566,
  392,
  _phoneWidth,
  (_phoneWidth - 2 * _bezel) * _phone.height / _phone.width + 2 * _bezel,
);

/// The light theme's own tokens: ink, muted text, the neutral surface, the
/// hairline and the one signal blue.
final _scheme = appColorScheme(Brightness.light);
final _ink = _scheme.onSurface;
final _muted = _scheme.onSurfaceVariant;
final _signal = _scheme.tertiary;
final _hairline = _scheme.outlineVariant;
const _faint = Color(0xFFB4B4B2);

/// One poster's words.
class _Copy {
  const _Copy({
    required this.locale,
    required this.motto,
    required this.headline,
    required this.body,
    required this.second,
    required this.secondBody,
    required this.chips,
    required this.output,
  });

  final Locale locale;

  /// Under the name, in runs; the true ones are drawn in the signal blue.
  final List<(String, bool)> motto;
  final String headline;
  final String body;
  final String second;
  final String secondBody;
  final List<(IconData, String)> chips;

  /// Relative to `ui/`, where `flutter test` runs.
  final String output;
}

const _english = _Copy(
  locale: Locale('en'),
  motto: [
    ('P2P', true),
    (' first. ', false),
    ('Relay', true),
    (' always.', false),
  ],
  headline: 'Every private service,\none click away.',
  body:
      'Share a TCP or UDP service from one computer and open it on any '
      'other: direct over QUIC when the network allows, through your relay '
      'when it does not.',
  second: 'Your tunnels, as a galaxy.',
  secondBody:
      'The app draws this computer, the relay and every tunnel between '
      'them. Click a planet to start, stop, edit or connect it.',
  chips: [
    (LucideIcons.zap300, 'Direct over QUIC'),
    (LucideIcons.radioTower300, 'Relay fallback'),
    (LucideIcons.arrowLeftRight300, 'TCP & UDP'),
    (LucideIcons.keyRound300, 'Revocable keys'),
    (LucideIcons.code300, 'Rust & Node SDKs'),
  ],
  output: '../docs/assets/poster.png',
);

const _chinese = _Copy(
  locale: Locale('zh'),
  motto: [('P2P 直连', true), ('优先，', false), ('中继', true), ('兜底。', false)],
  headline: '每个私有服务，\n一键直达。',
  // Broken by hand: Chinese wraps anywhere, and left alone it strands the
  // last character of a sentence on a line of its own.
  body:
      '在一台电脑上共享 TCP / UDP 服务，\n'
      '在另一台电脑上打开：能直连就走 QUIC 直连，\n'
      '打不通就自动走你的中继。',
  second: '你的隧道，是一片星系。',
  secondBody:
      '这台电脑、中继和其间的每条隧道，\n'
      '都画成一片星系。点一颗行星，\n'
      '就能启动、停止、编辑或连接它。',
  chips: [
    (LucideIcons.zap300, 'QUIC 直连'),
    (LucideIcons.radioTower300, '中继兜底'),
    (LucideIcons.arrowLeftRight300, 'TCP 与 UDP'),
    (LucideIcons.keyRound300, '可吊销密钥'),
    (LucideIcons.code300, 'Rust 与 Node SDK'),
  ],
  output: '../docs/assets/poster.zh-CN.png',
);

/// A computer with a few services shared and a couple of connections open,
/// against a relay that holds a few more from other devices.
FakePbMapperApi _api(Locale locale) => FakePbMapperApi(
  config: const ConfigStatus(
    serverAddress: 'relay.example.com:7666',
    keepAliveEnabled: true,
    msgHeaderKey: '',
  ),
  services: [
    fakeService(serviceKey: 'nas-files', localAddress: '127.0.0.1:5000'),
    fakeService(serviceKey: 'home-ssh', localAddress: '127.0.0.1:22'),
    fakeService(serviceKey: 'jellyfin', localAddress: '127.0.0.1:8096'),
    fakeService(
      serviceKey: 'grafana',
      localAddress: '127.0.0.1:3000',
      status: 'retrying',
      message: locale.languageCode == 'zh'
          ? '本地服务拒绝了连接，正在重试'
          : 'The local service refused the connection; retrying',
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
    'jellyfin',
    'grafana',
    'office-rdp',
    'lab-db',
    'build-cache',
    'media-server',
    'gpu-notebook',
  ],
  conns: {
    'office-rdp': [fakeConn(connId: 41), fakeConn(connId: 42)],
  },
);

/// The app on [platform] at [size], on [page], after [arrange].
Future<ui.Image> _shot(
  WidgetTester tester, {
  required _Copy copy,
  required TargetPlatform platform,
  required Size size,
  required AppPage page,
  ThemeMode theme = ThemeMode.light,
  EdgeInsets safeArea = EdgeInsets.zero,
  Future<void> Function(WidgetTester tester)? arrange,
}) async {
  debugDefaultTargetPlatformOverride = platform;
  try {
    tester.view.padding = tester.view.viewPadding = FakeViewPadding(
      top: safeArea.top,
      bottom: safeArea.bottom,
    );
    await pumpApp(
      tester,
      _api(copy.locale),
      page: page,
      size: size,
      themeMode: theme,
      locale: copy.locale,
    );
    if (arrange != null) {
      await arrange(tester);
      await settle(tester);
    }
    final boundary = tester.renderObject<RenderRepaintBoundary>(
      find.byKey(appRootKey),
    );
    final image = await tester.runAsync(
      () => boundary.toImage(pixelRatio: _captureScale),
    );
    await finish(tester);
    return image!;
  } finally {
    debugDefaultTargetPlatformOverride = null;
    tester.view
      ..resetPadding()
      ..resetViewPadding();
  }
}

TextStyle _style(
  double size, {
  FontWeight weight = FontWeight.w400,
  Color? color,
  double height = 1.15,
  double spacing = 0,
}) => TextStyle(
  fontFamily: 'GoogleSansFlex',
  fontFamilyFallback: const ['NotoSansSC'],
  fontSize: size,
  fontWeight: weight,
  color: color ?? _ink,
  height: height,
  letterSpacing: spacing,
);

/// A soft, outer-only shadow, the way the app's own sheets cast one.
const _shadows = [
  BoxShadow(color: Color(0x24000000), blurRadius: 48, offset: Offset(0, 28)),
  BoxShadow(color: Color(0x14000000), blurRadius: 3, offset: Offset(0, 2)),
];

Widget _screen(ui.Image image) => RawImage(
  image: image,
  fit: BoxFit.cover,
  filterQuality: FilterQuality.high,
);

/// A desktop window: the captured screen, with the macOS window controls in
/// the strip the rail leaves for them, and a hairline edge.
class _Window extends StatelessWidget {
  const _Window(this.screen);

  final ui.Image screen;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final scale = constraints.maxWidth / _desktop.width;
        const colors = [
          Color(0xFFFF5F57),
          Color(0xFFFEBC2E),
          Color(0xFF28C840),
        ];
        final shape = BorderRadius.circular(14);
        return DecoratedBox(
          decoration: BoxDecoration(borderRadius: shape, boxShadow: _shadows),
          position: DecorationPosition.background,
          child: ClipRRect(
            borderRadius: shape,
            child: Stack(
              fit: StackFit.expand,
              children: [
                _screen(screen),
                // The rail's 46 pt drag strip is where the lights go.
                for (final (i, color) in colors.indexed)
                  Positioned(
                    left: (20 + i * 20 - 6) * scale,
                    top: (23 - 6) * scale,
                    width: 12 * scale,
                    height: 12 * scale,
                    child: DecoratedBox(
                      decoration: BoxDecoration(
                        color: color,
                        shape: BoxShape.circle,
                      ),
                    ),
                  ),
                DecoratedBox(
                  decoration: BoxDecoration(
                    borderRadius: shape,
                    border: Border.all(color: const Color(0x1F000000)),
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

/// The phone: the captured screen in an ink frame, with a status bar around
/// its island.
class _Phone extends StatelessWidget {
  const _Phone(this.screen);

  final ui.Image screen;

  @override
  Widget build(BuildContext context) {
    const radius = 44.0;
    return DecoratedBox(
      decoration: BoxDecoration(
        color: _ink,
        borderRadius: BorderRadius.circular(radius),
        boxShadow: _shadows,
      ),
      child: Padding(
        padding: const EdgeInsets.all(_bezel),
        child: ClipRRect(
          borderRadius: BorderRadius.circular(radius - _bezel),
          child: Stack(
            fit: StackFit.expand,
            children: [
              _screen(screen),
              Positioned(
                top: 6,
                left: 0,
                right: 0,
                height: 24,
                child: Row(
                  children: [
                    const SizedBox(width: 28),
                    Text(
                      '9:41',
                      style: _style(13, weight: FontWeight.w600, height: 1),
                    ),
                    const Spacer(),
                    Container(
                      width: _phoneWidth * 0.3,
                      height: 22,
                      decoration: BoxDecoration(
                        color: _ink,
                        borderRadius: BorderRadius.circular(11),
                      ),
                    ),
                    const Spacer(),
                    Container(
                      width: 22,
                      height: 11,
                      padding: const EdgeInsets.all(2),
                      decoration: BoxDecoration(
                        border: Border.all(color: _ink, width: 1.2),
                        borderRadius: BorderRadius.circular(3),
                      ),
                      child: DecoratedBox(
                        decoration: BoxDecoration(
                          color: _ink,
                          borderRadius: BorderRadius.circular(1.5),
                        ),
                      ),
                    ),
                    const SizedBox(width: 22),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _Chip extends StatelessWidget {
  const _Chip(this.icon, this.label);

  final IconData icon;
  final String label;

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 34,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      decoration: BoxDecoration(
        color: Colors.white,
        borderRadius: BorderRadius.circular(17),
        border: Border.all(color: _hairline),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 16, color: _signal),
          const SizedBox(width: 7),
          Text(label, style: _style(14, weight: FontWeight.w500, height: 1)),
        ],
      ),
    );
  }
}

class _Poster extends StatelessWidget {
  const _Poster({
    required this.copy,
    required this.galaxy,
    required this.tunnels,
    required this.phone,
  });

  final _Copy copy;
  final ui.Image galaxy;
  final ui.Image tunnels;
  final ui.Image phone;

  @override
  Widget build(BuildContext context) {
    const left = 96.0;
    return SizedBox.fromSize(
      size: _poster,
      child: Stack(
        children: [
          // The app's backdrop: neutral, lit only by white.
          Positioned.fill(
            child: DecoratedBox(
              decoration: BoxDecoration(
                gradient: LinearGradient(
                  begin: Alignment.topCenter,
                  end: Alignment.bottomCenter,
                  colors: [_scheme.surface, const Color(0xFFEFEFED)],
                ),
              ),
            ),
          ),
          const Positioned.fill(
            child: DecoratedBox(
              decoration: BoxDecoration(
                gradient: RadialGradient(
                  center: Alignment(-0.58, -0.41),
                  radius: 0.95,
                  colors: [Color(0xCCFFFFFF), Color(0x00FFFFFF)],
                ),
              ),
            ),
          ),
          // The logo's blue, faintly, where the galaxy is: the one signal.
          Positioned.fill(
            child: DecoratedBox(
              decoration: BoxDecoration(
                gradient: RadialGradient(
                  center: const Alignment(0.42, 0.12),
                  radius: 0.62,
                  colors: [
                    _signal.withValues(alpha: 0.13),
                    _signal.withValues(alpha: 0),
                  ],
                ),
              ),
            ),
          ),
          Positioned(
            left: left,
            top: 0,
            bottom: 0,
            width: 480,
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const BrandMark(size: 84),
                const SizedBox(height: 22),
                Text(
                  'pb-mapper',
                  softWrap: false,
                  style: _style(84, weight: FontWeight.w700, spacing: -2.4),
                ),
                const SizedBox(height: 8),
                Text.rich(
                  TextSpan(
                    children: [
                      for (final (text, accent) in copy.motto)
                        TextSpan(
                          text: text,
                          style: TextStyle(color: accent ? _signal : _faint),
                        ),
                    ],
                  ),
                  style: _style(34, weight: FontWeight.w600, spacing: 0.2),
                ),
                const SizedBox(height: 40),
                Text(
                  copy.headline,
                  style: _style(
                    44,
                    weight: FontWeight.w600,
                    height: 1.14,
                    spacing: -0.6,
                  ),
                ),
                const SizedBox(height: 22),
                SizedBox(
                  width: 440,
                  child: Text(
                    copy.body,
                    style: _style(18, color: _muted, height: 1.5),
                  ),
                ),
                const SizedBox(height: 28),
                Text(
                  copy.second,
                  style: _style(22, weight: FontWeight.w600, spacing: -0.2),
                ),
                const SizedBox(height: 10),
                SizedBox(
                  width: 440,
                  child: Text(
                    copy.secondBody,
                    style: _style(18, color: _muted, height: 1.5),
                  ),
                ),
                const SizedBox(height: 26),
                SizedBox(
                  width: 460,
                  child: Wrap(
                    spacing: 7,
                    runSpacing: 7,
                    children: [
                      for (final (icon, label) in copy.chips)
                        _Chip(icon, label),
                    ],
                  ),
                ),
                const SizedBox(height: 18),
                Text(
                  'Windows · macOS · Linux · Android · iOS',
                  style: _style(
                    16,
                    weight: FontWeight.w500,
                    color: _faint,
                    spacing: 0.2,
                  ),
                ),
              ],
            ),
          ),
          // Behind: every tunnel; in front: the galaxy; on the phone, a
          // connection being made.
          Positioned.fromRect(rect: _back, child: _Window(tunnels)),
          Positioned.fromRect(rect: _front, child: _Window(galaxy)),
          Positioned.fromRect(rect: _phoneRect, child: _Phone(phone)),
        ],
      ),
    );
  }
}

Future<void> _render(WidgetTester tester, _Copy copy) async {
  final galaxy = await _shot(
    tester,
    copy: copy,
    platform: TargetPlatform.macOS,
    size: _desktop,
    page: AppPage.overview,
    theme: ThemeMode.dark,
  );
  final tunnels = await _shot(
    tester,
    copy: copy,
    platform: TargetPlatform.macOS,
    size: _desktop,
    page: AppPage.tunnels,
  );
  final phone = await _shot(
    tester,
    copy: copy,
    platform: TargetPlatform.iOS,
    size: _phone,
    page: AppPage.tunnels,
    safeArea: const EdgeInsets.only(top: 54, bottom: 34),
    // Connect to a service on the relay: pick it, and the form is done.
    arrange: (tester) async {
      await tester.tap(find.byKey(const Key('new-tunnel')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('tunnel-kind-connect')));
      await settle(tester);
      await tester.tap(find.byKey(const Key('suggestion-media-server')));
    },
  );

  tester.view
    ..physicalSize = _poster
    ..devicePixelRatio = 1;
  await tester.pumpWidget(
    RepaintBoundary(
      key: appRootKey,
      child: Directionality(
        textDirection: TextDirection.ltr,
        child: _Poster(
          copy: copy,
          galaxy: galaxy,
          tunnels: tunnels,
          phone: phone,
        ),
      ),
    ),
  );
  await tester.pump();
  final boundary = tester.renderObject<RenderRepaintBoundary>(
    find.byKey(appRootKey),
  );
  final png = await tester.runAsync(() async {
    final image = await boundary.toImage(pixelRatio: _outputScale);
    final data = await image.toByteData(format: ui.ImageByteFormat.png);
    return data!.buffer.asUint8List();
  });
  // Synchronous: real I/O never completes inside the fake-async zone.
  File(copy.output).writeAsBytesSync(png!);
  await tester.pumpWidget(const SizedBox.shrink());
  tester.view.reset();
}

/// The screens are captured as macOS and iOS, whose CJK chain leads with
/// PingFang SC. The test has no such face and would draw some of its glyphs
/// as boxes, so the bundled subset stands in for it.
Future<void> _standInForPingFang() async {
  final loader = FontLoader('PingFang SC');
  for (final weight in ['Regular', 'Medium', 'SemiBold']) {
    loader.addFont(
      rootBundle.load('assets/fonts/NotoSansSC-$weight.subset.ttf'),
    );
  }
  await loader.load();
}

void main() {
  final skip = Platform.environment['PB_MAPPER_REGEN_POSTER'] != '1';

  setUpAll(() async {
    if (skip) return;
    await loadRealFonts();
    await _standInForPingFang();
  });

  for (final copy in [_english, _chinese]) {
    testWidgets(
      'derive the ${copy.locale.languageCode} README poster from the app',
      (tester) async {
        // `flutter test` draws every shadow as a solid shape; a poster wants
        // them as the app draws them.
        debugDisableShadows = false;
        try {
          await _render(tester, copy);
        } finally {
          debugDisableShadows = true;
        }
      },
      skip: skip,
    );
  }
}
