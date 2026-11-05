import 'dart:async';
import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';

class FakeDesktopApi implements DesktopApiClient {
  DesktopBackground background = const DesktopBackground();
  @override
  Future<DesktopBackground> backgroundAction(String action) async {
    if (action == 'start') {
      background = DesktopBackground(
        configured: true,
        enabled: true,
        service: background.service,
        server: background.server,
        invitation: background.invitation,
        autoStart: background.autoStart,
      );
      host = const DesktopStatus(state: 'waiting');
    } else if (action == 'stop') {
      await stopHost();
    } else if (action == 'revoke') {
      host = null;
      background = const DesktopBackground();
    }
    return background;
  }

  DesktopTrust trust = const DesktopTrust();
  @override
  Future<DesktopTrust> trustStatus() async => trust;
  @override
  Future<String> restoreHost() async {
    final saved = trust.saved!;
    return startHost(
      service: saved.service,
      displayId: saved.displayId,
      allowControl: saved.allowControl,
      relayOnly: saved.relayOnly,
      privileged: saved.privileged,
      remember: true,
      autoStart: saved.autoStart,
    );
  }

  @override
  Future<void> revokeTrust() async {
    host = null;
    trust = DesktopTrust(supported: trust.supported);
  }

  @override
  Stream<DesktopFrame> frames() => const Stream.empty();
  @override
  Future<void> keyframe() async {}
  @override
  Future<void> helperAction(String action) async => helperActions.add(action);
  final helperActions = <String>[];
  @override
  Future<void> presented(int generation, int frameId) async {}
  DesktopCapabilities available = const DesktopCapabilities(
    host: true,
    displays: [DesktopDisplay(1, 'Display 1', 1920, 1080)],
  );
  DesktopStatus? host, viewer;
  DesktopFrame? image;
  final inputs = <Map<String, Object?>>[];
  final sequences = <int>[];
  int releases = 0;
  Completer<void>? inputGate;
  @override
  Future<DesktopCapabilities> capabilities() async => available;
  @override
  Future<DesktopSnapshot> status() async =>
      DesktopSnapshot(host: host, viewer: viewer, background: background);
  @override
  Future<String> startHost({
    required String service,
    required int displayId,
    required bool allowControl,
    required bool relayOnly,
    bool privileged = false,
    bool remember = false,
    bool autoStart = false,
    bool background = false,
  }) async {
    host = DesktopStatus(state: 'waiting', controlAllowed: allowControl);
    if (background) {
      this.background = DesktopBackground(
        configured: true,
        enabled: true,
        service: service,
        server: 'test',
        invitation: 'private background invitation',
        autoStart: autoStart,
      );
      return this.background.invitation!;
    } else if (remember) {
      trust = DesktopTrust(
        supported: true,
        matchesRelay: true,
        saved: DesktopTrustedHost(
          relay: 'test',
          service: service,
          displayId: displayId,
          allowControl: allowControl,
          relayOnly: relayOnly,
          privileged: privileged,
          autoStart: autoStart,
        ),
      );
    }
    return 'private test invitation';
  }

  @override
  Future<void> stopHost({bool onExit = false}) async {
    if (background.enabled && onExit) return;
    host = null;
    if (background.configured) {
      background = DesktopBackground(
        configured: true,
        service: background.service,
        server: background.server,
        invitation: background.invitation,
      );
    }
    if (trust.saved case final saved? when !onExit) {
      trust = DesktopTrust(
        supported: trust.supported,
        matchesRelay: trust.matchesRelay,
        saved: DesktopTrustedHost(
          relay: saved.relay,
          service: saved.service,
          displayId: saved.displayId,
          allowControl: saved.allowControl,
          relayOnly: saved.relayOnly,
          privileged: saved.privileged,
        ),
      );
    }
  }

  @override
  Future<void> allowControl(bool allow) async {
    host = DesktopStatus(state: 'waiting', controlAllowed: allow);
  }

  @override
  Future<void> connect(String invitation, {required bool relayOnly}) async {
    viewer = const DesktopStatus(state: 'connecting');
  }

  @override
  Future<void> disconnect() async {
    viewer = null;
  }

  @override
  Future<void> control(bool enabled) async {
    viewer = DesktopStatus(
      state: 'streaming',
      controlling: enabled,
      controlAllowed: true,
      generation: image?.status.generation ?? 1,
    );
  }

  @override
  Future<void> release() async {
    releases++;
  }

  @override
  Future<void> input({
    required int generation,
    required int frameId,
    required int sequence,
    required Map<String, Object?> input,
  }) async {
    await inputGate?.future;
    inputs.add(input);
    sequences.add(sequence);
  }

  @override
  Future<DesktopFrame?> frame(int after, int generation) async =>
      image?.status.frameId == after && image?.status.generation == generation
      ? null
      : image;
}
