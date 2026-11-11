import 'dart:async';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../state/desktop_models.dart';
import 'desktop_video.dart';
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
    this.updateAvailable = false,
    this.message = '',
  });
  factory DesktopHelper.fromJson(Map<String, dynamic> data) => DesktopHelper(
    installed: data['installed'] == true,
    running: data['running'] == true,
    authorized: data['authorized_application'] == true,
    identityVerified: data['identity_verified'] == true,
    protocol: data['protocol'] as int? ?? 0,
    updateRequired: data['update_required'] == true,
    updateAvailable: data['update_available'] == true,
    message: data['message'] as String? ?? '',
  );
  final bool installed, running, authorized, identityVerified;

  /// The installed helper's IPC protocol; [updateRequired] when it is not
  /// this app's, so the helper must be reinstalled (D2).
  final int protocol;
  final bool updateRequired;

  /// The installed helper is another build of this app's protocol: it
  /// works, but hosts in the background with its own build's code until it
  /// is updated.
  final bool updateAvailable;
  final String message;
}

class DesktopCapabilities {
  const DesktopCapabilities({
    this.host = false,
    this.displays = const [],
    this.message = '',
    this.helper,
    this.reason,
    this.backgroundService = true,
    this.permissions,
  });
  final bool host;
  final List<DesktopDisplay> displays;
  final String message;
  final DesktopHelper? helper;

  /// Why this computer cannot host: `platform`, `media_foundation_missing`,
  /// `no_display` or `permission_missing`; null when it can.
  final String? reason;

  /// Sharing goes through the background service (Windows' helper); false
  /// where the app shares the desktop itself (macOS).
  final bool backgroundService;

  /// The permissions hosting asks the owner for (macOS); null elsewhere.
  final DesktopHostPermissions? permissions;
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
    this.displayMode,
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
    displayMode: DisplayModeList.fromJson(status['display_mode']),
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

  /// False when the settings now name another relay or credential namespace
  /// than [server] and its namespace, as Rust compares them; true while
  /// unknown.
  final bool matchesRelay;

  /// A viewer changed this display's mode, and it is not back yet.
  final DisplayModeList? displayMode;

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
    displayMode: displayMode,
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

  /// False when the settings now name another relay or credential namespace
  /// than the ones it is published on, as Rust compares them; true while
  /// unknown, since a resume that would move it is refused unless asked.
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

  /// `h264` or `h265` (a native consumer attaches), or `jpeg`
  /// ([DesktopApiClient.jpegFrames]). Rust refuses an `h265` it did not
  /// offer.
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
    this.modes,
    this.hostOs,
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
      'display_mode_failed' => DesktopNoticeCode.displayModeFailed,
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
      modes: DisplayModeList.fromJson(json['modes']),
      hostOs: json['host_os'] as String?,
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
        status: json,
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

  /// The remote display's modes, from a host that offers them (R17).
  final DisplayModeList? modes;

  /// `windows`, `macos`, `linux` or `android`, where the host says.
  final String? hostOs;
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
    modes: modes,
    hostOs: hostOs,
    capabilities: capabilities,
    notice: notice,
    decoderState: decoderState ?? this.decoderState,
    stats: stats ?? this.stats,
  );
}

abstract interface class DesktopApiClient {
  // ---- lifecycle export: async, on the service's worker isolate
  Future<DesktopCapabilities> capabilities();

  /// Asks the owner for [permission], `screen_capture` or `input`, where
  /// the platform asks (macOS: its prompt the first time, and the app listed
  /// in System Settings); whether it is granted now.
  Future<bool> requestHostPermission(String permission);
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

  /// Puts back this computer's display mode, which a viewer changed
  /// (R17): the app's own host, or the background service's. False when
  /// nothing was there to ask.
  Future<bool> restoreHostDisplayMode();

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

  /// Asks the service for its state.
  Future<DesktopBackground> backgroundStatus();

  /// Resumes the service's saved host on the relay and credential the
  /// settings name now. Throws [DesktopCallException] `RELAY_CHANGED` when
  /// they are not the relay and namespace it was published on, unless
  /// [moveRelay] says the owner agreed to its new invitation. A helper that
  /// is not ready, or another build, is set up first as [enableHost] does:
  /// then [DesktopHostEnable.restart] may ask the app to restart into it.
  Future<DesktopHostEnable> resumeBackground({bool moveRelay = false});

  /// Throws [DesktopCallException] `SETUP_STRING` or `INVALID_INVITATION`.
  Future<DesktopInvitationInfo> inspectInvitation(String text);

  /// Sends exactly one of `invitation` and `computer_id`: the invitation
  /// when both are given. [decoders] is the offer (D6): H.265 where it has
  /// a decoder, H.264 where it has one, else JPEG; the limits are
  /// [resolution]'s (null: the default) and [maxFps] (0: Auto), fitted
  /// inside the largest picture of the decoder offered first (D7, D8).
  Future<DesktopSession> connect(
    String? invitation, {
    String? computerId,
    required bool relayOnly,
    required DesktopQuality quality,
    required double refreshHz,
    required DesktopDecoders decoders,
    DesktopResolution? resolution,
    required int maxFps,
  });
  Future<void> disconnect();

  /// What this device decodes natively (I4); never throws.
  Future<DesktopDecoders> decoders();

  // ---- hot export and view leaf: synchronous, UI isolate only, never block
  /// [local], this computer's display, fills [DisplayModeList.fitLocal].
  DesktopViewerStatus? viewerStatus({DisplayMode? local});
  DesktopView? view();
  void control(bool enabled);
  void release();

  /// Throws [DesktopCallException] `NOT_FRESH`, `NOT_CONTROLLING` or
  /// `INPUT_CONGESTION`; Rust stamps frame id and sequence.
  void input(int generation, List<Map<String, Object?>> inputs);
  bool requestKeyframe();
  void setQuality(DesktopQuality quality);

  /// Changes the remote display's mode, live, never a reconnect (R8); returns
  /// the mode asked for. Throws [DesktopCallException]
  /// `DISPLAY_MODE_NOT_OFFERED`, `NOT_CONTROLLING`, `DISPLAY_MODE_NO_FIT`,
  /// `DISPLAY_MODE_NOT_LISTED` or, once closed, `IO`. The host refuses with
  /// a `displayModeFailed` notice.
  DisplayMode setDisplayMode(DisplayModeTarget target);
  void pause();
  void resume();

  /// The definition [DesktopView.cursorShape] names, fetched once per
  /// number; null once the viewer no longer holds it.
  DesktopCursorShape? cursorShape(int shape);
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
    final capabilities = data['capabilities'] as Map?;
    final permissions = capabilities?['permissions'];
    return DesktopCapabilities(
      host: capabilities?['host'] == true,
      reason: capabilities?['reason'] as String?,
      backgroundService: capabilities?['background_service'] != false,
      permissions: permissions is Map
          ? DesktopHostPermissions.fromJson(
              Map<String, dynamic>.from(permissions),
            )
          : null,
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
  Future<bool> requestHostPermission(String permission) async =>
      (await _call('host_request_permission', {
        'permission': permission,
      }))['granted'] ==
      true;

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
  Future<bool> restoreHostDisplayMode() async =>
      (await _call('host_restore_display_mode'))['restored'] == true;

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
  Future<DesktopBackground> backgroundStatus() async =>
      _background(await _call('background_action', {'action': 'status'})) ??
      const DesktopBackground();

  @override
  Future<DesktopHostEnable> resumeBackground({bool moveRelay = false}) async {
    final data = await _call('background_action', {
      'action': 'start',
      'move_relay': moveRelay,
    });
    return DesktopHostEnable(
      restart: data['restart'] == true,
      invitation: data['invitation'] as String?,
      background: _background(data['background']),
    );
  }

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
    required DesktopDecoders decoders,
    DesktopResolution? resolution,
    required int maxFps,
  }) async {
    if (invitation == null && computerId == null) {
      throw const DesktopCallException(
        'INVALID_ARGUMENT',
        'an invitation or a remembered computer is required',
      );
    }
    List<int>? largest(DesktopDecoderLimit? limit) =>
        limit == null ? null : [limit.maxWidth, limit.maxHeight];
    final data = await _call('connect', {
      if (invitation != null)
        'invitation': invitation
      else
        'computer_id': computerId,
      'relay_only': relayOnly,
      'quality': quality.name,
      'refresh_hz': refreshHz,
      'decoders': {
        'h264': largest(decoders.h264),
        'h265': largest(decoders.h265),
      },
      'resolution': ?resolution?.wire,
      'max_fps': maxFps,
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

  /// A runner without native video has no decoder to offer: its picture is
  /// JPEG, which Dart decodes. What a failed probe offers is
  /// [decodersFromProbe]'s rule.
  @override
  Future<DesktopDecoders> decoders() => probeDesktopDecoders();

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
  DesktopViewerStatus? viewerStatus({DisplayMode? local}) {
    final data = _hot('viewer_status', {'local': ?local?.toJson()});
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
  DisplayMode setDisplayMode(DisplayModeTarget target) {
    final data = _hot('display_mode', {'target': target.toJson()});
    final mode = data is Map ? DisplayMode.fromJson(data['mode']) : null;
    if (mode == null) {
      throw const DesktopCallException('INTERNAL', 'no display mode answered');
    }
    return mode;
  }

  @override
  void pause() => _hot('pause');

  @override
  void resume() => _hot('resume');

  @override
  DesktopCursorShape? cursorShape(int shape) {
    final data = _hot('cursor_shape', {'shape': shape});
    return data is Map
        ? DesktopCursorShape.fromJson(Map<String, dynamic>.from(data))
        : null;
  }

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
    cursorShape: leaf.cursorShape,
    state: leaf.state,
    cursorVisible: flag(0),
    controlling: flag(1),
    controlAllowed: flag(2),
    secureDesktop: flag(3),
    fresh: flag(4),
    paused: flag(5),
  );
}
