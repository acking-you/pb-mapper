/// Derives the README posters from the app itself, rendered with the bundled
/// fonts and fixture data: in front, the remote desktop viewer in control of
/// another computer, with the quality monitor open; behind it, the galaxy of
/// this computer, its relay and its tunnels; on a phone, the Remote tab's
/// remembered computers. Beside them: the mark, the name and what pb-mapper
/// and its SDK do.
///
/// The poster is about every device, so its words name no operating system
/// and neither does the remote screen ([RemoteScreenFixture]). The three
/// screens are captured as three platforms: the galaxy as macOS, the viewer
/// as Windows (where its H.265 figures are true) and the phone as Android.
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
import 'package:pb_mapper_ui/src/pages/remote/invite_scanner.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_page.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_title_bar.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/ffi/remote_computer_vault.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/widgets/brand_mark.dart';

import 'fake_pb_mapper_api.dart';
import 'remote_preview.dart';
import 'remote_screen_fixture.dart';
import 'support.dart';

/// The poster's layout size; it is written at [_outputScale] times this.
const _poster = Size(1536, 1024);
const _outputScale = 1.25;

/// Screens are captured at twice their logical size, so they stay sharp once
/// placed in the poster.
const _captureScale = 2.0;

/// The desktop screens' logical size, and the phone's. The viewer is as tall
/// as its title bar and the remote picture at its width, so the picture fills
/// it; the galaxy's window is shorter, so its stars clear the window in front.
const _windowWidth = 1040.0;
final _viewer = Size(
  _windowWidth,
  kViewerTitleBarHeight +
      _windowWidth *
          RemoteScreenFixture.size.height /
          RemoteScreenFixture.size.width,
);
const _galaxy = Size(_windowWidth, 600);
const _phone = Size(393, 852);

/// The Android status bar's height on the phone, in its logical pixels.
const _statusBar = 32.0;

/// Where the screens sit in the poster. Windows keep their screens'
/// proportions; the phone's frame adds its bezel around the screen's, and
/// its top clears the viewer's toolbar.
const _back = Rect.fromLTWH(890, 62, 600, 346.2);
final _front = Rect.fromLTWH(
  640,
  306,
  850,
  850 * _viewer.height / _viewer.width,
);
const _bezel = 9.0;
const _phoneWidth = 262.0;
final _phoneRect = Rect.fromLTWH(
  566,
  398,
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
  headline: 'Every private service,\nevery device,\none click away.',
  // Broken by hand, so that no line ends on a stranded word.
  body:
      'Share a TCP or UDP service, or a whole desktop,\n'
      'and open it from any device: direct over QUIC\n'
      'when it can, through your relay when it cannot.\n'
      'Hardware H.265/H.264, real keyboard and mouse.',
  second: 'All of it, in the SDK.',
  secondBody:
      'Tunnels, the picture and its codec data, keyboard and mouse, status '
      'streams and automatic recovery: the app runs on the Rust SDK, and so '
      'can yours.',
  // Two rows of three, paired by column: the paths, the picture, the SDK
  // and the hands on it.
  chips: [
    (AppIcons.direct, 'QUIC direct'),
    (AppIcons.remote, 'Remote desktop'),
    (LucideIcons.code400, 'Rust & Node SDKs'),
    (AppIcons.relay, 'Relay fallback'),
    (LucideIcons.film400, 'H.265 / H.264'),
    (AppIcons.keyboard, 'Keyboard & mouse'),
  ],
  output: '../docs/assets/poster.png',
);

const _chinese = _Copy(
  locale: Locale('zh'),
  motto: [('P2P 直连', true), ('优先，', false), ('中继', true), ('兜底。', false)],
  headline: '每个私有服务，\n每台设备，\n一键直达。',
  // Broken by hand: Chinese wraps anywhere, and left alone it strands the
  // last character of a sentence on a line of its own.
  body:
      '共享一项 TCP / UDP 服务，或整个桌面，\n'
      '在任何设备上打开：能直连就走 QUIC，\n'
      '打不通就走你自己的中继。\n'
      '画面硬件 H.265/H.264 编码，键盘鼠标真实操控。',
  second: '这一切，都在 SDK 里。',
  secondBody:
      '隧道、画面与编解码数据、键盘鼠标、\n'
      '状态流和自动恢复：应用就建在 Rust SDK\n'
      '之上，你的程序也可以。',
  chips: [
    (AppIcons.direct, 'QUIC 直连'),
    (AppIcons.remote, '远程桌面'),
    (LucideIcons.code400, 'Rust 与 Node SDK'),
    (AppIcons.relay, '中继兜底'),
    (LucideIcons.film400, 'H.265 / H.264'),
    (AppIcons.keyboard, '键盘鼠标'),
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
      serviceKey: 'wiki',
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
    'wiki',
    'office-rdp',
    'lab-db',
    'build-cache',
    'media-server',
  ],
  conns: {
    'office-rdp': [fakeConn(connId: 41), fakeConn(connId: 42)],
  },
);

/// The relay the remote desktop's shots name: the galaxy's.
const _relay = 'relay.example.com:7666';

/// [previewRemoteApi] against [_relay], on a device that views and does not
/// share.
FakePbMapperApi _remoteApi() {
  final api = previewRemoteApi(host: false);
  api.config = const ConfigStatus(
    serverAddress: _relay,
    keepAliveEnabled: true,
    msgHeaderKey: '',
  );
  return api;
}

/// A direct session's figures: H.265 at 60 FPS over 1080p, within the
/// Balanced preset's 4–8 Mbit/s in `docs/remote-desktop.md`.
const _stats = DesktopStats(
  path: 'direct',
  codec: 'h265',
  presentedFps: 60,
  receivedFps: 60,
  captureFps: 60,
  rttMs: 11,
  speedRxBps: 590000,
  speedTxBps: 6100,
  encoderBitrateBps: 4600000,
  receivedBitrateBps: 4700000,
  width: 1920,
  height: 1080,
  preset: DesktopQuality.balanced,
  maxFps: 60,
);

/// The app on [platform] at [size], on [page], after [arrange]: by default
/// against [_api], otherwise against [api] with [vault] and [prefs].
Future<ui.Image> _shot(
  WidgetTester tester, {
  required _Copy copy,
  required TargetPlatform platform,
  required Size size,
  required AppPage page,
  FakePbMapperApi? api,
  RemoteComputerVault? vault,
  ViewerPrefs? prefs,
  ThemeMode theme = ThemeMode.light,
  EdgeInsets safeArea = EdgeInsets.zero,
  Future<void> Function(WidgetTester tester, FakePbMapperApi api)? arrange,
}) async {
  Future<ui.Image> shoot() async {
    final app = api ?? _api(copy.locale);
    tester.view.padding = tester.view.viewPadding = FakeViewPadding(
      top: safeArea.top,
      bottom: safeArea.bottom,
    );
    await pumpApp(
      tester,
      app,
      page: page,
      size: size,
      themeMode: theme,
      locale: copy.locale,
      vault: vault,
      viewerPrefs: prefs,
    );
    if (arrange != null) {
      await arrange(tester, app);
      await tester.pump();
    }
    final boundary = tester.renderObject<RenderRepaintBoundary>(
      find.byKey(appRootKey),
    );
    final image = await tester.runAsync(
      () => boundary.toImage(pixelRatio: _captureScale),
    );
    await finish(tester);
    return image!;
  }

  try {
    if (platform == TargetPlatform.windows) {
      late ui.Image image;
      await onWindows(tester, () async => image = await shoot());
      return image;
    }
    debugDefaultTargetPlatformOverride = platform;
    return await shoot();
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

/// A desktop window: the captured screen with a hairline edge and, when
/// [lights], the macOS window controls in the strip the rail leaves for
/// them. A Windows screen draws its own caption buttons.
class _Window extends StatelessWidget {
  const _Window(this.screen, {this.lights = true});

  final ui.Image screen;
  final bool lights;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final scale = constraints.maxWidth / _windowWidth;
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
                if (lights)
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

/// The phone, an Android one as captured: the screen in an ink frame, with
/// a status bar around its punch-hole camera.
class _Phone extends StatelessWidget {
  const _Phone(this.screen);

  final ui.Image screen;

  @override
  Widget build(BuildContext context) {
    const radius = 36.0;
    final bar = (_phoneWidth - 2 * _bezel) / _phone.width * _statusBar;
    Widget status(IconData icon) => Padding(
      padding: const EdgeInsets.only(left: 3),
      child: Icon(icon, size: 11, color: _ink),
    );
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
                top: 0,
                left: 0,
                right: 0,
                height: bar,
                child: Stack(
                  alignment: Alignment.center,
                  children: [
                    Row(
                      children: [
                        const SizedBox(width: 18),
                        Text(
                          '9:30',
                          style: _style(11, weight: FontWeight.w500, height: 1),
                        ),
                        const Spacer(),
                        status(LucideIcons.wifi400),
                        status(LucideIcons.signal400),
                        status(LucideIcons.batteryFull400),
                        const SizedBox(width: 16),
                      ],
                    ),
                    Container(
                      width: 10,
                      height: 10,
                      decoration: BoxDecoration(
                        color: _ink,
                        shape: BoxShape.circle,
                      ),
                    ),
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
      padding: const EdgeInsets.symmetric(horizontal: 10),
      decoration: BoxDecoration(
        color: Colors.white,
        borderRadius: BorderRadius.circular(17),
        border: Border.all(color: _hairline),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: AppIconSize.inline, color: _signal),
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
    required this.viewer,
    required this.galaxy,
    required this.phone,
  });

  final _Copy copy;
  final ui.Image viewer;
  final ui.Image galaxy;
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
          // Behind: the tunnels, as a galaxy; in front: another computer,
          // in control; on the phone, the computers it views.
          Positioned.fromRect(rect: _back, child: _Window(galaxy)),
          Positioned.fromRect(
            rect: _front,
            child: _Window(viewer, lights: false),
          ),
          Positioned.fromRect(rect: _phoneRect, child: _Phone(phone)),
        ],
      ),
    );
  }
}

/// Gives Office PC, the featured computer, its last picture, as a session
/// leaves it behind, and lets it decode.
Future<void> _rememberPicture(WidgetTester tester) async {
  final computers = AppScope.of(
    tester.element(find.byType(RemotePage)),
  ).store.computers;
  final id = computers.computers.first.id;
  final png = await RemoteScreenFixture.png(tester);
  computers.thumbnailFile(id)!.writeAsBytesSync(png);
  computers.thumbnailWritten(id);
  await decodePreviewPicture(tester);
}

DateTime _thisMorning() {
  final now = DateTime.now();
  return DateTime(now.year, now.month, now.day, 9, 12);
}

Future<void> _render(WidgetTester tester, _Copy copy) async {
  // The viewer: Windows, where H.265 is decoded in hardware.
  final monitor = ViewerPrefs(persist: false);
  await monitor.setQualityMonitor(true);
  final viewer = await _shot(
    tester,
    copy: copy,
    platform: TargetPlatform.windows,
    size: _viewer,
    page: AppPage.remote,
    api: _remoteApi(),
    vault: previewComputers(relay: _relay),
    prefs: monitor,
    arrange: (tester, api) async {
      await viewPreviewComputer(tester, api, stats: _stats);
      // Past the toolbar's entrance and the monitor's first sample.
      await tester.pump(const Duration(seconds: 2));
    },
  );
  final galaxy = await _shot(
    tester,
    copy: copy,
    platform: TargetPlatform.macOS,
    size: _galaxy,
    page: AppPage.overview,
    theme: ThemeMode.dark,
  );
  // The phone: Android, which views and remembers computers as Windows
  // does, and scans an invitation's QR code.
  InviteScanner.debugSupportedOverride = true;
  final phone = await _shot(
    tester,
    copy: copy,
    platform: TargetPlatform.android,
    size: _phone,
    page: AppPage.remote,
    api: _remoteApi(),
    // Last used this morning, before the phone's 9:30.
    vault: previewComputers(relay: _relay, officeUsed: _thisMorning()),
    safeArea: const EdgeInsets.only(top: _statusBar, bottom: 28),
    arrange: (tester, _) => _rememberPicture(tester),
  ).whenComplete(() => InviteScanner.debugSupportedOverride = null);

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
          viewer: viewer,
          galaxy: galaxy,
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

/// The galaxy is captured as macOS, whose CJK chain leads with PingFang SC.
/// The test has no such face and would draw some of its glyphs as boxes, so
/// the bundled subset stands in for it.
Future<void> _standInForPingFang() async {
  final loader = FontLoader('PingFang SC');
  for (final weight in ['Regular', 'Medium', 'SemiBold']) {
    loader.addFont(
      rootBundle.load('assets/fonts/NotoSansSC-$weight.subset.ttf'),
    );
  }
  await loader.load();
}

/// The viewer is captured as Windows, whose face is Segoe UI. No machine
/// but a Windows one has it, so the poster's own face stands in, the same
/// on every machine.
Future<void> _standInForSegoeUi() async {
  final loader = FontLoader('Segoe UI');
  for (final weight in ['Regular', 'Medium', 'SemiBold', 'Bold']) {
    loader.addFont(rootBundle.load('assets/fonts/GoogleSansFlex-$weight.ttf'));
  }
  await loader.load();
}

void main() {
  final skip = Platform.environment['PB_MAPPER_REGEN_POSTER'] != '1';

  setUpAll(() async {
    if (skip) return;
    await loadRealFonts();
    await _standInForPingFang();
    await _standInForSegoeUi();
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
