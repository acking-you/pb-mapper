import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:pb_mapper_ui/l10n/app_localizations.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/pages/remote/invite_scanner.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_page.dart';
import 'package:pb_mapper_ui/src/pages/remote/viewer/viewer_screen.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';
import 'package:pb_mapper_ui/src/state/viewer_prefs.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';

import 'fake_desktop_api.dart';
import 'fake_pb_mapper_api.dart';
import 'fake_remote_computer_vault.dart';
import 'support.dart';

/// A host whose permission change can be held in flight: the store is busy
/// with it.
class _HostDesktop extends FakeDesktopApi {
  Completer<void>? holdPermission;

  @override
  Future<void> allowControl(bool allow) async {
    await holdPermission?.future;
    return super.allowControl(allow);
  }
}

class _Api extends FakePbMapperApi {
  final _host = _HostDesktop();

  @override
  _HostDesktop get desktop => _host;
}

/// A remembered grant Windows cannot unlock: every read of it fails with
/// `PROTOCOL`, as `trust_status` does, until it is revoked.
class _UnreadableTrust extends FakeDesktopApi {
  bool unreadable = true;

  @override
  Future<DesktopTrust> trustStatus() async {
    if (!unreadable) return super.trustStatus();
    calls.add(('trust_status', const {}));
    throw const DesktopCallException(
      'PROTOCOL',
      'Windows could not unlock desktop trust for this user; revoke it and '
          'authorize a new invitation',
    );
  }

  @override
  Future<void> revokeTrust() async {
    unreadable = false;
    await super.revokeTrust();
  }
}

class _UnreadableApi extends FakePbMapperApi {
  final _host = _UnreadableTrust();

  @override
  _UnreadableTrust get desktop => _host;
}

/// A host that can be re-keyed: its invitations name [fingerprint].
class _RekeyedHost extends FakeDesktopApi {
  String fingerprint = 'AB12-CD34-EF56-7890';

  @override
  Future<DesktopInvitationInfo> inspectInvitation(String text) async {
    final info = await super.inspectInvitation(text);
    return DesktopInvitationInfo(
      service: info.service,
      name: info.name,
      fingerprint: fingerprint,
      format: info.format,
    );
  }
}

class _RekeyedApi extends FakePbMapperApi {
  final _host = _RekeyedHost();

  @override
  _RekeyedHost get desktop => _host;
}

/// The phone's camera, as mobile_scanner's platform sees it: it starts and
/// stops, and reads whatever QR code a test [show]s it.
class _Camera extends MobileScannerPlatform {
  final _codes = StreamController<BarcodeCapture?>.broadcast();
  int starts = 0, stops = 0;

  void show(String text) =>
      _codes.add(BarcodeCapture(barcodes: [Barcode(rawValue: text)]));

  @override
  Stream<BarcodeCapture?> get barcodesStream => _codes.stream;
  @override
  Stream<TorchState> get torchStateStream => const Stream.empty();
  @override
  Stream<double> get zoomScaleStateStream => const Stream.empty();
  @override
  Widget buildCameraView() => const ColoredBox(color: Colors.black);
  @override
  Future<MobileScannerViewAttributes> start(StartOptions startOptions) async {
    starts++;
    return const MobileScannerViewAttributes(
      cameraDirection: CameraFacing.back,
      currentTorchMode: TorchState.unavailable,
      size: Size(1280, 720),
    );
  }

  @override
  Future<void> stop() async => stops++;
  @override
  Future<void> updateScanWindow(Rect? window) async {}
  @override
  Future<void> dispose() async {}
}

Finder _key(String key) => find.byKey(Key(key));

/// The editable text inside the field [field] finds.
EditableText _editable(WidgetTester tester, Finder field) =>
    tester.widget<EditableText>(
      find.descendant(of: field, matching: find.byType(EditableText)),
    );

/// Scrolls [finder] into view and taps it, without waiting for anything
/// else to settle.
Future<void> _tapIn(WidgetTester tester, Finder finder) async {
  await tester.ensureVisible(finder);
  await tester.pump();
  await tester.tap(finder);
}

void main() {
  setUp(() => InviteScanner.debugSupportedOverride = false);
  tearDown(() => InviteScanner.debugSupportedOverride = null);

  testWidgets(
    'an invitation is recognised before connecting, never shown in full, '
    'and connects from the keyboard',
    (tester) async {
      final api = FakePbMapperApi();
      final vault = FakeRemoteComputerVault()
        ..records.add(
          const RemoteComputer(
            id: '0123456789abcdef0123456789abcdef',
            name: 'Lab',
            service: 'desktop:lab',
            fingerprint: 'FFFF-0000-FFFF-0000',
            relay: 'localhost:7666',
            namespace: 0,
          ),
        );
      await pumpApp(tester, api, page: AppPage.tunnels, vault: vault);
      await tester.tap(_key('nav-remote'));
      await settle(tester);
      await tester.tap(_key('remote-add'));
      await settle(tester);
      expect(_editable(tester, _key('add-invite')).focusNode.hasFocus, isTrue);

      // A setup string is refused for what it is, and nothing connects.
      await tester.enterText(_key('add-invite'), 'pbmt1_not-an-invitation');
      await tester.pump(const Duration(milliseconds: 300));
      await settle(tester);
      expect(find.textContaining('pb-mapper setup string'), findsOneWidget);
      expect(
        tester.widget<FilledButton>(_key('add-connect')).onPressed,
        isNull,
      );

      const invitation = 'pbd1.office-pc.SECRET-TOKEN-0123';
      await tester.enterText(_key('add-invite'), invitation);
      await tester.pump(const Duration(milliseconds: 300));
      await settle(tester);
      expect(find.text('desktop:office-pc'), findsOneWidget);
      expect(find.textContaining('AB12-CD34-EF56-7890'), findsOneWidget);
      expect(find.textContaining('SECRET-TOKEN'), findsNothing);
      expect(tester.widget<SwitchListTile>(_key('add-remember')).value, isTrue);

      // Recognition moves to the name, selected so typing replaces it, and
      // Enter there connects.
      final name = _editable(tester, _key('add-name'));
      expect(name.focusNode.hasFocus, isTrue);
      expect(
        name.controller.selection,
        const TextSelection(baseOffset: 0, extentOffset: 'office-pc'.length),
      );
      await tester.testTextInput.receiveAction(TextInputAction.done);
      // The viewer's Connecting spinner never settles.
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 50));
      }
      expect(api.desktop.argsOf('connect').single['invitation'], invitation);
      final desktop = AppScope.of(
        tester.element(find.byType(RemoteViewerScreen)),
      ).store.desktop;
      final session = desktop.session.value;
      expect(session.target?.computerId, matches(RegExp(r'^[0-9a-f]{32}$')));
      expect(session.phase, DesktopPhase.connecting);
      expect(find.byType(RemotePage), findsNothing);
      // Nothing is remembered before the first picture.
      expect(vault.ops, isNot(contains('remember')));
      await finish(tester);
    },
  );

  testWidgets(
    'a name typed after the invitation is recognised is the one remembered',
    (tester) async {
      final api = FakePbMapperApi();
      final vault = FakeRemoteComputerVault();
      await pumpApp(tester, api, page: AppPage.remote, vault: vault);
      await _tapIn(tester, _key('remote-add-empty'));
      await settle(tester);
      await tester.enterText(_key('add-invite'), 'pbd1.office-pc.TOKEN-0123');
      await tester.pump(const Duration(milliseconds: 300));
      await settle(tester);
      await tester.enterText(_key('add-name'), 'Studio');
      await tester.tap(_key('add-connect'));
      // The viewer's Connecting spinner never settles.
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 50));
      }
      api.desktop.viewerStatusValue = liveStatus();
      await tester.pump(const Duration(milliseconds: 500));
      await tester.pump();
      expect(vault.records.single.name, 'Studio');
      await finish(tester);
    },
  );

  testWidgets(
    'enabling shares view-only or with control, Stop works while busy, and '
    'helper maintenance stays reachable',
    (tester) async {
      // An installed helper of an older protocol: the one action
      // reinstalls it too (D2). This share is view-only.
      var api = _Api();
      api.desktop.available = const DesktopCapabilities(
        host: true,
        displays: [DesktopDisplay(1, 'Display 1', 1920, 1080)],
        helper: DesktopHelper(installed: true, updateRequired: true),
      );
      await pumpApp(tester, api, page: AppPage.remote);
      await tester.tap(_key('host-allow-control'));
      await settle(tester);
      await _tapIn(tester, _key('host-enable'));
      await settle(tester);
      final enable = api.desktop.argsOf('host_enable').single;
      expect(enable['service'], startsWith('desktop:'));
      expect(enable['display_id'], 1);
      expect(enable['allow_control'], isFalse);
      expect(api.desktop.hostSnapshot.background?.enabled, isTrue);
      expect(_key('host-enable'), findsNothing);

      // Allowing control waits its turn; Stop never waits behind it.
      final hold = api.desktop.holdPermission = Completer<void>();
      final control = find.descendant(
        of: _key('host-permission'),
        matching: find.text('View and control'),
      );
      await _tapIn(tester, control);
      await tester.pump();
      await _tapIn(tester, _key('host-stop'));
      await tester.pump();
      expect(api.desktop.ops, contains('host_stop'));
      expect(api.desktop.argsOf('host_permission'), isEmpty);
      hold.complete();
      await settle(tester);
      expect(api.desktop.argsOf('host_permission'), [
        {'allow_control': true},
      ]);
      await finish(tester);

      // A grant cached while service management is lost: the helper's
      // maintenance is reachable without stopping first, and Stop works.
      api = _Api();
      api.desktop.available = const DesktopCapabilities(
        host: true,
        displays: [DesktopDisplay(1, 'Display 1', 1920, 1080)],
        helper: DesktopHelper(
          installed: true,
          running: true,
          authorized: true,
          identityVerified: true,
        ),
      );
      api.desktop.hostSnapshot = const DesktopHostSnapshot(
        background: DesktopBackground(
          configured: true,
          enabled: true,
          autoStart: true,
          invitation: FakeDesktopApi.backgroundInvitation,
          message: 'Management connection lost',
        ),
      );
      await pumpApp(tester, api, page: AppPage.remote);
      await _tapIn(tester, _key('host-more'));
      await settle(tester);
      expect(
        tester.widget<MenuItemButton>(_key('host-helper-uninstall')).onPressed,
        isNotNull,
      );
      await tester.tap(_key('host-helper-disable'));
      await settle(tester);
      expect(api.desktop.argsOf('helper_action'), [
        {'action': 'disable'},
      ]);
      await _tapIn(tester, _key('host-stop'));
      await settle(tester);
      final background = api.desktop.hostSnapshot.background;
      expect(api.desktop.hostSnapshot.host, isNull);
      expect(background?.configured, isTrue);
      expect(background?.enabled, isFalse);
      expect(background?.autoStart, isFalse);
      await finish(tester);

      // On a phone-sized window: control by default, and sharing shows on
      // Remote's bar entry from every page until it stops.
      api = _Api();
      await pumpApp(
        tester,
        api,
        page: AppPage.tunnels,
        size: const Size(390, 844),
      );
      final sharingDot = find.descendant(
        of: _key('nav-remote'),
        matching: find.byType(StatusDot),
      );
      expect(sharingDot, findsNothing);
      await tester.tap(_key('nav-remote'));
      await settle(tester);
      await _tapIn(tester, _key('host-enable'));
      await settle(tester);
      expect(api.desktop.hostSnapshot.background?.host?.controlAllowed, isTrue);
      expect(tester.takeException(), isNull);
      await tester.tap(_key('nav-tunnels'));
      await settle(tester);
      expect(sharingDot, findsOneWidget);
      await tester.tap(_key('nav-remote'));
      await settle(tester);
      await _tapIn(tester, _key('host-stop'));
      await settle(tester);
      expect(api.desktop.hostSnapshot.background?.enabled, isFalse);
      expect(sharingDot, findsNothing);
      await finish(tester);
    },
  );

  testWidgets(
    'a remembered grant Windows cannot unlock says so and offers Revoke, '
    'without retrying in the background',
    (tester) async {
      final api = _UnreadableApi();
      await pumpApp(tester, api, page: AppPage.remote);
      expect(find.textContaining('could not unlock'), findsOneWidget);
      // Enabling would read the same grant and fail the same way.
      expect(
        tester.widget<FilledButton>(_key('host-enable')).onPressed,
        isNull,
      );

      // Only a visit reads it again, never a timer.
      final reads = api.desktop.argsOf('trust_status').length;
      await tester.pump(const Duration(seconds: 30));
      expect(api.desktop.argsOf('trust_status'), hasLength(reads));

      await _tapIn(tester, _key('host-error-revoke'));
      await settle(tester);
      await tester.tap(_key('confirm-destructive'));
      await settle(tester);
      expect(api.desktop.ops, contains('trust_revoke'));
      expect(_key('host-error'), findsNothing);
      expect(
        tester.widget<FilledButton>(_key('host-enable')).onPressed,
        isNotNull,
      );
      await finish(tester);
    },
  );

  testWidgets('a stopped host is renamed only once it shares again', (
    tester,
  ) async {
    final api = FakePbMapperApi();
    api.desktop.trust = const DesktopTrust(
      supported: true,
      matchesRelay: true,
      saved: DesktopTrustedHost(
        relay: 'localhost:7666',
        service: 'desktop:here',
        displayId: 1,
        invitation: FakeDesktopApi.localInvitation,
      ),
    );
    await pumpApp(tester, api, page: AppPage.remote);
    await _tapIn(tester, _key('host-more'));
    await settle(tester);
    expect(
      tester.widget<MenuItemButton>(_key('host-rename')).onPressed,
      isNull,
    );
    await tester.tapAt(Offset.zero);
    await settle(tester);

    await _tapIn(tester, _key('host-start'));
    await settle(tester);
    await _tapIn(tester, _key('host-more'));
    await settle(tester);
    await tester.tap(_key('host-rename'));
    await settle(tester);
    await tester.enterText(_key('host-rename-field'), 'den');
    await tester.tap(_key('host-rename-save'));
    await settle(tester);
    expect(api.desktop.argsOf('host_rename'), [
      {'service': 'desktop:den'},
    ]);
    expect(_key('host-error'), findsNothing);
    await finish(tester);
  });

  testWidgets(
    'the invitation QR shows only on request and hides when the page is left '
    'or the app goes inactive',
    (tester) async {
      final api = FakePbMapperApi();
      api.desktop.hostSnapshot = viewedHost();
      await pumpApp(tester, api, page: AppPage.remote);
      expect(_key('host-qr-panel'), findsNothing);
      await _tapIn(tester, _key('host-qr'));
      await settle(tester);
      expect(_key('host-qr-panel'), findsOneWidget);

      await tester.tap(_key('nav-tunnels'));
      await settle(tester);
      await tester.tap(_key('nav-remote'));
      await settle(tester);
      expect(_key('host-qr-panel'), findsNothing);

      await _tapIn(tester, _key('host-qr'));
      await settle(tester);
      expect(_key('host-qr-panel'), findsOneWidget);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      await tester.pump();
      expect(_key('host-qr-panel'), findsNothing);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      await finish(tester);
    },
  );

  testWidgets(
    "the viewer's controls stay on screen when the host card scrolls away",
    (tester) async {
      final api = FakePbMapperApi();
      api.desktop.hostSnapshot = viewedHost();
      await pumpApp(tester, api, page: AppPage.remote);
      // The viewer panel offers them, so no pill floats over the page.
      expect(_key('host-viewer'), findsOneWidget);
      expect(_key('sharing-fallback'), findsNothing);

      final page = find.byKey(const PageStorageKey('remote-page'));
      await tester.drag(page, const Offset(0, -600));
      await settle(tester);
      expect(_key('sharing-fallback'), findsOneWidget);
      await tester.drag(page, const Offset(0, 600));
      await settle(tester);
      expect(_key('sharing-fallback'), findsNothing);
      await finish(tester);
    },
  );

  testWidgets(
    'a computer remembered while it is viewed, whose identity changes before '
    "the vault answers, takes the host's new invitation from the viewer in "
    'place of its record',
    (tester) async {
      final api = _RekeyedApi();
      final vault = FakeRemoteComputerVault();
      final prefs = ViewerPrefs(persist: false);
      await prefs.setGestureSheetSeen(true);
      await pumpApp(
        tester,
        api,
        page: AppPage.remote,
        vault: vault,
        viewerPrefs: prefs,
      );
      Future<void> connect(String invitation) async {
        await tester.enterText(_key('add-invite'), invitation);
        await tester.pump(const Duration(milliseconds: 300));
        await settle(tester);
        await tester.tap(_key('add-connect'));
        // The viewer's Connecting spinner never settles.
        for (var i = 0; i < 6; i++) {
          await tester.pump(const Duration(milliseconds: 50));
        }
      }

      Future<void> firstPicture() async {
        api.desktop.viewerStatusValue = liveStatus();
        await tester.pump(const Duration(milliseconds: 500));
        await tester.pump();
      }

      // Added with Remember on: the viewer opens before the computer is
      // remembered, at its first picture, and the vault answers late.
      await _tapIn(tester, _key('remote-add-empty'));
      await settle(tester);
      await connect('pbd1.lab.OLD-TOKEN-0123');
      final remote = AppScope.of(
        tester.element(find.byType(RemoteViewerScreen)),
      ).remote;
      final id = remote.desktop.session.value.target!.computerId!;
      final answer = Completer<void>();
      vault.rememberHeld = answer;
      await firstPicture();
      expect(vault.ops, contains('remember'));

      // Re-keyed, the host meets the reconnect with another identity, while
      // the computer is not yet remembered: nothing to replace.
      api.desktop
        ..fingerprint = 'FFFF-0000-FFFF-0000'
        ..failNext('connect', 'IDENTITY_CHANGED')
        ..viewerStatusValue = stoppedStatus('transport');
      await tester.pump(const Duration(milliseconds: 500));
      await tester.pump(const Duration(seconds: 1));
      await settle(tester);
      expect(_key('viewer-overlay-identityChanged'), findsOneWidget);
      final l10n = lookupAppLocalizations(const Locale('en'));
      final paste = find.text(l10n.viewerPasteNewInvite);
      expect(paste, findsNothing);

      // The vault's answer is the only change left: the card offers the
      // host's new invitation from it.
      vault.rememberHeld = null;
      answer.complete();
      await settle(tester);
      expect(remote.computers.byId(id), isNotNull);
      expect(paste, findsOneWidget);

      // The new invitation connects as that computer, by its id, and
      // replaces its record at the first picture.
      await tester.tap(paste);
      await settle(tester);
      const invitation = 'pbd1.lab.NEW-TOKEN-0123';
      await connect(invitation);
      expect(api.desktop.argsOf('connect').last['invitation'], invitation);
      expect(remote.desktop.session.value.target?.computerId, id);
      await firstPicture();
      expect(vault.records.map((c) => (c.id, c.fingerprint)), [
        (id, 'FFFF-0000-FFFF-0000'),
      ]);
      await finish(tester);
    },
    // The touch viewer and the desktop viewer.
    variant: const TargetPlatformVariant({
      TargetPlatform.android,
      TargetPlatform.windows,
    }),
  );

  testWidgets(
    'Scan opens the camera wherever the compact Remote tab offers it, and a '
    'code it rejects does not end scanning',
    (tester) async {
      InviteScanner.debugSupportedOverride = true;
      final camera = _Camera();
      final platform = MobileScannerPlatform.instance;
      MobileScannerPlatform.instance = camera;
      addTearDown(() {
        MobileScannerPlatform.instance = platform;
        MobileScannerController.resetPlatformSessionOwner();
      });
      // A tablet held upright: wider than a dialog, too narrow for the rail.
      final api = FakePbMapperApi();
      await pumpApp(
        tester,
        api,
        page: AppPage.remote,
        size: const Size(800, 1280),
      );
      await _tapIn(tester, _key('remote-scan'));
      await settle(tester);
      expect(find.byType(InviteScanner), findsOneWidget);
      expect(camera.starts, 1);

      // The wrong code is refused, once however long it stays in view, and
      // the camera keeps looking for the right one.
      camera.show('pbmt1_a-setup-string');
      await settle(tester);
      expect(find.textContaining('pb-mapper setup string'), findsOneWidget);
      camera.show('pbmt1_a-setup-string');
      await settle(tester);
      expect(api.desktop.argsOf('inspect_invitation'), hasLength(1));
      expect(camera.stops, 0);
      camera.show('pbd1.office-pc.SECRET-TOKEN-0123');
      await settle(tester);
      expect(_key('add-recognised'), findsOneWidget);
      await finish(tester);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.android),
  );

  testWidgets(
    "a phone's featured computer can be removed from its More",
    (tester) async {
      const id = '0123456789abcdef0123456789abcdef';
      final vault = FakeRemoteComputerVault()
        ..records.add(
          const RemoteComputer(
            id: id,
            name: 'Lab',
            service: 'desktop:lab',
            fingerprint: 'FFFF-0000-FFFF-0000',
            relay: 'localhost:7666',
            namespace: 0,
          ),
        );
      // A phone is not controlled, so its computers lead the page.
      final api = FakePbMapperApi();
      api.desktop.available = const DesktopCapabilities(
        host: false,
        displays: [],
      );
      await pumpApp(
        tester,
        api,
        page: AppPage.remote,
        size: const Size(390, 844),
        vault: vault,
      );
      // A phone's only computer is the featured one, never a row.
      expect(_key('computer-featured'), findsOneWidget);
      await _tapIn(tester, _key('computer-featured-more'));
      await settle(tester);
      await tester.tap(_key('computer-remove-$id'));
      await settle(tester);
      await tester.tap(_key('confirm-destructive'));
      await settle(tester);
      expect(vault.records, isEmpty);
      await finish(tester);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.android),
  );

  testWidgets(
    "a remembered computer's rename and remove are reachable without a mouse",
    (tester) async {
      const id = '0123456789abcdef0123456789abcdef';
      final semantics = tester.ensureSemantics();
      final vault = FakeRemoteComputerVault()
        ..records.add(
          const RemoteComputer(
            id: id,
            name: 'Lab',
            service: 'desktop:lab',
            fingerprint: 'FFFF-0000-FFFF-0000',
            relay: 'lb7666.top:7666',
            namespace: 0,
          ),
        );
      await pumpApp(
        tester,
        FakePbMapperApi(serverServices: ['desktop:lab']),
        page: AppPage.remote,
        vault: vault,
      );
      Element inside(String key, Type type) => tester.element(
        find.descendant(of: _key(key), matching: find.byType(type)),
      );

      // Nothing hovers or focuses the row, and a screen reader still finds
      // its menu button.
      expect(
        tester.getSemantics(
          find.descendant(
            of: _key('computer-more-$id'),
            matching: find.byType(InkWell),
          ),
        ),
        isSemantics(isButton: true),
      );

      // Shift+Tab from Open reaches it, shown while it has focus, and Enter
      // opens the menu.
      Focus.of(inside('computer-open-$id', Text)).requestFocus();
      await tester.pump();
      await tester.sendKeyDownEvent(LogicalKeyboardKey.shiftLeft);
      await tester.sendKeyEvent(LogicalKeyboardKey.tab);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.shiftLeft);
      await tester.pump();
      expect(
        Focus.of(inside('computer-more-$id', Icon)).hasPrimaryFocus,
        isTrue,
      );
      final shown = find.ancestor(
        of: _key('computer-more-$id'),
        matching: find.byType(Opacity),
      );
      expect(tester.widget<Opacity>(shown.first).opacity, 1);
      await tester.sendKeyEvent(LogicalKeyboardKey.enter);
      await settle(tester);
      expect(_key('computer-remove-$id'), findsOneWidget);
      semantics.dispose();
      await finish(tester);
    },
  );
}
