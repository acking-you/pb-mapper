import 'dart:async';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../state/desktop_models.dart';
import 'pb_mapper_ffi.dart';
import 'pb_mapper_service.dart';

export '../state/desktop_models.dart';

/// A refused desktop call. [code] is the FFI `ErrorCode`, for example
/// `HOST_BUSY`, `IDENTITY_CHANGED` or `NOT_FRESH`; never match [message].
class DesktopCallException implements Exception {
  const DesktopCallException(this.code, [this.message = '']);
  final String code, message;
  @override
  String toString() => message.isEmpty ? code : message;
}

enum DesktopInvitationFormat { pbd1, jsonV1 }

class DesktopHelper {
  const DesktopHelper({
    this.installed = false,
    this.running = false,
    this.authorized = false,
    this.identityVerified = false,
    this.protocol = 0,
    this.updateRequired = false,
    this.message = '',
  });
  factory DesktopHelper.fromJson(Map<String, dynamic> data) => DesktopHelper(
    installed: data['installed'] == true,
    running: data['running'] == true,
    authorized: data['authorized_application'] == true,
    identityVerified: data['identity_verified'] == true,
    protocol: data['protocol'] as int? ?? 0,
    updateRequired: data['update_required'] == true,
    message: data['message'] as String? ?? '',
  );
  final bool installed, running, authorized, identityVerified;

  /// The installed helper's IPC protocol; [updateRequired] when it is not
  /// this app's, so the helper must be reinstalled (D2).
  final int protocol;
  final bool updateRequired;
  final String message;
}

class DesktopCapabilities {
  const DesktopCapabilities({
    this.host = false,
    this.displays = const [],
    this.message = '',
    this.helper,
  });
  final bool host;
  final List<DesktopDisplay> displays;
  final String message;
  final DesktopHelper? helper;
}

/// A host's `HostStatus`. The local host's also carries its [service] and
/// [invitation], which `HostStatus` itself never holds, and where it is
/// published.
class DesktopHostStatus {
  const DesktopHostStatus({
    this.state = 'waiting',
    this.reason,
    this.message = '',
    this.controlAllowed = false,
    this.viewer,
    this.display,
    this.desktop = 'ordinary',
    this.capabilities = const [],
    this.service,
    this.invitation,
    this.server,
    this.matchesRelay = true,
  });
  factory DesktopHostStatus.fromJson(
    Map<String, dynamic> status, {
    String? service,
    String? invitation,
    String? server,
    bool matchesRelay = true,
  }) => DesktopHostStatus(
    state: status['state'] as String? ?? 'waiting',
    reason: status['reason'] as String?,
    message: status['message'] as String? ?? '',
    controlAllowed: status['control_allowed'] == true,
    viewer: status['viewer'] is Map
        ? DesktopHostViewer.fromJson(
            Map<String, dynamic>.from(status['viewer'] as Map),
          )
        : null,
    display: status['display'] is Map
        ? DesktopDisplay.fromJson(
            Map<String, dynamic>.from(status['display'] as Map),
          )
        : null,
    desktop: status['desktop'] as String? ?? 'ordinary',
    capabilities: [
      for (final c in status['capabilities'] as List? ?? const []) c as String,
    ],
    service: service,
    invitation: invitation,
    server: server,
    matchesRelay: matchesRelay,
  );

  /// `waiting`, `connecting`, `streaming`, `paused`, `capture_unavailable`
  /// or `stopped`.
  final String state;
  final String? reason;
  final String message;
  final bool controlAllowed;
  final DesktopHostViewer? viewer;
  final DesktopDisplay? display;
  final String desktop;
  final List<String> capabilities;
  final String? service, invitation;

  /// The relay the local host is published on.
  final String? server;

  /// False when the settings now name another relay or credential than
  /// [server] and its namespace, as Rust compares them; true while unknown.
  final bool matchesRelay;

  DesktopHostStatus copyWith({
    bool? controlAllowed,
    DesktopHostViewer? Function()? viewer,
    DesktopDisplay? display,
    String? service,
    String? invitation,
  }) => DesktopHostStatus(
    state: state,
    reason: reason,
    message: message,
    controlAllowed: controlAllowed ?? this.controlAllowed,
    viewer: viewer == null ? this.viewer : viewer(),
    display: display ?? this.display,
    desktop: desktop,
    capabilities: capabilities,
    service: service ?? this.service,
    invitation: invitation ?? this.invitation,
    server: server,
    matchesRelay: matchesRelay,
  );
}

/// The Windows background service's hosting, as it last reported it.
class DesktopBackground {
  const DesktopBackground({
    this.configured = false,
    this.enabled = false,
    this.service,
    this.server,
    this.invitation,
    this.autoStart = false,
    this.displayId,
    this.allowControl = false,
    this.host,
    this.message = '',
    this.matchesRelay = true,
  });
  factory DesktopBackground.fromJson(Map<String, dynamic> data) {
    final settings = data['settings'] is Map
        ? Map<String, dynamic>.from(data['settings'] as Map)
        : const <String, dynamic>{};
    return DesktopBackground(
      configured: data['configured'] == true,
      enabled: data['enabled'] == true,
      service: settings['service'] as String?,
      server: data['server'] as String?,
      invitation: data['invitation'] as String?,
      autoStart: settings['auto_start'] == true,
      displayId: settings['display_id'] as int?,
      allowControl: settings['allow_control'] == true,
      host: data['host'] is Map
          ? DesktopHostStatus.fromJson(
              Map<String, dynamic>.from(data['host'] as Map),
            )
          : null,
      message: data['message'] as String? ?? '',
      matchesRelay: data['matches_relay'] != false,
    );
  }
  final bool configured, enabled, autoStart, allowControl;

  /// [server] is the relay its host is published on.
  final String? service, server, invitation;
  final int? displayId;
  final DesktopHostStatus? host;
  final String message;

  /// False when the settings now name another relay or credential than the
  /// one it is published on, as Rust compares them; true while unknown.
  final bool matchesRelay;
}

/// `host_status`: the local host, if one runs, and the background service.
class DesktopHostSnapshot {
  const DesktopHostSnapshot({this.host, this.background, this.busy = false});
  final DesktopHostStatus? host;
  final DesktopBackground? background;

  /// A lifecycle call held the host, so [host] is unknown, not absent.
  final bool busy;
}

class DesktopTrustedHost {
  const DesktopTrustedHost({
    required this.relay,
    required this.service,
    required this.displayId,
    this.allowControl = false,
    this.relayOnly = false,
    this.privileged = false,
    this.autoStart = false,
    this.background = false,
    this.invitation,
  });
  factory DesktopTrustedHost.fromJson(Map<String, dynamic> data) =>
      DesktopTrustedHost(
        relay: data['relay'] as String,
        service: data['service'] as String,
        displayId: data['display_id'] as int,
        allowControl: data['allow_control'] == true,
        relayOnly: data['relay_only'] == true,
        privileged: data['privileged'] == true,
        autoStart: data['auto_start'] == true,
        background: data['background'] == true,
        invitation: data['invitation'] as String?,
      );
  final String relay, service;
  final int displayId;
  final bool allowControl, relayOnly, privileged, autoStart, background;

  /// The remembered identity's invitation; null unless it matches the
  /// current relay and namespace.
  final String? invitation;
}

class DesktopTrust {
  const DesktopTrust({
    this.supported = false,
    this.saved,
    this.matchesRelay = false,
  });
  final bool supported, matchesRelay;
  final DesktopTrustedHost? saved;
}

class DesktopHostEnable {
  const DesktopHostEnable({
    this.restart = false,
    this.invitation,
    this.background,
  });
  final bool restart;
  final String? invitation;
  final DesktopBackground? background;
}

/// A connected viewer: where its video arrives, and in which codec.
class DesktopSession {
  const DesktopSession({required this.videoSource, required this.codec});
  final int videoSource;

  /// `h264` (a native consumer attaches) or `jpeg` ([DesktopApiClient.jpegFrames]).
  final String codec;
}

/// What an invitation names, read without connecting.
class DesktopInvitationInfo {
  const DesktopInvitationInfo({
    required this.service,
    required this.name,
    required this.fingerprint,
    required this.format,
  });
  final String service, name;

  /// The short form, `AB12-CD34-EF56-7890`.
  final String fingerprint;
  final DesktopInvitationFormat format;
}

/// One encoded JPEG picture, with the consumer that must receipt it.
class DesktopJpegFrame {
  const DesktopJpegFrame({
    required this.consumer,
    required this.generation,
    required this.frameId,
    required this.bytes,
  });
  final int consumer, generation, frameId;
  final Uint8List bytes;
}

/// The viewer's `ViewerStatus`: polled at most every 500 ms, never per frame.
class DesktopViewerStatus {
  const DesktopViewerStatus({
    this.state = 'connecting',
    this.stage,
    this.reason,
    this.message = '',
    this.controlAllowed = false,
    this.controlling = false,
    this.fresh = false,
    this.paused = false,
    this.generation = 0,
    this.display,
    this.secureDesktop = false,
    this.codec,
    this.videoWidth = 0,
    this.videoHeight = 0,
    this.presentedFrameId,
    this.lastVideoAgeMs,
    this.quality = DesktopQuality.balanced,
    this.capabilities = const [],
    this.notice,
    this.decoderState = 'ok',
    this.stats = DesktopStats.none,
  });

  factory DesktopViewerStatus.fromJson(Map<String, dynamic> json) {
    Map<String, dynamic> map(Object? value) => value is Map
        ? Map<String, dynamic>.from(value)
        : const <String, dynamic>{};
    final video = map(json['video']);
    final presented = map(json['presented']);
    final notice = map(json['notice']);
    final decoder = map(json['decoder']);
    final paused = json['paused'] == true;
    final noticeCode = switch (notice['code']) {
      'locked' => DesktopNoticeCode.locked,
      'lock_failed' => DesktopNoticeCode.lockFailed,
      _ => null,
    };
    return DesktopViewerStatus(
      state: json['state'] as String? ?? 'connecting',
      stage: json['stage'] as String?,
      reason: json['reason'] as String?,
      message: json['message'] as String? ?? '',
      controlAllowed: json['control_allowed'] == true,
      controlling: json['controlling'] == true,
      fresh: json['fresh'] == true,
      paused: paused,
      generation: json['generation'] as int? ?? 0,
      display: json['display'] is Map
          ? DesktopDisplay.fromJson(map(json['display']))
          : null,
      secureDesktop: json['desktop'] == 'secure',
      codec: video['codec'] as String?,
      videoWidth: video['width'] as int? ?? 0,
      videoHeight: video['height'] as int? ?? 0,
      presentedFrameId: presented.isEmpty
          ? null
          : presented['frame_id'] as int?,
      lastVideoAgeMs: json['last_video_age_ms'] as int?,
      quality: desktopQualityFromWire(json['quality']),
      capabilities: [
        for (final c in json['capabilities'] as List? ?? const []) c as String,
      ],
      notice:
          noticeCode != null && (notice['age_ms'] as int? ?? 0) < noticeTtlMs
          ? DesktopNotice(noticeCode, notice['message'] as String? ?? '')
          : null,
      decoderState: decoder['state'] as String? ?? 'ok',
      stats: DesktopStats.fromJson(
        map(json['stats']),
        decoder: decoder,
        paused: paused,
      ),
    );
  }

  /// How long a host notice stays on screen (`NOTICE_TTL`).
  static const noticeTtlMs = 5000;

  /// `connecting`, `streaming`, `paused`, `capture_unavailable` or `stopped`.
  final String state;

  /// While connecting: `path`, `identity` or `authorizing`.
  final String? stage;

  /// The `ViewerReason`, such as `transport`, `host_stopped` or
  /// `secure_desktop`.
  final String? reason;
  final String message;
  final bool controlAllowed, controlling, fresh, paused;
  final int generation;
  final DesktopDisplay? display;
  final bool secureDesktop;
  final String? codec;
  final int videoWidth, videoHeight;

  /// The last frame a consumer presented; null before the first.
  final int? presentedFrameId;

  /// Since the last video record of any generation.
  final int? lastVideoAgeMs;
  final DesktopQuality quality;
  final List<String> capabilities;
  final DesktopNotice? notice;

  /// `ok`, `recovering` or `failed`.
  final String decoderState;
  final DesktopStats stats;

  DesktopViewerStatus copyWith({
    String? state,
    String? Function()? reason,
    bool? controlAllowed,
    bool? controlling,
    bool? fresh,
    bool? paused,
    int? generation,
    int? Function()? presentedFrameId,
    int? Function()? lastVideoAgeMs,
    DesktopQuality? quality,
    String? decoderState,
    DesktopStats? stats,
  }) => DesktopViewerStatus(
    state: state ?? this.state,
    stage: stage,
    reason: reason == null ? this.reason : reason(),
    message: message,
    controlAllowed: controlAllowed ?? this.controlAllowed,
    controlling: controlling ?? this.controlling,
    fresh: fresh ?? this.fresh,
    paused: paused ?? this.paused,
    generation: generation ?? this.generation,
    display: display,
    secureDesktop: secureDesktop,
    codec: codec,
    videoWidth: videoWidth,
    videoHeight: videoHeight,
    presentedFrameId: presentedFrameId == null
        ? this.presentedFrameId
        : presentedFrameId(),
    lastVideoAgeMs: lastVideoAgeMs == null
        ? this.lastVideoAgeMs
        : lastVideoAgeMs(),
    quality: quality ?? this.quality,
    capabilities: capabilities,
    notice: notice,
    decoderState: decoderState ?? this.decoderState,
    stats: stats ?? this.stats,
  );
}

abstract interface class DesktopApiClient {
  // ---- lifecycle export: async, on the service's worker isolate
  Future<DesktopCapabilities> capabilities();
  Future<DesktopHostSnapshot> hostStatus();
  Future<DesktopHostEnable> enableHost(
    String service,
    int displayId, {
    required bool allowControl,
  });
  Future<String> startHost({
    required String service,
    required int displayId,
    required bool allowControl,
    bool relayOnly = false,
    bool privileged = false,
    bool remember = false,
    bool autoStart = false,
    bool background = false,
  });
  Future<void> stopHost({bool onExit = false});
  Future<void> allowControl(bool allow);

  /// Ends the current viewer's session; hosting continues (D15).
  Future<void> endViewerSession();

  /// Restarts the host on another display; returns the new invitation.
  Future<String> setHostDisplay(int displayId);

  /// Restarts the host under another service name; returns the new
  /// invitation.
  Future<String> renameHostService(String service);
  Future<DesktopTrust> trustStatus();
  Future<String?> restoreHost();
  Future<void> revokeTrust();

  /// True only after installation and an unelevated restart hand-off are
  /// ready.
  Future<bool> helperAction(String action);

  /// `status` asks the service; `start` resumes its saved host on the relay
  /// and credential the settings name now, with a new invitation when they
  /// are not the ones it was published on.
  Future<DesktopBackground> backgroundAction(String action);

  /// Throws [DesktopCallException] `SETUP_STRING` or `INVALID_INVITATION`.
  Future<DesktopInvitationInfo> inspectInvitation(String text);

  /// Sends exactly one of `invitation` and `computer_id`: the invitation
  /// when both are given.
  Future<DesktopSession> connect(
    String? invitation, {
    String? computerId,
    required bool relayOnly,
    required DesktopQuality quality,
    required double refreshHz,
  });
  Future<void> disconnect();

  // ---- hot export and view leaf: synchronous, UI isolate only, never block
  DesktopViewerStatus? viewerStatus();
  DesktopView? view();
  void control(bool enabled);
  void release();

  /// Throws [DesktopCallException] `NOT_FRESH`, `NOT_CONTROLLING` or
  /// `INPUT_CONGESTION`; Rust stamps frame id and sequence.
  void input(int generation, List<Map<String, Object?>> inputs);
  bool requestKeyframe();
  void setQuality(DesktopQuality quality);
  void pause();
  void resume();
  int instanceToken();

  // ---- JPEG consumer (platforms without a native decoder)
  Stream<DesktopJpegFrame> jpegFrames(int videoSource);
  bool presentedJpeg(int consumer, int generation, int frameId);
}

class DesktopApi implements DesktopApiClient {
  Future<Map<String, dynamic>> _call(
    String op, [
    Map<String, Object?> args = const {},
  ]) async {
    final result = await PbMapperService().desktopCall({'op': op, ...args});
    if (result['success'] != true) {
      throw DesktopCallException(
        result['code'] as String? ?? 'INTERNAL',
        result['message'] as String? ?? 'Desktop operation failed',
      );
    }
    return Map<String, dynamic>.from(result['data'] as Map? ?? {});
  }

  DesktopBackground? _background(Object? value) => value is Map
      ? DesktopBackground.fromJson(Map<String, dynamic>.from(value))
      : null;

  @override
  Future<DesktopCapabilities> capabilities() async {
    final data = await _call('capabilities');
    return DesktopCapabilities(
      host: (data['capabilities'] as Map?)?['host'] == true,
      displays: [
        for (final d in data['displays'] as List? ?? [])
          DesktopDisplay.fromJson(Map<String, dynamic>.from(d as Map)),
      ],
      message: data['message'] as String? ?? '',
      helper: data['helper'] is Map
          ? DesktopHelper.fromJson(
              Map<String, dynamic>.from(data['helper'] as Map),
            )
          : null,
    );
  }

  @override
  Future<DesktopHostSnapshot> hostStatus() async {
    final data = await _call('host_status');
    final host = data['host'] is Map
        ? Map<String, dynamic>.from(data['host'] as Map)
        : null;
    return DesktopHostSnapshot(
      host: host == null
          ? null
          : DesktopHostStatus.fromJson(
              Map<String, dynamic>.from(host['status'] as Map),
              service: host['service'] as String?,
              invitation: host['invitation'] as String?,
              server: host['server'] as String?,
              matchesRelay: host['matches_relay'] != false,
            ),
      background: _background(data['background']),
      busy: data['busy'] == true,
    );
  }

  @override
  Future<DesktopHostEnable> enableHost(
    String service,
    int displayId, {
    required bool allowControl,
  }) async {
    final data = await _call('host_enable', {
      'service': service,
      'display_id': displayId,
      'allow_control': allowControl,
    });
    return DesktopHostEnable(
      restart: data['restart'] == true,
      invitation: data['invitation'] as String?,
      background: _background(data['background']),
    );
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
  }) async =>
      (await _call('host_start', {
            'service': service,
            'display_id': displayId,
            'allow_control': allowControl,
            'relay_only': relayOnly,
            'privileged': privileged,
            'remember': remember,
            'auto_start': autoStart,
            'background': background,
          }))['invitation']
          as String;

  @override
  Future<void> stopHost({bool onExit = false}) async {
    await _call('host_stop', {'on_exit': onExit});
  }

  @override
  Future<void> allowControl(bool allow) async {
    await _call('host_permission', {'allow_control': allow});
  }

  @override
  Future<void> endViewerSession() async {
    await _call('host_end_session');
  }

  @override
  Future<String> setHostDisplay(int displayId) async =>
      (await _call('host_set_display', {'display_id': displayId}))['invitation']
          as String;

  @override
  Future<String> renameHostService(String service) async =>
      (await _call('host_rename', {'service': service}))['invitation']
          as String;

  @override
  Future<DesktopTrust> trustStatus() async {
    final data = await _call('trust_status');
    return DesktopTrust(
      supported: data['supported'] == true,
      matchesRelay: data['matches_relay'] == true,
      saved: data['saved'] is Map
          ? DesktopTrustedHost.fromJson(
              Map<String, dynamic>.from(data['saved'] as Map),
            )
          : null,
    );
  }

  @override
  Future<String?> restoreHost() async =>
      (await _call('host_restore'))['invitation'] as String?;

  @override
  Future<void> revokeTrust() async {
    await _call('trust_revoke');
  }

  @override
  Future<bool> helperAction(String action) async =>
      (await _call('helper_action', {'action': action}))['restart'] == true;

  @override
  Future<DesktopBackground> backgroundAction(String action) async =>
      _background(await _call('background_action', {'action': action})) ??
      const DesktopBackground();

  @override
  Future<DesktopInvitationInfo> inspectInvitation(String text) async {
    final data = await _call('inspect_invitation', {'invitation': text});
    return DesktopInvitationInfo(
      service: data['service'] as String,
      name: data['name'] as String,
      fingerprint: data['fingerprint'] as String,
      format: data['format'] == 'json_v1'
          ? DesktopInvitationFormat.jsonV1
          : DesktopInvitationFormat.pbd1,
    );
  }

  @override
  Future<DesktopSession> connect(
    String? invitation, {
    String? computerId,
    required bool relayOnly,
    required DesktopQuality quality,
    required double refreshHz,
  }) async {
    if (invitation == null && computerId == null) {
      throw const DesktopCallException(
        'INVALID_ARGUMENT',
        'an invitation or a remembered computer is required',
      );
    }
    final data = await _call('connect', {
      if (invitation != null)
        'invitation': invitation
      else
        'computer_id': computerId,
      'relay_only': relayOnly,
      'quality': quality.name,
      'refresh_hz': refreshHz,
    });
    return DesktopSession(
      videoSource: data['video_source'] as int,
      codec: data['codec'] as String? ?? 'h264',
    );
  }

  @override
  Future<void> disconnect() async {
    await _call('disconnect');
  }

  // ---- the hot export, the view leaf and the video exports: synchronous

  /// A hot op, on this (the UI) isolate.
  Object? _hot(String op, [Map<String, Object?> args = const {}]) {
    final result = PbMapperService().desktopViewerCallSync({'op': op, ...args});
    if (result['success'] != true) {
      throw DesktopCallException(
        result['code'] as String? ?? 'INTERNAL',
        result['message'] as String? ?? 'Desktop viewer operation failed',
      );
    }
    return result['data'];
  }

  /// The view leaf's one struct: it is read on the UI isolate only, and
  /// copied out before the call returns.
  static final _view = calloc<PbDesktopViewV1>();

  @override
  DesktopViewerStatus? viewerStatus() {
    final data = _hot('viewer_status');
    return data is Map
        ? DesktopViewerStatus.fromJson(Map<String, dynamic>.from(data))
        : null;
  }

  @override
  DesktopView? view() => PbMapperService().desktopView(_view) == 1
      ? desktopViewFromLeaf(_view.ref)
      : null;

  @override
  void control(bool enabled) => _hot('control', {'enabled': enabled});

  @override
  void release() => _hot('release');

  @override
  void input(int generation, List<Map<String, Object?>> inputs) =>
      _hot('input', {'generation': generation, 'inputs': inputs});

  @override
  bool requestKeyframe() => (_hot('keyframe') as Map?)?['requested'] == true;

  @override
  void setQuality(DesktopQuality quality) =>
      _hot('quality', {'preset': quality.name});

  @override
  void pause() => _hot('pause');

  @override
  void resume() => _hot('resume');

  @override
  int instanceToken() => PbMapperService().instanceToken();

  @override
  Stream<DesktopJpegFrame> jpegFrames(int videoSource) => PbMapperService()
      .desktopJpegUnits(videoSource)
      .map(
        (unit) => DesktopJpegFrame(
          consumer: unit.consumer,
          generation: unit.generation,
          frameId: unit.frameId,
          bytes: unit.bytes,
        ),
      );

  @override
  bool presentedJpeg(int consumer, int generation, int frameId) =>
      PbMapperService().desktopVideoPresented(consumer, generation, frameId);
}

/// Copies the view leaf's struct into a [DesktopView].
DesktopView desktopViewFromLeaf(PbDesktopViewV1 leaf) {
  final flags = leaf.flags;
  bool flag(int bit) => flags & (1 << bit) != 0;
  return DesktopView(
    generation: leaf.generation,
    presentedFrameId: leaf.presentedFrameId,
    presentedAgeMs: leaf.presentedAgeMs,
    cursorSeq: leaf.cursorSeq,
    displayWidth: leaf.displayWidth,
    displayHeight: leaf.displayHeight,
    videoWidth: leaf.videoWidth,
    videoHeight: leaf.videoHeight,
    cursorX: leaf.cursorX,
    cursorY: leaf.cursorY,
    state: leaf.state,
    cursorVisible: flag(0),
    controlling: flag(1),
    controlAllowed: flag(2),
    secureDesktop: flag(3),
    fresh: flag(4),
    paused: flag(5),
  );
}
