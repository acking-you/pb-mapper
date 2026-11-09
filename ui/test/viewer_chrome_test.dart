import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/desktop_viewer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/minimap.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/modifier_latch.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/quality_panel.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_cursor_layer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_input.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_surface.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/remote_viewport.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/touch_gestures.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/touch_viewer.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_menus.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_overlays.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_stage.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_toolbar.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';

import 'fake_desktop_api.dart';
import 'remote_viewer_harness.dart';

const _target = DesktopTarget(invitation: 'pbd1.office', name: 'Office');

/// [child] as the app shows a viewer: localised, in the viewer's theme.
Widget _app(Widget child) => MaterialApp(
  localizationsDelegates: AppLocalizations.localizationsDelegates,
  supportedLocales: AppLocalizations.supportedLocales,
  home: Builder(
    builder: (context) => Theme(data: viewerTheme(context), child: child),
  ),
);

void _size(WidgetTester tester, Size size) {
  tester.view
    ..physicalSize = size
    ..devicePixelRatio = 1;
  addTearDown(tester.view.reset);
}

/// A 16:9 picture, encoded as PNG: the store decodes it like a JPEG.
Future<Uint8List> _png(WidgetTester tester) async =>
    (await tester.runAsync(() async {
      final recorder = ui.PictureRecorder();
      Canvas(recorder).drawRect(
        const Rect.fromLTWH(0, 0, 320, 180),
        Paint()..color = const Color(0xff26445e),
      );
      final picture = recorder.endRecording();
      final image = await picture.toImage(320, 180);
      final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
      image.dispose();
      picture.dispose();
      return bytes!.buffer.asUint8List();
    }))!;

void main() {
  testWidgets(
    'an unpinned toolbar collapses 2 s after the pointer leaves, never while '
    'a menu is open, and focus returns to the picture when the menu closes',
    (tester) async {
      _size(tester, const Size(1280, 800));
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final demand = store.demand(DesktopDemand.viewer);
      await store.connect(_target);
      api.viewerStatusValue = liveStatus(controlling: true);
      await tester.pump(const Duration(milliseconds: 500));
      final prefs = ViewerPrefs(persist: false);
      await prefs.setToolbarPinned(false);
      await tester.pumpWidget(
        _app(DesktopViewer(desktop: store, prefs: prefs, title: 'Office')),
      );
      await tester.pump();

      final pin = find.byKey(const Key('viewer-pin')).hitTestable();
      final handle = find
          .byKey(const Key('viewer-toolbar-handle'))
          .hitTestable();
      const picture = Offset(640, 600);
      final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await mouse.addPointer(location: tester.getCenter(pin));
      await tester.pump();
      Future<void> click(Finder target) async {
        await mouse.moveTo(tester.getCenter(target));
        await tester.pump();
        await mouse.down(tester.getCenter(target));
        await mouse.up();
        await tester.pump(const Duration(milliseconds: 300));
      }

      // The pointer leaves: two seconds later the toolbar folds away.
      await mouse.moveTo(picture);
      await tester.pump(const Duration(milliseconds: 1900));
      expect(pin, findsOneWidget);
      expect(handle, findsNothing);
      await tester.pump(const Duration(milliseconds: 100));
      expect(pin, findsNothing);
      // The handle slides in.
      await tester.pump(Motion.medium);
      expect(handle, findsOneWidget);

      // Back out, Display opened, and the pointer gone: an open menu holds
      // the toolbar out.
      await click(handle);
      expect(pin, findsOneWidget);
      await click(find.byKey(const Key('viewer-display')));
      expect(find.byKey(const Key('display-fit')), findsOneWidget);
      await mouse.moveTo(picture);
      await tester.pump(const Duration(seconds: 3));
      expect(pin, findsOneWidget);
      expect(find.byKey(const Key('display-fit')), findsOneWidget);

      // Esc closes the menu, never reaching the remote, and the picture
      // has the keyboard again.
      await tester.sendKeyEvent(LogicalKeyboardKey.escape);
      await tester.pump(const Duration(milliseconds: 300));
      expect(find.byKey(const Key('display-fit')), findsNothing);
      String key(Map<String, Object?> e) => ViewerHarness.describe(e);
      expect(api.inputs.map(key), isNot(contains('escape↓')));
      await tester.sendKeyEvent(LogicalKeyboardKey.f5);
      expect(api.inputs.map(key).toList().reversed.take(2), ['f5↑', 'f5↓']);

      // With the menu closed it folds away again; pinned, never.
      await tester.pump(const Duration(seconds: 2));
      await tester.pump(Motion.medium);
      expect(handle, findsOneWidget);
      await click(handle);
      await click(pin);
      expect(prefs.toolbarPinned, isTrue);
      await mouse.moveTo(picture);
      await tester.pump(const Duration(seconds: 3));
      expect(pin, findsOneWidget);

      await mouse.removePointer();
      await tester.pumpWidget(const SizedBox.shrink());
      demand.dispose();
      store.dispose();
      await tester.pump(const Duration(seconds: 1));
    },
  );

  testWidgets('connection overlays follow the phase, stay translucent while a '
      'frame can arrive, and never replace the picture', (tester) async {
    _size(tester, const Size(1280, 800));
    // The input core needs a store; the stage itself reads only the
    // notifiers below.
    final store = DesktopStore(FakeDesktopApi());
    final latch = ModifierLatch();
    final viewport = RemoteViewport();
    final prefs = ViewerPrefs(persist: false);
    final touchMode = ValueNotifier(TouchMode.direct);
    late final TouchGestureMachine machine;
    final input = RemoteInput(
      desktop: store,
      latch: latch,
      onLocalCancel: () => machine.cancel(Duration.zero),
    );
    machine = TouchGestureMachine(
      sink: input,
      viewport: viewport,
      mode: () => touchMode.value,
      interactive: () => input.ready.value,
      haptic: (_) {},
    );
    final session = ValueNotifier(
      const DesktopSessionView(
        target: _target,
        phase: DesktopPhase.connecting,
        setup: DesktopSetupStep.verifying,
      ),
    );
    final picture = ValueNotifier<DesktopPicture?>(
      const DesktopPicture(textureId: 7, aspectRatio: 16 / 9, codec: 'h264'),
    );
    await tester.pumpWidget(
      _app(
        ViewerStage(
          session: session,
          geometry: ValueNotifier(null),
          picture: picture,
          jpegImage: ValueNotifier(null),
          stats: ValueNotifier(DesktopStats.none),
          viewport: viewport,
          input: input,
          machine: machine,
          touchMode: touchMode,
          title: 'Office',
          actions: ViewerActions(
            desktop: store,
            input: input,
            prefs: prefs,
            viewport: viewport,
            onDisconnect: () async {},
            onQuality: (_) {},
            onResolution: (_) {},
            onMaxFps: (_) {},
            onDisplayMode: (_) {},
            onTypeText: () {},
          ),
        ),
      ),
    );
    await tester.pump();
    final texture = tester.element(find.byType(Texture));
    ViewerScrim scrim() => tester.widget<ViewerScrim>(find.byType(ViewerScrim));
    Future<void> enter(DesktopSessionView s) async {
      session.value = s;
      await tester.pump();
      expect(tester.element(find.byType(Texture)), same(texture));
    }

    // The first frame is presented while connecting: nothing opaque on it.
    expect(find.byKey(const Key('viewer-overlay-connecting')), findsOneWidget);
    expect(scrim().opaque, isFalse);

    await enter(
      const DesktopSessionView(
        target: _target,
        phase: DesktopPhase.live,
        hasPicture: true,
        controlAllowed: true,
      ),
    );
    expect(find.byType(ViewerScrim), findsNothing);

    // Lost: the last picture stays, greyed, and the pill offers Retry now.
    await enter(
      DesktopSessionView(
        target: _target,
        phase: DesktopPhase.reconnecting,
        hasPicture: true,
        reconnectAttempt: 1,
        retryAt: DateTime.now().add(const Duration(seconds: 2)),
      ),
    );
    expect(
      find.byKey(const Key('viewer-overlay-retryWaiting')),
      findsOneWidget,
    );
    expect(find.text('Retry now'), findsOneWidget);
    expect(find.byType(ViewerScrim), findsNothing);
    expect(find.byType(BackdropFilter), findsOneWidget);

    await enter(
      const DesktopSessionView(
        target: _target,
        phase: DesktopPhase.waitingForDesktop,
        hasPicture: true,
        secureDesktop: true,
      ),
    );
    expect(
      find.byKey(const Key('viewer-overlay-secureDesktop')),
      findsOneWidget,
    );
    expect(scrim().opaque, isFalse);

    await enter(
      const DesktopSessionView(
        target: _target,
        phase: DesktopPhase.live,
        hasPicture: true,
      ),
    );
    expect(find.byType(ViewerScrim), findsNothing);

    // The session is dead: the identity change covers everything else.
    await enter(
      const DesktopSessionView(
        target: _target,
        phase: DesktopPhase.identityChanged,
        hasPicture: true,
        secureDesktop: true,
        reconnectAttempt: 2,
      ),
    );
    expect(
      find.byKey(const Key('viewer-overlay-identityChanged')),
      findsOneWidget,
    );
    expect(scrim().opaque, isTrue);

    await tester.pumpWidget(const SizedBox.shrink());
    input.dispose();
    machine.dispose();
    viewport.dispose();
    latch.dispose();
    store.dispose();
  });

  testWidgets('the chip and the quality panel show no figures while the '
      'connection is down', (tester) async {
    _size(tester, const Size(1280, 800));
    // The store keeps its last sample through a reconnect.
    final session = ValueNotifier(
      const DesktopSessionView(
        target: _target,
        phase: DesktopPhase.reconnecting,
      ),
    );
    final stats = ValueNotifier(
      const DesktopStats(
        path: 'direct',
        rttMs: 18,
        presentedFps: 60,
        receivedBitrateBps: 8000000,
        speedRxBps: 2516582,
        width: 1920,
        height: 1080,
        codec: 'h264',
      ),
    );
    await tester.pumpWidget(
      _app(
        Column(
          children: [
            ToolbarStatusChip(session: session, stats: stats, onTap: () {}),
            QualityPanel(session: session, stats: stats),
          ],
        ),
      ),
    );
    // Neither the last sample nor zeros.
    expect(
      find.textContaining(RegExp(r'\d (ms|fps|Mbps|[KM]iB/s)')),
      findsNothing,
    );
    expect(find.textContaining(RegExp(r'— ms · +— fps · +—')), findsOneWidget);

    session.value = const DesktopSessionView(
      target: _target,
      phase: DesktopPhase.live,
      path: 'direct',
    );
    await tester.pump();
    expect(
      find.textContaining(RegExp(r'18 ms · +60 fps · +2\.4 MiB/s')),
      findsOneWidget,
    );
    expect(find.text('60 fps'), findsOneWidget);
  });

  testWidgets('the quality panel tells a LAN path, both round trips, the '
      "three frame rates and the decoder's own rates apart, and scrolls on a "
      'landscape phone', (tester) async {
    _size(tester, const Size(851, 393));
    final stats = ValueNotifier(
      DesktopStats.fromJson(
        {
          'path': 'direct',
          'rtt_ms': 37,
          'presented_fps': 58.6,
          'received_fps': 59.4,
          'capture_fps': 60.0,
          'codec': 'h265',
          'link': {'kind': 'lan', 'family': 'ipv4', 'rtt_ms': 4},
        },
        status: {'speed_rx_bps': 2516582, 'speed_tx_bps': 18432},
      ),
    );
    // Two reads of Android's decoder a second apart.
    final decoder = ValueNotifier<DesktopDecoderStats?>(
      DesktopDecoderStats.between(
        {
          'decoded': 100,
          'timings': {
            'surface_consume': {'samples': 100, 'mean_ms': 10.0},
          },
        },
        {
          'decoded': 160,
          'timings': {
            'surface_consume': {'samples': 160, 'mean_ms': 9.25},
          },
        },
        const Duration(seconds: 1),
      ),
    );
    await tester.pumpWidget(
      _app(
        Align(
          alignment: Alignment.topLeft,
          child: SizedBox(
            height: 369,
            child: Align(
              alignment: Alignment.topLeft,
              child: QualityPanel(
                session: ValueNotifier(
                  const DesktopSessionView(
                    target: _target,
                    phase: DesktopPhase.live,
                    path: 'direct',
                  ),
                ),
                stats: stats,
                decoder: decoder,
              ),
            ),
          ),
        ),
      ),
    );
    final l10n = AppLocalizations.of(
      tester.element(find.byType(QualityPanel)),
    )!;
    Finder row(String id, String text) => find.descendant(
      of: find.byKey(Key('quality-$id')),
      matching: find.text(text),
    );
    expect(row('path', '${l10n.qualityPathLan} · IPv4'), findsOneWidget);
    expect(row('link-rtt', '4 ms'), findsOneWidget);
    expect(row('stream-rtt', '37 ms'), findsOneWidget);
    expect(row('speed', '2.4 MiB/s'), findsOneWidget);
    expect(row('speed', l10n.qualitySpeedSent('18 KiB/s')), findsOneWidget);
    expect(row('codec', 'H.265'), findsOneWidget);
    expect(row('chroma', '4:2:0'), findsOneWidget);
    expect(row('presented-fps', '59 fps'), findsOneWidget);
    expect(row('received-fps', '59 fps'), findsOneWidget);
    expect(row('capture-fps', '60 fps'), findsOneWidget);
    expect(row('decoded', '60 /s'), findsOneWidget);
    expect(row('surface', '8.0 ms'), findsOneWidget);
    // What the host and the decoder do not report is left out.
    expect(find.byKey(const Key('quality-sent-bitrate')), findsNothing);
    expect(find.byKey(const Key('quality-credits')), findsNothing);
    expect(
      tester.getSize(find.byType(QualityPanel)).height,
      lessThanOrEqualTo(369),
    );
    expect(tester.takeException(), isNull);
  });

  testWidgets('the chip keeps its width as the figures change', (tester) async {
    // The toolbar is centred around the chip: a wider figure would move
    // every button.
    final stats = ValueNotifier(
      const DesktopStats(path: 'direct', rttMs: 8, presentedFps: 4),
    );
    await tester.pumpWidget(
      _app(
        Center(
          child: ToolbarStatusChip(
            key: const Key('viewer-quality-chip'),
            session: ValueNotifier(
              const DesktopSessionView(
                target: _target,
                phase: DesktopPhase.live,
                path: 'direct',
              ),
            ),
            stats: stats,
            onTap: () {},
          ),
        ),
      ),
    );
    final chip = find.byKey(const Key('viewer-quality-chip'));
    final width = tester.getSize(chip).width;

    stats.value = const DesktopStats(
      path: 'direct',
      rttMs: 128,
      presentedFps: 120,
    );
    await tester.pump();
    expect(tester.getSize(chip).width, width);
  });

  testWidgets(
    "the remote resolution is a desktop runner's, shown only while this "
    'viewer controls a host offering more than one mode; Fit local changes '
    'the remote display live',
    (tester) async {
      _size(tester, const Size(1280, 800));
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final demand = store.demand(DesktopDemand.viewer);
      await store.connect(_target);
      const modes = DisplayModeList(
        current: DisplayMode(2560, 1440),
        original: DisplayMode(2560, 1440),
        supported: [DisplayMode(2560, 1440), DisplayMode(1920, 1080)],
        fitLocal: DisplayMode(1920, 1080),
      );
      final submenu = find.byKey(const Key('display-remote-resolution'));
      Future<bool> offered(
        TargetPlatform platform, {
        required bool controlling,
      }) async {
        debugDefaultTargetPlatformOverride = platform;
        api.viewerStatusValue = liveStatus(
          controlling: controlling,
          modes: modes,
        );
        await tester.pump(const Duration(milliseconds: 500));
        await tester.pumpWidget(
          _app(
            DesktopViewer(
              desktop: store,
              prefs: ViewerPrefs(persist: false),
              title: 'Office',
            ),
          ),
        );
        await tester.pump();
        await tester.tap(find.byKey(const Key('viewer-display')));
        await tester.pump(const Duration(milliseconds: 300));
        final shown = submenu.evaluate().isNotEmpty;
        if (!shown) {
          await tester.sendKeyEvent(LogicalKeyboardKey.escape);
          await tester.pump(const Duration(milliseconds: 300));
        }
        return shown;
      }

      expect(await offered(TargetPlatform.android, controlling: true), isFalse);
      expect(
        await offered(TargetPlatform.windows, controlling: false),
        isFalse,
      );
      expect(await offered(TargetPlatform.windows, controlling: true), isTrue);
      final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await mouse.addPointer(location: tester.getCenter(submenu));
      await mouse.moveBy(const Offset(4, 0));
      await tester.pump(const Duration(milliseconds: 400));
      await tester.tap(find.byKey(const Key('remote-resolution-fit')));
      await tester.pump();
      expect(api.argsOf('display_mode').single['target'], {
        'fit_local': store.localDisplay!.toJson(),
      });
      expect(api.argsOf('connect'), hasLength(1));

      debugDefaultTargetPlatformOverride = null;
      await mouse.removePointer();
      await tester.pumpWidget(const SizedBox.shrink());
      demand.dispose();
      store.dispose();
      await tester.pump(const Duration(seconds: 1));
    },
  );

  testWidgets(
    'the pointer maps to display pixels and nothing is sent unless input is '
    'live',
    (tester) async {
      _size(tester, const Size(1280, 800));
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final demand = store.demand(DesktopDemand.viewer);
      final png = await _png(tester);
      await tester.pumpWidget(
        _app(
          DesktopViewer(
            desktop: store,
            prefs: ViewerPrefs(persist: false),
            title: 'Office',
          ),
        ),
      );
      await store.connect(_target);
      // The host lets this viewer watch only.
      api.viewerStatusValue = liveStatus(generation: 3, controlAllowed: false);
      api.emitJpeg(png, generation: 3, frameId: 1);
      for (var i = 0; i < 20 && find.byType(RawImage).evaluate().isEmpty; i++) {
        await tester.runAsync(
          () => Future<void>.delayed(const Duration(milliseconds: 10)),
        );
        await tester.pump();
      }
      await tester.pump(const Duration(milliseconds: 500));
      expect(find.byType(RawImage), findsOneWidget);
      expect(api.argsOf('presented'), [
        {'consumer': 1, 'generation': 3, 'frame_id': 1},
      ]);
      expect(find.byKey(const Key('viewer-view-only')), findsOneWidget);
      final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await mouse.addPointer(location: Offset.zero);
      await mouse.moveTo(tester.getCenter(find.byType(RawImage)));
      await tester.tap(find.byType(RawImage));
      await tester.pump();
      expect(api.inputs, isEmpty);

      // Allowed: control is the viewer's to ask for.
      api.viewerStatusValue = liveStatus(generation: 3);
      await tester.pump(const Duration(milliseconds: 500));
      await tester.tap(
        find.descendant(
          of: find.byKey(const Key('viewer-mode')),
          matching: find.text('Control'),
        ),
      );
      await tester.pump(const Duration(milliseconds: 500));
      expect(store.session.value.inputLive, isTrue);
      await tester.tap(find.byType(RawImage));
      await tester.pump();
      expect(api.inputs.map((e) => e['kind']), ['move', 'button', 'button']);
      expect(api.inputs.first['x'], 960);
      expect(api.inputs.first['y'], 540);
      expect(api.inputs.last['down'], false);
      final viewer = tester.getRect(find.byType(RawImage));
      await mouse.moveTo(viewer.topLeft + viewer.size.center(Offset.zero) / 2);
      await tester.pump(const Duration(milliseconds: 5));
      expect(api.inputs.length, 4);
      expect(api.inputs.last, containsPair('kind', 'move'));
      expect(api.inputs.last['x'], 480);
      expect(api.inputs.last['y'], 270);

      // A stale picture takes no input.
      api.viewerStatusValue = liveStatus(
        generation: 3,
        controlling: true,
        fresh: false,
      );
      await tester.pump(const Duration(milliseconds: 500));
      await mouse.moveTo(viewer.center);
      await tester.tap(find.byType(RawImage), warnIfMissed: false);
      await tester.pump(const Duration(milliseconds: 5));
      expect(api.inputs.length, 4);
      await mouse.removePointer();
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox.shrink());
      demand.dispose();
      store.dispose();
      await tester.pump(const Duration(seconds: 1));
    },
  );

  testWidgets(
    'the soft keyboard only occludes the touch viewer, and a modifier '
    'latched on its bar wraps the next typed key',
    (tester) async {
      _size(tester, const Size(844, 390));
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final demand = store.demand(DesktopDemand.viewer);
      await store.connect(_target);
      api.viewerStatusValue = liveStatus(controlling: true);
      await tester.pump(const Duration(milliseconds: 500));
      final prefs = ViewerPrefs(persist: false);
      await prefs.setGestureSheetSeen(true);
      await tester.pumpWidget(
        _app(
          TouchViewer(
            desktop: store,
            prefs: prefs,
            title: 'Office',
            onBack: () {},
          ),
        ),
      );
      await tester.pump();
      final scene = find.byKey(const Key('remote-scene-transform'));
      final picture = tester.element(scene);
      Matrix4 transform() => tester.widget<Transform>(scene).transform;
      final before = transform();
      List<String> sent() => [
        for (final e in api.argsOf('input').last['inputs']! as List)
          ViewerHarness.describe(e as Map),
      ];

      // The keyboard and the bar cover the picture; nothing relayouts.
      await tester.tap(find.byKey(const Key('touch-keyboard')));
      tester.view.viewInsets = const FakeViewPadding(bottom: 200);
      await tester.pump();
      await tester.pump(kKeyboardSettle);
      expect(find.byKey(const Key('modifier-bar')), findsOneWidget);
      expect(find.byKey(const Key('touch-keyboard')), findsNothing);
      expect(tester.element(scene), same(picture));
      expect(transform().getMaxScaleOnAxis(), before.getMaxScaleOnAxis());

      // Ctrl armed on the bar applies to the next key the IME commits.
      await tester.tap(find.byKey(const Key('modifier-control')));
      await tester.pump();
      final pill = find.descendant(
        of: find.byKey(const Key('modifier-pill')),
        matching: find.byType(ViewerPill),
      );
      expect(pill, findsOneWidget);
      tester.testTextInput.updateEditingValue(
        TextEditingValue(
          text: '${'1' * 64}a',
          selection: const TextSelection.collapsed(offset: 65),
        ),
      );
      await tester.pump();
      expect(sent(), ['control↓', 'a↓', 'a↑', 'control↑']);
      expect(pill, findsNothing);

      // The keyboard goes: the gutters return and the view is as it was.
      await tester.tap(find.byKey(const Key('modifier-collapse')));
      tester.view.resetViewInsets();
      await tester.pump();
      expect(find.byKey(const Key('touch-keyboard')), findsOneWidget);
      expect(tester.element(scene), same(picture));
      expect(transform(), before);
      expect(tester.takeException(), isNull);

      await tester.pumpWidget(const SizedBox.shrink());
      demand.dispose();
      store.dispose();
      await tester.pump(const Duration(seconds: 1));
    },
  );

  testWidgets(
    'the virtual mouse follows its grip however many moves a frame brings',
    (tester) async {
      _size(tester, const Size(844, 390));
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final demand = store.demand(DesktopDemand.viewer);
      await store.connect(_target);
      api.viewerStatusValue = liveStatus(controlling: true);
      await tester.pump(const Duration(milliseconds: 500));
      final prefs = ViewerPrefs(persist: false);
      await prefs.setGestureSheetSeen(true);
      await prefs.setTouchMode(TouchMode.direct);
      await prefs.setVirtualMouse(true);
      await tester.pumpWidget(
        _app(
          TouchViewer(
            desktop: store,
            prefs: prefs,
            title: 'Office',
            onBack: () {},
          ),
        ),
      );
      await tester.pump();
      final pad = find.byKey(const Key('virtual-mouse'));
      final before = tester.getTopLeft(pad);

      // Two moves on the grip before the next frame: both count.
      final grip = await tester.startGesture(
        tester.getBottomLeft(pad) + const Offset(36, -14),
      );
      await grip.moveBy(const Offset(-10, -10));
      await grip.moveBy(const Offset(-10, -10));
      await tester.pump();
      expect(tester.getTopLeft(pad), before + const Offset(-20, -20));
      await grip.up();
      expect(tester.takeException(), isNull);

      await tester.pumpWidget(const SizedBox.shrink());
      demand.dispose();
      store.dispose();
      await tester.pump(const Duration(seconds: 1));
    },
  );

  testWidgets(
    "a phone's More panel keeps Disconnect in reach without scrolling",
    (tester) async {
      _size(tester, const Size(844, 390));
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final demand = store.demand(DesktopDemand.viewer);
      await store.connect(_target);
      api.viewerStatusValue = liveStatus(controlling: true);
      await tester.pump(const Duration(milliseconds: 500));
      final prefs = ViewerPrefs(persist: false);
      await prefs.setGestureSheetSeen(true);
      var disconnected = 0;
      await tester.pumpWidget(
        _app(
          TouchViewer(
            desktop: store,
            prefs: prefs,
            title: 'Office',
            onBack: () {},
            onDisconnect: () async => disconnected++,
          ),
        ),
      );
      await tester.pump();

      await tester.tap(find.byKey(const Key('touch-more')));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      final disconnect = find.byKey(const Key('more-disconnect')).hitTestable();
      expect(disconnect, findsOneWidget);
      await tester.tap(disconnect);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(disconnected, 1);
      expect(tester.takeException(), isNull);

      await tester.pumpWidget(const SizedBox.shrink());
      demand.dispose();
      store.dispose();
      await tester.pump(const Duration(seconds: 1));
    },
  );

  testWidgets(
    'beside a cutout the picture fits between the gutters, zoomed in it pans '
    "clear of them, and a finger's ring is drawn over their strips",
    (tester) async {
      _size(tester, const Size(851, 393));
      tester.view.padding = const FakeViewPadding(left: 29);
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final demand = store.demand(DesktopDemand.viewer);
      await store.connect(_target);
      api.viewerStatusValue = liveStatus(controlling: true);
      await tester.pump(const Duration(milliseconds: 500));
      final prefs = ViewerPrefs(persist: false);
      await prefs.setGestureSheetSeen(true);
      await tester.pumpWidget(
        _app(
          TouchViewer(
            desktop: store,
            prefs: prefs,
            title: 'Office',
            onBack: () {},
          ),
        ),
      );
      await tester.pump(kHintDuration);
      final viewport = tester
          .widget<RemoteSurface>(find.byType(RemoteSurface))
          .viewport;
      Rect picture() => Rect.fromPoints(
        viewport.toLocal(Offset.zero),
        viewport.toLocal(const Offset(1, 1)),
      );

      // Fitted, the picture sits between the controls.
      for (final control in [
        'touch-back',
        'touch-quality',
        'touch-mode-toggle',
        'touch-keyboard',
        'touch-shortcuts',
        'touch-right-click',
        'touch-more',
      ]) {
        final rect = tester.getRect(find.byKey(Key(control)));
        expect(
          rect.deflate(0.01).overlaps(picture()),
          isFalse,
          reason: control,
        );
      }

      // Pinched in, then pushed right as far as it goes: its left edge
      // comes to rest at the left strip's, not under it.
      final centre = picture().center;
      final a = await tester.startGesture(centre - const Offset(30, 0));
      final b = await tester.startGesture(centre + const Offset(30, 0));
      await tester.pump(kNavigateStart);
      for (var i = 0; i < 5; i++) {
        await a.moveBy(const Offset(-15, 0));
        await b.moveBy(const Offset(15, 0));
        await tester.pump(const Duration(milliseconds: 16));
      }
      expect(viewport.scale, greaterThan(2));
      for (var i = 0; i < 10; i++) {
        await a.moveBy(const Offset(90, 0));
        await b.moveBy(const Offset(90, 0));
        await tester.pump(const Duration(milliseconds: 16));
      }
      await a.up();
      await b.up();
      final strip = tester.getRect(find.byKey(const Key('touch-gutter-lead')));
      expect(strip.left, 0);
      expect(picture().left, moreOrLessEquals(strip.right));
      expect(api.argsOf('input'), isEmpty);

      // A finger on the strip, between two controls: its ring is there, and
      // drawn after the strip, so over it.
      await tester.pump(const Duration(seconds: 1));
      final back = tester.getRect(find.byKey(const Key('touch-back')));
      final quality = tester.getRect(find.byKey(const Key('touch-quality')));
      final on = Offset(strip.center.dx, (back.bottom + quality.top) / 2);
      final finger = await tester.startGesture(on);
      await tester.pump();
      final ring = tester.widget<TouchIndicator>(find.byType(TouchIndicator));
      expect(ring.machine.indicatorPoint.value, on);
      final painted = find
          .byWidgetPredicate(
            (w) =>
                w is TouchIndicator || w.key == const Key('touch-gutter-lead'),
          )
          .evaluate()
          .map((e) => e.widget is TouchIndicator ? 'ring' : 'strip');
      expect(painted, ['strip', 'ring']);
      await finger.up();
      expect(tester.takeException(), isNull);

      await tester.pumpWidget(const SizedBox.shrink());
      demand.dispose();
      store.dispose();
      await tester.pump(const Duration(seconds: 1));
    },
  );

  testWidgets(
    "a tablet's rotation keeps the zoom and the point at the centre, though "
    'the gutters become bars, however short',
    (tester) async {
      _size(tester, const Size(1280, 800));
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final demand = store.demand(DesktopDemand.viewer);
      await store.connect(_target);
      api.viewerStatusValue = liveStatus(controlling: true);
      await tester.pump(const Duration(milliseconds: 500));
      final prefs = ViewerPrefs(persist: false);
      await prefs.setGestureSheetSeen(true);
      await tester.pumpWidget(
        _app(
          TouchViewer(
            desktop: store,
            prefs: prefs,
            title: 'Office',
            onBack: () {},
          ),
        ),
      );
      await tester.pump(kHintDuration);
      final viewport = tester
          .widget<RemoteSurface>(find.byType(RemoteSurface))
          .viewport;
      Offset centre() =>
          viewport.toNormalized(viewport.visibleRect.center, clamp: true)!;
      viewport
        ..zoomAt(viewport.visibleRect.center, 3)
        ..panBy(const Offset(-200, -100));
      await tester.pump();
      final before = centre();

      for (final (size, framed) in [
        (const Size(800, 1280), true),
        // Bars no taller than the quality button: it shrinks to fit them.
        (const Size(1200, 830), true),
        (const Size(1280, 800), false),
      ]) {
        tester.view.physicalSize = size;
        await tester.pump();
        expect(viewport.frame.top > 0, framed, reason: '$size');
        expect(viewport.scale, 3, reason: '$size');
        expect(centre().dx, moreOrLessEquals(before.dx), reason: '$size');
        expect(centre().dy, moreOrLessEquals(before.dy), reason: '$size');
      }
      expect(tester.takeException(), isNull);

      await tester.pumpWidget(const SizedBox.shrink());
      demand.dispose();
      store.dispose();
      await tester.pump(const Duration(seconds: 1));
    },
  );

  testWidgets(
    'a trackpad stroke repaints the cursor and the ring alone, and a pan the '
    'stage and the minimap alone',
    (tester) async {
      _size(tester, const Size(844, 390));
      final api = FakeDesktopApi();
      final store = DesktopStore(api);
      final demand = store.demand(DesktopDemand.viewer);
      await store.connect(_target);
      api.viewerStatusValue = liveStatus(controlling: true);
      await tester.pump(const Duration(milliseconds: 500));
      final prefs = ViewerPrefs(persist: false);
      await prefs.setGestureSheetSeen(true);
      await tester.pumpWidget(
        _app(
          TouchViewer(
            desktop: store,
            prefs: prefs,
            title: 'Office',
            onBack: () {},
          ),
        ),
      );
      await tester.pump(kHintDuration);
      final centre = tester.getCenter(
        find.byKey(const Key('remote-scene-transform')),
      );

      /// While [moves] run, frame by frame, what paints lies under [owners]
      /// and nowhere else.
      Future<void> repaintsOnly(
        List<Type> owners,
        Future<void> Function() moves,
      ) async {
        var inside = 0;
        final outside = <String>{};
        debugOnProfilePaint = (o) {
          final element = (o.debugCreator! as DebugCreator).element;
          var owned = owners.contains(element.widget.runtimeType);
          element.visitAncestorElements((e) {
            owned |= owners.contains(e.widget.runtimeType);
            return !owned;
          });
          if (owned) {
            inside++;
          } else {
            outside.add('${element.widget.runtimeType}');
          }
        };
        try {
          await moves();
        } finally {
          debugOnProfilePaint = null;
        }
        expect(inside, isPositive);
        expect(outside, isEmpty);
      }

      // Pinched in: the minimap shows. Two fingers pan on.
      final a = await tester.startGesture(centre - const Offset(30, 0));
      final b = await tester.startGesture(centre + const Offset(30, 0));
      await tester.pump(kNavigateStart);
      for (var i = 0; i < 5; i++) {
        await a.moveBy(const Offset(-15, 0));
        await b.moveBy(const Offset(15, 0));
        await tester.pump(const Duration(milliseconds: 16));
      }
      expect(find.byKey(const Key('minimap')), findsOneWidget);
      await repaintsOnly([ViewerStage, Minimap], () async {
        for (var i = 0; i < 3; i++) {
          await a.moveBy(const Offset(4, 3));
          await b.moveBy(const Offset(4, 3));
          await tester.pump();
        }
      });
      await a.up();
      await b.up();
      await tester.pump(const Duration(seconds: 1));

      // One finger strokes the cursor, with the ring under it.
      final finger = await tester.startGesture(centre);
      await tester.pump(const Duration(milliseconds: 16));
      await finger.moveBy(const Offset(20, 0));
      await tester.pump(const Duration(milliseconds: 16));
      await repaintsOnly([RemoteCursorLayer, TouchIndicator], () async {
        for (var i = 0; i < 3; i++) {
          await finger.moveBy(const Offset(3, 2));
          await tester.pump();
        }
      });
      await finger.up();
      await tester.pump(const Duration(seconds: 1));

      await tester.pumpWidget(const SizedBox.shrink());
      demand.dispose();
      store.dispose();
      await tester.pump(const Duration(seconds: 1));
    },
  );
}
