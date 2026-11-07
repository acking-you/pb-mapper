// The values the desktop store publishes, and the ones the FFI returns that
// the store passes on unchanged. Every one is immutable with value equality,
// so a `ValueNotifier` holding it notifies only on a real change (D14).
import 'package:flutter/foundation.dart';

/// The quality preset (D12). The names are the wire names.
enum DesktopQuality { quality, balanced, smooth }

DesktopQuality desktopQualityFromWire(Object? value) => switch (value) {
  'quality' => DesktopQuality.quality,
  'smooth' => DesktopQuality.smooth,
  _ => DesktopQuality.balanced,
};

enum DesktopPhase {
  idle,
  connecting,
  live,
  recovering,
  waitingForDesktop,
  paused,
  reconnecting,
  identityChanged,
  ended,
}

/// Rust's connect stages `path`, `identity` and `authorizing`, then Dart's
/// wait for the first presented picture.
enum DesktopSetupStep { finding, verifying, authorizing, firstPicture }

enum DesktopEndReason {
  hostStopped,
  disconnectedByHost,
  revoked,
  versionMismatch,
  denied,
  invalidInvitation,
  decoderFailed,
  failed,
}

enum DesktopAdaptation {
  startup,
  steady,
  raising,
  loweringFps,
  loweringBitrate,
  unresponsive,
  paused,
}

DesktopAdaptation _adaptationFromWire(Object? value) => switch (value) {
  'steady' => DesktopAdaptation.steady,
  'raising' => DesktopAdaptation.raising,
  'lowering_fps' => DesktopAdaptation.loweringFps,
  'lowering_bitrate' => DesktopAdaptation.loweringBitrate,
  'unresponsive' => DesktopAdaptation.unresponsive,
  'paused' => DesktopAdaptation.paused,
  _ => DesktopAdaptation.startup,
};

/// Who wants the store's timers: the Remote page, or a viewer on screen.
enum DesktopDemand { page, viewer }

/// The Windows helper service: not installed (or no helper on this
/// platform), ready, stopped, needing an update (`update_required`), or
/// running with an identity that cannot be verified.
enum DesktopHelperState { none, ready, stopped, outdated, unverified }

enum DesktopNoticeCode { locked, lockFailed }

@immutable
class DesktopDisplay {
  const DesktopDisplay(this.id, this.name, this.width, this.height);
  factory DesktopDisplay.fromJson(Map<String, dynamic> json) => DesktopDisplay(
    json['id'] as int,
    json['name'] as String? ?? '',
    json['width'] as int,
    json['height'] as int,
  );
  final int id;
  final String name;
  final int width;
  final int height;

  @override
  bool operator ==(Object other) =>
      other is DesktopDisplay &&
      other.id == id &&
      other.name == name &&
      other.width == width &&
      other.height == height;
  @override
  int get hashCode => Object.hash(id, name, width, height);
}

@immutable
class DesktopNotice {
  const DesktopNotice(this.code, this.message);
  final DesktopNoticeCode code;
  final String message;

  @override
  bool operator ==(Object other) =>
      other is DesktopNotice && other.code == code && other.message == message;
  @override
  int get hashCode => Object.hash(code, message);
}

/// What to connect to. A remembered computer is named by [computerId] alone:
/// its invitation stays in Rust's vault. A just-added computer carries both,
/// and the invitation wins, because it is not in the vault yet.
@immutable
class DesktopTarget {
  const DesktopTarget({
    this.invitation,
    required this.name,
    this.computerId,
    this.relayOnly = false,
    this.quality = DesktopQuality.balanced,
  }) : assert(invitation != null || computerId != null);
  final String? invitation;
  final String name;
  final String? computerId;
  final bool relayOnly;
  final DesktopQuality quality;

  DesktopTarget withQuality(DesktopQuality quality) => DesktopTarget(
    invitation: invitation,
    name: name,
    computerId: computerId,
    relayOnly: relayOnly,
    quality: quality,
  );

  @override
  bool operator ==(Object other) =>
      other is DesktopTarget &&
      other.invitation == invitation &&
      other.name == name &&
      other.computerId == computerId &&
      other.relayOnly == relayOnly &&
      other.quality == quality;
  @override
  int get hashCode =>
      Object.hash(invitation, name, computerId, relayOnly, quality);

  /// The invitation is a bearer secret: never in a log.
  @override
  String toString() =>
      'DesktopTarget($name, computer: $computerId, '
      'invitation: ${invitation == null ? 'none' : 'redacted'}, '
      'relayOnly: $relayOnly, ${quality.name})';
}

/// [code] is an FFI error code (`HOST_BUSY`) or a viewer stop reason
/// (`transport`).
@immutable
class DesktopFailure {
  const DesktopFailure(this.code, this.message);
  final String code;
  final String message;

  @override
  bool operator ==(Object other) =>
      other is DesktopFailure && other.code == code && other.message == message;
  @override
  int get hashCode => Object.hash(code, message);
  @override
  String toString() => message.isEmpty ? code : message;
}

@immutable
class DesktopSessionView {
  const DesktopSessionView({
    this.target,
    this.phase = DesktopPhase.idle,
    this.setup,
    this.reconnectAttempt = 0,
    this.retryAt,
    this.end,
    this.failure,
    this.controlAllowed = false,
    this.controlWanted = false,
    this.controlling = false,
    this.fresh = false,
    this.secureDesktop = false,
    this.hasPicture = false,
    this.paused = false,
    this.path,
    this.notice,
    this.capabilities = const [],
  });
  final DesktopTarget? target;
  final DesktopPhase phase;
  final DesktopSetupStep? setup;

  /// Above 0 from the first scheduled retry until the next `live`, including
  /// while that retry connects.
  final int reconnectAttempt;
  final DateTime? retryAt;
  final DesktopEndReason? end;
  final DesktopFailure? failure;
  final bool controlAllowed, controlWanted, controlling, fresh;
  final bool secureDesktop, hasPicture, paused;

  /// `direct` or `relay`; null before the path is known.
  final String? path;
  final DesktopNotice? notice;

  /// What the host offers beyond the base protocol, such as `lock`.
  final List<String> capabilities;

  bool get inputLive => fresh && controlling;

  /// The connection is up: live, recovering a stalled picture, or waiting
  /// for the host's desktop. Only then are its figures and a view-only
  /// grant current.
  bool get connected =>
      phase == DesktopPhase.live ||
      phase == DesktopPhase.recovering ||
      phase == DesktopPhase.waitingForDesktop;

  @override
  bool operator ==(Object other) =>
      other is DesktopSessionView &&
      other.target == target &&
      other.phase == phase &&
      other.setup == setup &&
      other.reconnectAttempt == reconnectAttempt &&
      other.retryAt == retryAt &&
      other.end == end &&
      other.failure == failure &&
      other.controlAllowed == controlAllowed &&
      other.controlWanted == controlWanted &&
      other.controlling == controlling &&
      other.fresh == fresh &&
      other.secureDesktop == secureDesktop &&
      other.hasPicture == hasPicture &&
      other.paused == paused &&
      other.path == path &&
      other.notice == notice &&
      listEquals(other.capabilities, capabilities);
  @override
  int get hashCode => Object.hash(
    target,
    phase,
    setup,
    reconnectAttempt,
    retryAt,
    end,
    failure,
    controlAllowed,
    controlWanted,
    controlling,
    fresh,
    secureDesktop,
    hasPicture,
    paused,
    path,
    notice,
    Object.hashAll(capabilities),
  );
}

/// The input space and picture size. [epoch] changes with the generation,
/// the display and every reconnect; input stamped with an older epoch is
/// dropped.
@immutable
class DesktopGeometry {
  const DesktopGeometry({
    required this.epoch,
    required this.generation,
    required this.displayWidth,
    required this.displayHeight,
    required this.videoWidth,
    required this.videoHeight,
  });
  final int epoch, generation;
  final int displayWidth, displayHeight, videoWidth, videoHeight;

  @override
  bool operator ==(Object other) =>
      other is DesktopGeometry &&
      other.epoch == epoch &&
      other.generation == generation &&
      other.displayWidth == displayWidth &&
      other.displayHeight == displayHeight &&
      other.videoWidth == videoWidth &&
      other.videoHeight == videoHeight;
  @override
  int get hashCode => Object.hash(
    epoch,
    generation,
    displayWidth,
    displayHeight,
    videoWidth,
    videoHeight,
  );
}

/// The native texture, which survives reconnects, or a JPEG picture
/// ([textureId] null; the image is `DesktopStore.jpegImage`).
@immutable
class DesktopPicture {
  const DesktopPicture({
    this.textureId,
    required this.aspectRatio,
    required this.codec,
  });
  final int? textureId;
  final double aspectRatio;
  final String codec;

  @override
  bool operator ==(Object other) =>
      other is DesktopPicture &&
      other.textureId == textureId &&
      other.aspectRatio == aspectRatio &&
      other.codec == codec;
  @override
  int get hashCode => Object.hash(textureId, aspectRatio, codec);
}

/// The mirror of `PbDesktopViewV1`: read every rendered frame, never
/// published through a notifier.
@immutable
class DesktopView {
  const DesktopView({
    this.generation = 0,
    this.presentedFrameId = 0,
    this.presentedAgeMs = noAge,
    this.cursorSeq = 0,
    this.displayWidth = 0,
    this.displayHeight = 0,
    this.videoWidth = 0,
    this.videoHeight = 0,
    this.cursorX = 0,
    this.cursorY = 0,
    this.state = 0,
    this.cursorVisible = false,
    this.controlling = false,
    this.controlAllowed = false,
    this.secureDesktop = false,
    this.fresh = false,
    this.paused = false,
  });

  /// `presented_age_ms` when nothing has been presented.
  static const noAge = 0xFFFFFFFF;

  /// [state]: 0 none, 1 connecting, 2 streaming, 3 capture unavailable,
  /// 4 paused, 5 stopped.
  final int generation, presentedFrameId, presentedAgeMs, cursorSeq;
  final int displayWidth, displayHeight, videoWidth, videoHeight;
  final int cursorX, cursorY;
  final int state;
  final bool cursorVisible, controlling, controlAllowed, secureDesktop;
  final bool fresh, paused;

  @override
  bool operator ==(Object other) =>
      other is DesktopView &&
      other.generation == generation &&
      other.presentedFrameId == presentedFrameId &&
      other.presentedAgeMs == presentedAgeMs &&
      other.cursorSeq == cursorSeq &&
      other.displayWidth == displayWidth &&
      other.displayHeight == displayHeight &&
      other.videoWidth == videoWidth &&
      other.videoHeight == videoHeight &&
      other.cursorX == cursorX &&
      other.cursorY == cursorY &&
      other.state == state &&
      other.cursorVisible == cursorVisible &&
      other.controlling == controlling &&
      other.controlAllowed == controlAllowed &&
      other.secureDesktop == secureDesktop &&
      other.fresh == fresh &&
      other.paused == paused;
  @override
  int get hashCode => Object.hash(
    generation,
    presentedFrameId,
    presentedAgeMs,
    cursorSeq,
    displayWidth,
    displayHeight,
    videoWidth,
    videoHeight,
    cursorX,
    cursorY,
    state,
    cursorVisible,
    controlling,
    controlAllowed,
    secureDesktop,
    fresh,
    paused,
  );
}

/// Intervals between accepted presentations over the last 60 s.
@immutable
class DesktopIntervals {
  const DesktopIntervals({
    this.p50Ms = 0,
    this.p95Ms = 0,
    this.p99Ms = 0,
    this.maxMs = 0,
    this.gaps100 = 0,
    this.gaps250 = 0,
  });
  factory DesktopIntervals.fromJson(Map<String, dynamic> json) =>
      DesktopIntervals(
        p50Ms: _double(json['p50_ms']),
        p95Ms: _double(json['p95_ms']),
        p99Ms: _double(json['p99_ms']),
        maxMs: _double(json['max_ms']),
        gaps100: json['gaps_100'] as int? ?? 0,
        gaps250: json['gaps_250'] as int? ?? 0,
      );
  final double p50Ms, p95Ms, p99Ms, maxMs;
  final int gaps100, gaps250;

  @override
  bool operator ==(Object other) =>
      other is DesktopIntervals &&
      other.p50Ms == p50Ms &&
      other.p95Ms == p95Ms &&
      other.p99Ms == p99Ms &&
      other.maxMs == maxMs &&
      other.gaps100 == gaps100 &&
      other.gaps250 == gaps250;
  @override
  int get hashCode => Object.hash(p50Ms, p95Ms, p99Ms, maxMs, gaps100, gaps250);
}

/// The session's metrics, from `ViewerStatus.stats` and its decoder; the
/// store publishes them at most once a second and only while listened to.
@immutable
class DesktopStats {
  const DesktopStats({
    this.path = '',
    this.relayTransport,
    this.rttMs,
    this.baseRttMs,
    this.queueDelayMs,
    this.pictureLatencyMs,
    this.presentedFps = 0,
    this.receivedFps = 0,
    this.paceFps = 0,
    this.targetFps = 0,
    this.fpsCap = 0,
    this.bitrateBps = 0,
    this.receivedBitrateBps = 0,
    this.width = 0,
    this.height = 0,
    this.codec = '',
    this.decoder,
    this.hardwareDecoder,
    this.adaptation = DesktopAdaptation.startup,
    this.degraded = false,
    this.paused = false,
    this.keyframes = 0,
    this.backlogDrops = 0,
    this.presentInterval = const DesktopIntervals(),
  });

  /// [stats] is `ViewerStatus.stats`; the decoder and the pause come from
  /// the status around it.
  factory DesktopStats.fromJson(
    Map<String, dynamic> stats, {
    Map<String, dynamic> decoder = const {},
    bool paused = false,
  }) => DesktopStats(
    path: stats['path'] as String? ?? '',
    relayTransport: stats['relay_transport'] as String?,
    rttMs: stats['rtt_ms'] as int?,
    baseRttMs: stats['base_rtt_ms'] as int?,
    queueDelayMs: stats['queue_delay_ms'] as int?,
    pictureLatencyMs: stats['picture_latency_ms'] as int?,
    presentedFps: _double(stats['presented_fps']),
    receivedFps: _double(stats['received_fps']),
    paceFps: stats['pace_fps'] as int? ?? 0,
    targetFps: stats['target_fps'] as int? ?? 0,
    fpsCap: stats['fps_cap'] as int? ?? 0,
    bitrateBps: stats['bitrate'] as int? ?? 0,
    receivedBitrateBps: stats['received_bitrate'] as int? ?? 0,
    width: stats['width'] as int? ?? 0,
    height: stats['height'] as int? ?? 0,
    codec: stats['codec'] as String? ?? '',
    decoder: decoder['name'] as String?,
    hardwareDecoder: decoder['hardware'] as bool?,
    adaptation: _adaptationFromWire(stats['adaptation']),
    degraded: stats['degraded'] == true,
    paused: paused,
    keyframes: stats['keyframes'] as int? ?? 0,
    backlogDrops: stats['backlog_drops'] as int? ?? 0,
    presentInterval: stats['present_interval'] is Map
        ? DesktopIntervals.fromJson(
            Map<String, dynamic>.from(stats['present_interval'] as Map),
          )
        : const DesktopIntervals(),
  );

  static const none = DesktopStats();

  final String path;
  final String? relayTransport;
  final int? rttMs, baseRttMs, queueDelayMs, pictureLatencyMs;
  final double presentedFps, receivedFps;
  final int paceFps, targetFps, fpsCap;
  final int bitrateBps, receivedBitrateBps;
  final int width, height;
  final String codec;
  final String? decoder;
  final bool? hardwareDecoder;
  final DesktopAdaptation adaptation;
  final bool degraded, paused;
  final int keyframes, backlogDrops;
  final DesktopIntervals presentInterval;

  @override
  bool operator ==(Object other) =>
      other is DesktopStats &&
      other.path == path &&
      other.relayTransport == relayTransport &&
      other.rttMs == rttMs &&
      other.baseRttMs == baseRttMs &&
      other.queueDelayMs == queueDelayMs &&
      other.pictureLatencyMs == pictureLatencyMs &&
      other.presentedFps == presentedFps &&
      other.receivedFps == receivedFps &&
      other.paceFps == paceFps &&
      other.targetFps == targetFps &&
      other.fpsCap == fpsCap &&
      other.bitrateBps == bitrateBps &&
      other.receivedBitrateBps == receivedBitrateBps &&
      other.width == width &&
      other.height == height &&
      other.codec == codec &&
      other.decoder == decoder &&
      other.hardwareDecoder == hardwareDecoder &&
      other.adaptation == adaptation &&
      other.degraded == degraded &&
      other.paused == paused &&
      other.keyframes == keyframes &&
      other.backlogDrops == backlogDrops &&
      other.presentInterval == presentInterval;
  @override
  int get hashCode => Object.hashAll([
    path,
    relayTransport,
    rttMs,
    baseRttMs,
    queueDelayMs,
    pictureLatencyMs,
    presentedFps,
    receivedFps,
    paceFps,
    targetFps,
    fpsCap,
    bitrateBps,
    receivedBitrateBps,
    width,
    height,
    codec,
    decoder,
    hardwareDecoder,
    adaptation,
    degraded,
    paused,
    keyframes,
    backlogDrops,
    presentInterval,
  ]);
}

/// The viewer of this computer, as its host reports it (D24).
@immutable
class DesktopHostViewer {
  const DesktopHostViewer({
    this.controlling = false,
    this.paused = false,
    this.path = 'unknown',
    this.quality = DesktopQuality.balanced,
    required this.since,
  });

  /// `HostViewer`; [since] comes from `started_unix_ms`, so it is stable
  /// for the whole session and a host poll does not change the value.
  factory DesktopHostViewer.fromJson(Map<String, dynamic> json) =>
      DesktopHostViewer(
        controlling: json['controlling'] == true,
        paused: json['paused'] == true,
        path: json['path'] as String? ?? 'unknown',
        quality: desktopQualityFromWire(json['quality']),
        since: DateTime.fromMillisecondsSinceEpoch(
          json['started_unix_ms'] as int? ?? 0,
        ),
      );
  final bool controlling, paused;

  /// `direct`, `relay` or `unknown`.
  final String path;
  final DesktopQuality quality;
  final DateTime since;

  @override
  bool operator ==(Object other) =>
      other is DesktopHostViewer &&
      other.controlling == controlling &&
      other.paused == paused &&
      other.path == path &&
      other.quality == quality &&
      other.since == since;
  @override
  int get hashCode => Object.hash(controlling, paused, path, quality, since);
}

/// This computer as a host.
///
/// Invariant: `sharing || configured` implies `invitation != null`.
@immutable
class DesktopHostView {
  const DesktopHostView({
    this.loaded = false,
    this.supported = false,
    this.sharing = false,
    this.configured = false,
    this.enabling = false,
    this.restarting = false,
    this.background = false,
    this.controlAllowed = false,
    this.trustUnreadable = false,
    this.service,
    this.invitation,
    this.error,
    this.displays = const [],
    this.displayId,
    this.helper = DesktopHelperState.none,
    this.viewer,
  });

  /// False until the first capabilities, host status and trust round trip.
  final bool loaded;

  /// This platform can host.
  final bool supported;

  /// A local host runs, or the background service shares.
  final bool sharing;

  /// Sharing can resume without setup: a grant remembered for the current
  /// relay and namespace, or a configured background host.
  final bool configured;

  /// An enable or resume is in flight; [restarting]: the app is about to
  /// restart into the helper's hand-off.
  final bool enabling, restarting;

  /// The background service owns hosting; display and name are fixed.
  final bool background;
  final bool controlAllowed;

  /// A grant is remembered but cannot be read: Windows cannot unlock it, or
  /// it is invalid. Nothing can share until it is revoked; [error] says why.
  final bool trustUnreadable;
  final String? service, invitation, error;
  final List<DesktopDisplay> displays;
  final int? displayId;
  final DesktopHelperState helper;
  final DesktopHostViewer? viewer;

  @override
  bool operator ==(Object other) =>
      other is DesktopHostView &&
      other.loaded == loaded &&
      other.supported == supported &&
      other.sharing == sharing &&
      other.configured == configured &&
      other.enabling == enabling &&
      other.restarting == restarting &&
      other.background == background &&
      other.controlAllowed == controlAllowed &&
      other.trustUnreadable == trustUnreadable &&
      other.service == service &&
      other.invitation == invitation &&
      other.error == error &&
      listEquals(other.displays, displays) &&
      other.displayId == displayId &&
      other.helper == helper &&
      other.viewer == viewer;
  @override
  int get hashCode => Object.hash(
    loaded,
    supported,
    sharing,
    configured,
    enabling,
    restarting,
    background,
    controlAllowed,
    trustUnreadable,
    service,
    invitation,
    error,
    Object.hashAll(displays),
    displayId,
    helper,
    viewer,
  );

  @override
  String toString() =>
      'DesktopHostView(loaded: $loaded, sharing: $sharing, '
      'configured: $configured, enabling: $enabling, '
      'invitation: ${invitation == null ? 'none' : 'redacted'})';
}

double _double(Object? value) => (value as num?)?.toDouble() ?? 0;
