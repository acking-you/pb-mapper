/// The remote desktop as the previews and the README posters show it: a
/// relay with remembered computers sharing, this computer able to share its
/// own, and a session over Office PC showing [RemoteScreenFixture].
library;

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_page.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';
import 'package:pb_mapper_ui/src/state/remote_desktop.dart';

import 'fake_desktop_api.dart';
import 'fake_pb_mapper_api.dart';
import 'fake_remote_computer_vault.dart';
import 'remote_screen_fixture.dart';
import 'support.dart';

/// The relay [FakePbMapperApi] is configured with.
const previewRelayAddress = 'lb7666.top:7666';

/// Four remembered computers: one online and used often, one online over
/// the relay, one not sharing, one saved under another relay. All but the
/// last were saved under [relay]; Office PC was last used at [officeUsed],
/// two hours ago unless given.
FakeRemoteComputerVault previewComputers({
  String relay = previewRelayAddress,
  DateTime? officeUsed,
}) => FakeRemoteComputerVault()
  ..records.addAll([
    RemoteComputer(
      id: '${'a' * 31}1',
      name: 'Office PC',
      service: 'desktop:office-pc',
      fingerprint: 'AB12-CD34-EF56-7890',
      relay: relay,
      namespace: 0,
      lastConnectedAt:
          officeUsed ?? DateTime.now().subtract(const Duration(hours: 2)),
      lastPath: 'direct',
      lastControlAllowed: true,
    ),
    RemoteComputer(
      id: '${'b' * 31}2',
      name: 'Home server',
      service: 'desktop:home-pc',
      fingerprint: 'FFFF-0000-FFFF-0000',
      relay: relay,
      namespace: 0,
      lastConnectedAt: DateTime.now().subtract(const Duration(days: 1)),
      lastPath: 'relay',
    ),
    RemoteComputer(
      id: '${'c' * 31}3',
      name: 'Lab workstation',
      service: 'desktop:lab-ws',
      fingerprint: '1111-2222-3333-4444',
      relay: relay,
      namespace: 0,
      lastConnectedAt: DateTime(2026, 11, 2, 10),
    ),
    const RemoteComputer(
      id: 'dddddddddddddddddddddddddddddd04',
      name: "Parents' laptop",
      service: 'desktop:parents',
      fingerprint: '5555-6666-7777-8888',
      relay: 'other.example:7666',
      namespace: 0,
    ),
  ]);

/// A relay with two of [previewComputers] sharing. This computer can share
/// its own desktop when [host], and a viewer is watching it when [viewed].
FakePbMapperApi previewRemoteApi({bool host = true, bool viewed = false}) {
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
  // A 120 Hz display whose H.265 decoder takes 1440p and whose H.264 one
  // stops at 1080p: Native is out of its reach.
  api.desktop.decodersValue = const DesktopDecoders(
    h264: DesktopDecoderLimit(1920, 1080),
    h265: DesktopDecoderLimit(2560, 1440),
    displayMaxHz: 120,
  );
  return api;
}

/// Lets the fake's picture decode: that takes real time, which a widget
/// test's clock does not pass by itself.
Future<void> decodePreviewPicture(WidgetTester tester) async {
  for (var i = 0; i < 30; i++) {
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 10)),
    );
    await tester.pump(const Duration(milliseconds: 50));
  }
}

/// A direct session's figures: H.265 at 60 FPS over 1080p.
const previewStats = DesktopStats(
  path: 'direct',
  codec: 'h265',
  presentedFps: 60,
  rttMs: 18,
  speedRxBps: 2516582,
  speedTxBps: 18432,
  encoderBitrateBps: 8000000,
  width: 1920,
  height: 1080,
  preset: DesktopQuality.balanced,
  maxFps: 120,
);

/// Opens Office PC from the Remote page. Unless [live] is false, the
/// session goes live with [RemoteScreenFixture] on screen, reporting
/// [stats], in control when the host allows it.
Future<RemoteDesktop> viewPreviewComputer(
  WidgetTester tester,
  FakePbMapperApi api, {
  bool live = true,
  bool controlAllowed = true,
  DesktopStats stats = previewStats,
}) async {
  final png = await RemoteScreenFixture.png(tester);
  final scope = AppScope.of(tester.element(find.byType(RemotePage)));
  await scope.remote.open(scope.store.computers.computers.first);
  await tester.pump(const Duration(milliseconds: 100));
  if (live) {
    api.desktop.viewerStatusValue = liveStatus(
      controlling: controlAllowed,
      controlAllowed: controlAllowed,
      stats: stats,
    );
    await tester.pump(const Duration(milliseconds: 300));
    api.desktop.emitJpeg(png, generation: 1, frameId: 1);
  }
  await decodePreviewPicture(tester);
  return scope.remote;
}

const _windowManager = MethodChannel('window_manager');

/// Runs [body] as Windows, where a computer can share its desktop: the app
/// draws its own caption buttons, and the viewer its dark ones. Its window
/// answers as a restored one.
Future<void> onWindows(
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
