import 'dart:async';
import 'dart:typed_data';

import 'package:pb_mapper_ui/src/ffi/desktop_api.dart';
import 'package:pb_mapper_ui/src/ffi/desktop_video.dart';

/// What a live viewer reports: streaming on a 1920×1080 display, with a
/// presented, fresh picture.
DesktopViewerStatus liveStatus({
  int generation = 1,
  bool controlling = false,
  bool controlAllowed = true,
  bool fresh = true,
  int lastVideoAgeMs = 16,
  DesktopStats stats = const DesktopStats(path: 'direct', codec: 'h264'),
}) => DesktopViewerStatus(
  state: 'streaming',
  controlAllowed: controlAllowed,
  controlling: controlling,
  fresh: fresh,
  generation: generation,
  display: const DesktopDisplay(1, 'Display 1', 1920, 1080),
  codec: 'h264',
  videoWidth: 1920,
  videoHeight: 1080,
  presentedFrameId: 1,
  lastVideoAgeMs: lastVideoAgeMs,
  stats: stats,
);

/// What a viewer reports once its host closed the session for [reason].
DesktopViewerStatus stoppedStatus(String reason) =>
    DesktopViewerStatus(state: 'stopped', reason: reason, message: reason);

/// A scriptable desktop backend. Every call is logged in [calls] under its
/// FFI op name with the FFI's argument names; the host side keeps a small
/// consistent model (local host, background service, remembered trust), and
/// the viewer side returns whatever a test puts in [viewerStatusValue] and
/// [viewValue]. UI packages extend it by subclassing, never by editing it.
class FakeDesktopApi implements DesktopApiClient {
  final List<(String op, Map<String, Object?> args)> calls = [];

  /// What `capabilities` returns.
  DesktopCapabilities available = const DesktopCapabilities(
    host: true,
    displays: [DesktopDisplay(1, 'Display 1', 1920, 1080)],
  );

  /// What `host_status` returns.
  DesktopHostSnapshot hostSnapshot = const DesktopHostSnapshot();
  DesktopTrust trust = const DesktopTrust();

  /// What `viewer_status` and the view leaf return.
  DesktopViewerStatus? viewerStatusValue;
  DesktopView? viewValue;

  /// The codec a `connect` reports; `h264` needs a texture factory.
  String codec = 'jpeg';

  /// The host encodes H.265: a `connect` that offers it reports `h265`.
  bool hostH265 = false;

  /// What `decoders` answers, and how often it was asked. While
  /// [decodersGate] is set, the answer waits for it, as behind a wedged
  /// driver.
  DesktopDecoders decodersValue = DesktopDecoders.h264Only;
  int decodersProbed = 0;
  Completer<void>? decodersGate;

  /// While set, `connect` waits for it. A `disconnect` meanwhile makes that
  /// connect fail as cancelled, as the FFI does.
  Completer<void>? connectGate;

  /// While set, every `connect` fails with this code: a host that stays away.
  String? failConnects;

  final _failures = <String, String>{};
  int _sources = 0, _epoch = 0;
  StreamController<DesktopJpegFrame>? _jpeg;
  int? _jpegSource;

  /// The next call of [op] throws `DesktopCallException(code)`.
  void failNext(String op, String code) => _failures[op] = code;

  /// Sends one encoded picture to the current JPEG consumer. Previews pass
  /// PNG bytes: they decode through the same path.
  void emitJpeg(
    Uint8List encoded, {
    required int generation,
    required int frameId,
  }) {
    final source = _jpegSource;
    if (source == null) return;
    _jpeg?.add(
      DesktopJpegFrame(
        consumer: source,
        generation: generation,
        frameId: frameId,
        bytes: encoded,
      ),
    );
  }

  /// The ops logged so far, in order.
  List<String> get ops => [for (final call in calls) call.$1];

  /// The arguments of every logged [op].
  List<Map<String, Object?>> argsOf(String op) => [
    for (final call in calls)
      if (call.$1 == op) call.$2,
  ];

  /// Every input event sent, flattened across batches.
  List<Map<String, Object?>> get inputs => [
    for (final args in argsOf('input'))
      for (final input in args['inputs']! as List)
        input as Map<String, Object?>,
  ];

  void _log(String op, [Map<String, Object?> args = const {}]) {
    calls.add((op, args));
    final code = _failures.remove(op);
    if (code != null) throw DesktopCallException(code, '$op failed: $code');
  }

  static const localInvitation = 'pbd1.fake-local-invitation';
  static const backgroundInvitation = 'pbd1.fake-background-invitation';

  /// The background service's invitation once it moved to the current relay.
  static const movedInvitation = 'pbd1.fake-moved-invitation';

  // ---------------------------------------------------------------- host

  DesktopBackground get _background =>
      hostSnapshot.background ?? const DesktopBackground();

  void _set({
    DesktopHostStatus? Function()? host,
    DesktopBackground? Function()? background,
  }) => hostSnapshot = DesktopHostSnapshot(
    host: host == null ? hostSnapshot.host : host(),
    background: background == null ? hostSnapshot.background : background(),
  );

  DesktopBackground _backgroundWith({
    required bool enabled,
    required bool autoStart,
    DesktopHostStatus? host,
  }) {
    final b = _background;
    return DesktopBackground(
      configured: true,
      enabled: enabled,
      service: b.service,
      server: b.server,
      invitation: b.invitation,
      autoStart: autoStart,
      displayId: b.displayId,
      allowControl: b.allowControl,
      host: host,
      message: b.message,
      matchesRelay: b.matchesRelay,
    );
  }

  @override
  Future<DesktopCapabilities> capabilities() async {
    _log('capabilities');
    return available;
  }

  @override
  Future<DesktopHostSnapshot> hostStatus() async {
    _log('host_status');
    return hostSnapshot;
  }

  @override
  Future<DesktopHostEnable> enableHost(
    String service,
    int displayId, {
    required bool allowControl,
  }) async {
    _log('host_enable', {
      'service': service,
      'display_id': displayId,
      'allow_control': allowControl,
    });
    _configureBackground(service, displayId, allowControl, autoStart: true);
    return DesktopHostEnable(
      invitation: backgroundInvitation,
      background: hostSnapshot.background,
    );
  }

  /// The FFI stops a local host before the service takes over, and the
  /// remembered local grant is retired.
  void _configureBackground(
    String service,
    int displayId,
    bool allowControl, {
    required bool autoStart,
  }) {
    _set(
      host: () => null,
      background: () => DesktopBackground(
        configured: true,
        enabled: true,
        service: service,
        server: 'test',
        invitation: backgroundInvitation,
        autoStart: autoStart,
        displayId: displayId,
        allowControl: allowControl,
        host: DesktopHostStatus(controlAllowed: allowControl),
      ),
    );
    trust = DesktopTrust(supported: trust.supported);
  }

  @override
  Future<String> startHost({
    required String service,
    required int displayId,
    required bool allowControl,
    bool relayOnly = false,
    bool privileged = false,
    bool remember = false,
    bool autoStart = false,
    bool background = false,
  }) async {
    _log('host_start', {
      'service': service,
      'display_id': displayId,
      'allow_control': allowControl,
      'relay_only': relayOnly,
      'privileged': privileged,
      'remember': remember,
      'auto_start': autoStart,
      'background': background,
    });
    if (background) {
      _configureBackground(
        service,
        displayId,
        allowControl,
        autoStart: autoStart,
      );
      return backgroundInvitation;
    }
    _startLocal(service, displayId, allowControl);
    if (remember) {
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
          invitation: localInvitation,
        ),
      );
    }
    return localInvitation;
  }

  void _startLocal(String service, int displayId, bool allowControl) => _set(
    host: () => DesktopHostStatus(
      controlAllowed: allowControl,
      display: DesktopDisplay(displayId, 'Display $displayId', 1920, 1080),
      service: service,
      invitation: localInvitation,
    ),
  );

  @override
  Future<void> stopHost({bool onExit = false}) async {
    _log('host_stop', {'on_exit': onExit});
    if (_background.enabled && onExit) return;
    _set(host: () => null);
    if (_background.configured) {
      _set(background: () => _backgroundWith(enabled: false, autoStart: false));
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
          background: saved.background,
          invitation: saved.invitation,
        ),
      );
    }
  }

  @override
  Future<void> allowControl(bool allow) async {
    _log('host_permission', {'allow_control': allow});
    if (hostSnapshot.host case final host?) {
      _set(host: () => host.copyWith(controlAllowed: allow));
    } else if (_background.host case final host?) {
      _set(
        background: () => _backgroundWith(
          enabled: _background.enabled,
          autoStart: _background.autoStart,
          host: host.copyWith(controlAllowed: allow),
        ),
      );
    }
  }

  @override
  Future<void> endViewerSession() async {
    _log('host_end_session');
    if (hostSnapshot.host case final host?) {
      _set(host: () => host.copyWith(viewer: () => null));
    } else if (_background.host case final host?) {
      _set(
        background: () => _backgroundWith(
          enabled: _background.enabled,
          autoStart: _background.autoStart,
          host: host.copyWith(viewer: () => null),
        ),
      );
    }
  }

  @override
  Future<String> setHostDisplay(int displayId) async {
    _log('host_set_display', {'display_id': displayId});
    _refuseWhileBackground();
    final host = hostSnapshot.host;
    if (host == null) throw const DesktopCallException('IO', 'not shared');
    final invitation = '$localInvitation-$displayId';
    _set(
      host: () => host.copyWith(
        display: DesktopDisplay(displayId, 'Display $displayId', 1920, 1080),
        invitation: invitation,
      ),
    );
    return invitation;
  }

  @override
  Future<String> renameHostService(String service) async {
    _log('host_rename', {'service': service});
    _refuseWhileBackground();
    final host = hostSnapshot.host;
    if (host == null) throw const DesktopCallException('IO', 'not shared');
    final invitation = 'pbd1.$service';
    _set(
      host: () => host.copyWith(service: service, invitation: invitation),
    );
    return invitation;
  }

  void _refuseWhileBackground() {
    if (_background.configured) {
      throw const DesktopCallException(
        'INVALID_ARGUMENT',
        'stop the background host first',
      );
    }
  }

  @override
  Future<DesktopTrust> trustStatus() async {
    _log('trust_status');
    return trust;
  }

  @override
  Future<String?> restoreHost() async {
    _log('host_restore');
    final saved = trust.saved;
    if (saved == null) {
      throw const DesktopCallException('IO', 'nothing is remembered');
    }
    _startLocal(saved.service, saved.displayId, saved.allowControl);
    return localInvitation;
  }

  @override
  Future<void> revokeTrust() async {
    _log('trust_revoke');
    hostSnapshot = const DesktopHostSnapshot();
    trust = DesktopTrust(supported: trust.supported);
  }

  @override
  Future<bool> helperAction(String action) async {
    _log('helper_action', {'action': action});
    return false;
  }

  @override
  Future<DesktopBackground> backgroundStatus() async {
    _log('background_action', {'action': 'status'});
    return _background;
  }

  /// Resumes the service's host on the current relay, `test`, as the FFI
  /// configures it from the settings: from another relay only when asked,
  /// with a new invitation. A helper [available] says is not ready, or is
  /// another build, is installed first, and the app restarts into it.
  @override
  Future<DesktopHostEnable> resumeBackground({bool moveRelay = false}) async {
    _log('background_action', {'action': 'start', 'move_relay': moveRelay});
    final b = _background;
    if (!b.matchesRelay && !moveRelay) {
      throw const DesktopCallException('RELAY_CHANGED', 'on another relay');
    }
    if (available.helper case final helper?
        when helper.updateRequired ||
            helper.updateAvailable ||
            !helper.running ||
            !helper.authorized ||
            !helper.identityVerified) {
      return const DesktopHostEnable(restart: true);
    }
    _set(
      background: () => DesktopBackground(
        configured: true,
        enabled: true,
        service: b.service,
        server: 'test',
        invitation: b.matchesRelay ? b.invitation : movedInvitation,
        autoStart: true,
        displayId: b.displayId,
        allowControl: b.allowControl,
        host: DesktopHostStatus(controlAllowed: b.allowControl),
      ),
    );
    return DesktopHostEnable(
      invitation: _background.invitation,
      background: _background,
    );
  }

  @override
  Future<DesktopInvitationInfo> inspectInvitation(String text) async {
    _log('inspect_invitation', {'invitation': text});
    final trimmed = text.trim();
    if (trimmed.startsWith('pbd1.')) {
      final segment = RegExp(r'^[a-z0-9-]*').stringMatch(trimmed.substring(5));
      final service = 'desktop:${segment ?? ''}';
      return DesktopInvitationInfo(
        service: service,
        name: service.substring('desktop:'.length),
        fingerprint: 'AB12-CD34-EF56-7890',
        format: DesktopInvitationFormat.pbd1,
      );
    }
    if (trimmed.startsWith('pbmt1_') || _setupJson(trimmed)) {
      throw const DesktopCallException(
        'SETUP_STRING',
        'this is a pb-mapper setup string, not a desktop invitation',
      );
    }
    throw const DesktopCallException(
      'INVALID_INVITATION',
      'not a desktop invitation',
    );
  }

  /// A base64 pb-mapper setup string is JSON: it starts with `{"` encoded.
  static bool _setupJson(String text) =>
      text.startsWith('eyJ') || text.startsWith('{');

  // -------------------------------------------------------------- viewer

  /// A scripted failure ([failNext]) comes after [connectGate], as the FFI's
  /// does after the network: a stage seen meanwhile stays visible.
  @override
  Future<DesktopSession> connect(
    String? invitation, {
    String? computerId,
    required bool relayOnly,
    required DesktopQuality quality,
    required double refreshHz,
    required bool h265,
    required DesktopResolution resolution,
    required int maxFps,
  }) async {
    final epoch = _epoch;
    calls.add((
      'connect',
      {
        if (invitation != null)
          'invitation': invitation
        else
          'computer_id': computerId,
        'relay_only': relayOnly,
        'quality': quality.name,
        'refresh_hz': refreshHz,
        'h265': h265,
        'resolution': resolution.wire,
        'max_fps': maxFps,
      },
    ));
    final failure = _failures.remove('connect') ?? failConnects;
    final gate = connectGate;
    if (gate != null) {
      connectGate = null;
      await gate.future;
    }
    if (epoch != _epoch) {
      throw const DesktopCallException('IO', 'connection cancelled');
    }
    if (failure != null) {
      throw DesktopCallException(failure, 'connect failed: $failure');
    }
    viewerStatusValue = DesktopViewerStatus(quality: quality);
    viewValue = null;
    return DesktopSession(
      videoSource: ++_sources,
      codec: h265 && hostH265 ? 'h265' : codec,
    );
  }

  @override
  Future<DesktopDecoders> decoders() async {
    decodersProbed++;
    await decodersGate?.future;
    return decodersValue;
  }

  @override
  Future<void> disconnect() async {
    _epoch++;
    _log('disconnect');
    viewerStatusValue = null;
    viewValue = null;
    _closeJpeg();
  }

  /// The source closes; its consumer may already have cancelled, and then
  /// the close never completes, so nothing waits for it.
  void _closeJpeg() {
    final jpeg = _jpeg;
    _jpeg = null;
    _jpegSource = null;
    unawaited(jpeg?.close());
  }

  @override
  DesktopViewerStatus? viewerStatus() {
    _log('viewer_status');
    return viewerStatusValue;
  }

  @override
  DesktopView? view() {
    _log('view');
    return viewValue;
  }

  @override
  void control(bool enabled) {
    _log('control', {'enabled': enabled});
    final status = viewerStatusValue;
    if (status == null) return;
    final controlling = enabled && status.controlAllowed;
    viewerStatusValue = status.copyWith(controlling: controlling);
    final view = viewValue;
    if (view != null) {
      viewValue = DesktopView(
        generation: view.generation,
        presentedFrameId: view.presentedFrameId,
        presentedAgeMs: view.presentedAgeMs,
        cursorSeq: view.cursorSeq,
        displayWidth: view.displayWidth,
        displayHeight: view.displayHeight,
        videoWidth: view.videoWidth,
        videoHeight: view.videoHeight,
        cursorX: view.cursorX,
        cursorY: view.cursorY,
        state: view.state,
        cursorVisible: view.cursorVisible,
        controlling: controlling,
        controlAllowed: view.controlAllowed,
        secureDesktop: view.secureDesktop,
        fresh: view.fresh,
        paused: view.paused,
      );
    }
  }

  @override
  void release() => _log('release');

  @override
  void input(int generation, List<Map<String, Object?>> inputs) =>
      _log('input', {'generation': generation, 'inputs': inputs});

  @override
  bool requestKeyframe() {
    _log('keyframe');
    return true;
  }

  @override
  void setQuality(DesktopQuality quality) {
    _log('quality', {'preset': quality.name});
    viewerStatusValue = viewerStatusValue?.copyWith(quality: quality);
  }

  @override
  void pause() {
    _log('pause');
    viewerStatusValue = viewerStatusValue?.copyWith(
      paused: true,
      controlling: false,
    );
  }

  @override
  void resume() {
    _log('resume');
    viewerStatusValue = viewerStatusValue?.copyWith(paused: false);
  }

  @override
  int instanceToken() => 1;

  @override
  Stream<DesktopJpegFrame> jpegFrames(int videoSource) {
    _log('jpeg_frames', {'source': videoSource});
    unawaited(_jpeg?.close());
    final jpeg = StreamController<DesktopJpegFrame>();
    _jpeg = jpeg;
    _jpegSource = videoSource;
    return jpeg.stream;
  }

  @override
  bool presentedJpeg(int consumer, int generation, int frameId) {
    _log('presented', {
      'consumer': consumer,
      'generation': generation,
      'frame_id': frameId,
    });
    return true;
  }
}

/// A native texture that records what the store asks of it.
class FakeVideoTexture implements DesktopVideoTexture {
  FakeVideoTexture(this.textureId);

  @override
  final int textureId;

  /// Every source attached, in order: a reconnect attaches the new one.
  final attached = <int>[];
  final snapshots = <String>[];
  bool disposed = false;

  /// What [snapshot] answers; a future that never completes stands in for
  /// a decoder that does not answer.
  Future<bool> Function() snapshotAnswer = () async => true;

  /// Every [stats] call's `timings`, in order, and what it answers.
  final statsTimings = <bool>[];
  Future<Map<String, Object?>> Function() statsAnswer = () async => const {};

  @override
  Future<void> attach(int videoSource) async => attached.add(videoSource);

  @override
  Future<bool> snapshot(String path, {int maxWidth = 320}) {
    snapshots.add(path);
    return snapshotAnswer();
  }

  @override
  Future<Map<String, Object?>> stats({bool timings = true}) {
    statsTimings.add(timings);
    return statsAnswer();
  }

  @override
  Future<void> dispose() async => disposed = true;
}

/// A [DesktopTextureFactory] over [FakeVideoTexture]s, which it keeps.
class FakeTextures {
  final created = <FakeVideoTexture>[];

  /// While set, creation waits for it, as a slow platform channel would.
  Completer<void>? gate;

  Future<DesktopVideoTexture> create({required int instanceToken}) async {
    final waiting = gate;
    gate = null;
    await waiting?.future;
    final texture = FakeVideoTexture(created.length + 1);
    created.add(texture);
    return texture;
  }
}
