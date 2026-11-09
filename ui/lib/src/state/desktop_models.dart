// The values the desktop store publishes, and the ones the FFI returns that
// the store passes on unchanged. Every one is immutable with value equality,
// so a `ValueNotifier` holding it notifies only on a real change (D14).
import 'dart:math' as math;

import 'package:flutter/foundation.dart';

/// The quality preset (D12). The names are the wire names.
enum DesktopQuality {
  quality,
  balanced,
  smooth;

  /// The host's frame rate ceiling for this preset, as `qos.rs` sets it:
  /// Quality stays at 30 by design.
  int get ceilingFps => switch (this) {
    quality => 30,
    balanced => 60,
    smooth => 120,
  };
}

DesktopQuality desktopQualityFromWire(Object? value) => switch (value) {
  'quality' => DesktopQuality.quality,
  'smooth' => DesktopQuality.smooth,
  _ => DesktopQuality.balanced,
};

/// The largest picture a viewer asks for (D7), as Rust's `Resolution`: the
/// host keeps the display's aspect ratio inside these bounds and never
/// upscales. Fixed for a session, so a change reconnects. None chosen
/// (null) is the default, [native].
enum DesktopResolution {
  p720('720p', 1280, 720),
  p1080('1080p', 1920, 1080),
  p1440('1440p', 2560, 1440),

  /// The display's own size, up to 3840×2160: the default.
  native('native', 3840, 2160);

  const DesktopResolution(this.wire, this.maxWidth, this.maxHeight);
  final String wire;
  final int maxWidth, maxHeight;

  /// The picture a session at [r] sends of a [display] (width, height):
  /// the display fitted inside the bounds, never upscaled; the bounds
  /// themselves while the display is not known.
  static (int, int) pictureOf(DesktopResolution? r, (int, int)? display) {
    final bounds = r ?? native;
    if (display == null || display.$1 <= 0 || display.$2 <= 0) {
      return (bounds.maxWidth, bounds.maxHeight);
    }
    final scale = [
      1.0,
      bounds.maxWidth / display.$1,
      bounds.maxHeight / display.$2,
    ].reduce((a, b) => a < b ? a : b);
    return ((display.$1 * scale).floor(), (display.$2 * scale).floor());
  }
}

/// Null when there is none, or for a value this build does not know: the
/// default.
DesktopResolution? desktopResolutionFromWire(Object? value) {
  for (final r in DesktopResolution.values) {
    if (r.wire == value) return r;
  }
  return null;
}

/// A display mode's size in physical pixels: the remote resolution (R8),
/// the host display's real mode, which changes live, apart from the picture
/// size above.
@immutable
class DisplayMode {
  const DisplayMode(this.width, this.height);

  static DisplayMode? fromJson(Object? value) => switch (value) {
    {'width': final int width, 'height': final int height} => DisplayMode(
      width,
      height,
    ),
    _ => null,
  };

  final int width, height;

  Map<String, Object?> toJson() => {'width': width, 'height': height};

  String get label => '$width×$height';

  @override
  bool operator ==(Object other) =>
      other is DisplayMode && other.width == width && other.height == height;
  @override
  int get hashCode => Object.hash(width, height);
  @override
  String toString() => label;
}

/// The remote display's modes, from a host that offers them (R17): what it
/// shows, what it showed before a viewer changed it, the sizes it takes,
/// largest first, and the one that best fits this computer's display.
@immutable
class DisplayModeList {
  const DisplayModeList({
    required this.current,
    required this.original,
    this.supported = const [],
    this.fitLocal,
  });

  static DisplayModeList? fromJson(Object? value) {
    if (value is! Map) return null;
    final current = DisplayMode.fromJson(value['current']);
    final original = DisplayMode.fromJson(value['original']);
    if (current == null || original == null) return null;
    return DisplayModeList(
      current: current,
      original: original,
      supported: [
        for (final mode in value['supported'] as List? ?? const [])
          ?DisplayMode.fromJson(mode),
      ],
      fitLocal: DisplayMode.fromJson(value['fit_local']),
    );
  }

  final DisplayMode current, original;
  final List<DisplayMode> supported;

  /// Null where none fits, or this computer's display is not known.
  final DisplayMode? fitLocal;

  /// A viewer changed it, and it is not back yet.
  bool get changed => current != original;

  @override
  bool operator ==(Object other) =>
      other is DisplayModeList &&
      other.current == current &&
      other.original == original &&
      listEquals(other.supported, supported) &&
      other.fitLocal == fitLocal;
  @override
  int get hashCode =>
      Object.hash(current, original, Object.hashAll(supported), fitLocal);
}

/// What a viewer asks the remote display to show.
sealed class DisplayModeTarget {
  const DisplayModeTarget();

  /// What it showed before a viewer changed it.
  const factory DisplayModeTarget.original() = _OriginalMode;

  /// The mode that best fits this computer's display of [local] size.
  const factory DisplayModeTarget.fitLocal(DisplayMode local) = _FitLocalMode;

  /// One of the modes it offers.
  const factory DisplayModeTarget.size(DisplayMode mode) = _SizeMode;

  /// The FFI's `target`.
  Object toJson();

  /// Fit local, which a computer keeps (R19).
  bool get fitsLocal => false;
}

final class _OriginalMode extends DisplayModeTarget {
  const _OriginalMode();
  @override
  Object toJson() => 'original';
}

final class _FitLocalMode extends DisplayModeTarget {
  const _FitLocalMode(this.local);
  final DisplayMode local;
  @override
  bool get fitsLocal => true;
  @override
  Object toJson() => {'fit_local': local.toJson()};
}

final class _SizeMode extends DisplayModeTarget {
  const _SizeMode(this.mode);
  final DisplayMode mode;
  @override
  Object toJson() => {'size': mode.toJson()};
}

/// The frame rates a viewer may choose over Auto (D8): each becomes the
/// session's ceiling, every preset's included.
const kMaxFpsChoices = [30, 60, 90, 120];

/// [kMaxFpsChoices] up to a display of [displayHz], and [chosen] whatever
/// it is, so the choice in force is always listed. A hertz of slack keeps
/// 60 for a 59.94 Hz display, which Windows reads as 59 where it cannot
/// query the exact rate.
List<int> maxFpsChoicesFor(double displayHz, {int chosen = 0}) => [
  for (final fps in kMaxFpsChoices)
    if (fps <= displayHz + 1 || fps == chosen) fps,
];

/// The viewer's codec preference (D6), one for the whole device: whether a
/// computer's picture can be H.265 depends on this device's decoders.
enum DesktopCodecPreference {
  /// H.265 when both ends can, else H.264.
  auto,
  h264,
}

/// The largest picture one of this device's decoders takes.
@immutable
class DesktopDecoderLimit {
  const DesktopDecoderLimit(this.maxWidth, this.maxHeight);
  final int maxWidth, maxHeight;

  /// What the protocol carries at most: no limit of the decoder's own.
  static const largest = DesktopDecoderLimit(3840, 2160);

  /// A picture of [width] by [height] fits, in either orientation: the
  /// probe reports one, and a decoder takes both.
  bool takes(int width, int height) =>
      math.max(width, height) <= math.max(maxWidth, maxHeight) &&
      math.min(width, height) <= math.min(maxWidth, maxHeight);

  /// [bounds] fitted inside this decoder as Rust's `VideoLimits::clamped`
  /// fits them: the long edge within the longer side, the short within the
  /// shorter.
  (int, int) clamp((int, int) bounds) {
    final long = math.max(maxWidth, maxHeight);
    final short = math.min(maxWidth, maxHeight);
    final (w, h) = bounds;
    return w >= h
        ? (math.min(w, long), math.min(h, short))
        : (math.min(w, short), math.min(h, long));
  }

  @override
  bool operator ==(Object other) =>
      other is DesktopDecoderLimit &&
      other.maxWidth == maxWidth &&
      other.maxHeight == maxHeight;
  @override
  int get hashCode => Object.hash(maxWidth, maxHeight);
}

/// What this device decodes natively, as the `pb_mapper/desktop_video`
/// channel's `decoders` answers (I4), which never creates a decoder. A null
/// codec has no hardware decoder here. Once H.265 failed to decode, the
/// store keeps it out for the rest of the run ([withoutH265]).
@immutable
class DesktopDecoders {
  const DesktopDecoders({this.h264, this.h265, this.displayMaxHz});

  factory DesktopDecoders.fromJson(Map<String, Object?> json) {
    DesktopDecoderLimit? limit(Object? value) => switch (value) {
      {'max_width': final int w, 'max_height': final int h}
          when w > 0 && h > 0 =>
        DesktopDecoderLimit(w, h),
      _ => null,
    };
    final hz = json['display_max_hz'];
    return DesktopDecoders(
      h264: limit(json['h264']),
      h265: limit(json['h265']),
      displayMaxHz: hz is num && hz.isFinite && hz > 0 ? hz.toDouble() : null,
    );
  }

  /// No answer, or no channel (another platform): H.264 only, as before
  /// the probe, at any size the protocol carries.
  static const h264Only = DesktopDecoders(h264: DesktopDecoderLimit.largest);

  final DesktopDecoderLimit? h264, h265;

  /// The display's highest refresh rate; null when not reported.
  final double? displayMaxHz;

  DesktopDecoders withoutH265() =>
      DesktopDecoders(h264: h264, displayMaxHz: displayMaxHz);

  /// D6: H.265 is offered when the preference is Auto and there is an
  /// H.265 decoder: Rust fits the session's limits inside its largest
  /// picture, so a phone keeps H.265 at the default size.
  bool offersH265(DesktopTarget target) =>
      target.codec == DesktopCodecPreference.auto && h265 != null;

  /// The decoder whose largest picture bounds a session under [codec]:
  /// H.265's where it would be offered, else H.264's — H.264's alone once
  /// the computer declined H.265 ([h265Declined]). Null for JPEG, which
  /// Dart decodes at any size.
  DesktopDecoderLimit? limitFor(
    DesktopCodecPreference codec, {
    bool h265Declined = false,
  }) => codec == DesktopCodecPreference.auto && !h265Declined && h265 != null
      ? h265
      : h264;

  /// The limits Rust's `ViewerConfig::for_decoders` asks for at [r] when
  /// these decoders are offered.
  (int, int) limitsFor(DesktopResolution? r) {
    final bounds = (
      (r ?? DesktopResolution.native).maxWidth,
      (r ?? DesktopResolution.native).maxHeight,
    );
    return (h265 ?? h264)?.clamp(bounds) ?? bounds;
  }

  /// D7: the picture a session at [r] sends of a [display] (null while not
  /// known) fits the decoder that bounds it ([limitFor]). One that does not
  /// is fitted inside that decoder instead.
  bool decodes(
    DesktopResolution? r,
    DesktopCodecPreference codec, {
    bool h265Declined = false,
    (int, int)? display,
  }) {
    final limit = limitFor(codec, h265Declined: h265Declined);
    if (limit == null) return true;
    final (width, height) = DesktopResolution.pictureOf(r, display);
    return limit.takes(width, height);
  }

  @override
  bool operator ==(Object other) =>
      other is DesktopDecoders &&
      other.h264 == h264 &&
      other.h265 == h265 &&
      other.displayMaxHz == displayMaxHz;
  @override
  int get hashCode => Object.hash(h264, h265, displayMaxHz);
}

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

  /// Attempts kept failing for two minutes; the last failure says how.
  unreachable,
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
/// platform), ready, stopped, needing an update (`update_required`, or
/// `update_available`: another build), or running with an identity that
/// cannot be verified.
enum DesktopHelperState { none, ready, stopped, outdated, unverified }

enum DesktopNoticeCode { locked, lockFailed, displayModeFailed }

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

/// What to connect to, and how: everything a reconnect replays. A
/// remembered computer is named by [computerId] alone: its invitation stays
/// in Rust's vault. A just-added computer carries both, and the invitation
/// wins, because it is not in the vault yet.
@immutable
class DesktopTarget {
  const DesktopTarget({
    this.invitation,
    required this.name,
    this.computerId,
    this.relayOnly = false,
    this.quality = DesktopQuality.balanced,
    this.resolution,
    this.maxFps = 0,
    this.codec = DesktopCodecPreference.auto,
    this.fitLocal = false,
  }) : assert(invitation != null || computerId != null);
  final String? invitation;
  final String name;
  final String? computerId;
  final bool relayOnly;

  /// Live (D12); the store also keeps it apart, as the host reports it.
  final DesktopQuality quality;

  /// Fixed for a session (D7, D8): a change connects again. Null: the
  /// default, the display's own size.
  final DesktopResolution? resolution;

  /// 0: Auto; else one of [kMaxFpsChoices].
  final int maxFps;

  /// The device's preference, which the session takes at connect (D6).
  final DesktopCodecPreference codec;

  /// The computer keeps Fit local as its remote resolution (R19): this
  /// viewer asks for it whenever it gains control.
  final bool fitLocal;

  /// [resolution] is replaced when given, null included: `(to: null)` is
  /// the default again.
  DesktopTarget copyWith({
    DesktopQuality? quality,
    ({DesktopResolution? to})? resolution,
    int? maxFps,
    DesktopCodecPreference? codec,
    bool? fitLocal,
  }) => DesktopTarget(
    invitation: invitation,
    name: name,
    computerId: computerId,
    relayOnly: relayOnly,
    quality: quality ?? this.quality,
    resolution: resolution == null ? this.resolution : resolution.to,
    maxFps: maxFps ?? this.maxFps,
    codec: codec ?? this.codec,
    fitLocal: fitLocal ?? this.fitLocal,
  );

  @override
  bool operator ==(Object other) =>
      other is DesktopTarget &&
      other.invitation == invitation &&
      other.name == name &&
      other.computerId == computerId &&
      other.relayOnly == relayOnly &&
      other.quality == quality &&
      other.resolution == resolution &&
      other.maxFps == maxFps &&
      other.codec == codec &&
      other.fitLocal == fitLocal;
  @override
  int get hashCode => Object.hash(
    invitation,
    name,
    computerId,
    relayOnly,
    quality,
    resolution,
    maxFps,
    codec,
    fitLocal,
  );

  /// The invitation is a bearer secret: never in a log.
  @override
  String toString() =>
      'DesktopTarget($name, computer: $computerId, '
      'invitation: ${invitation == null ? 'none' : 'redacted'}, '
      'relayOnly: $relayOnly, ${quality.name}, ${resolution?.wire ?? 'default'}, '
      'maxFps: $maxFps, ${codec.name})';
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
    this.displayModes,
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

  /// The remote display's modes, from a host that offers them (R17), with
  /// the one that fits this computer's display.
  final DisplayModeList? displayModes;

  bool get inputLive => fresh && controlling;

  /// A target is set and the session has not ended: opening another
  /// computer would end it.
  bool get open =>
      target != null &&
      phase != DesktopPhase.idle &&
      phase != DesktopPhase.ended;

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
      listEquals(other.capabilities, capabilities) &&
      other.displayModes == displayModes;
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
    displayModes,
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

  /// The session's codec, `h264`, `h265` or `jpeg`; through a reconnect,
  /// the last session's.
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

/// A direct path's link (`ViewerStats.link`), as the SDK measured it.
@immutable
class DesktopLink {
  const DesktopLink({
    this.kind = '',
    this.family = '',
    this.via = '',
    this.peer,
    this.localIp,
    this.rttMs,
    this.minRttMs,
    this.setupMs,
    this.sameLan,
  });

  factory DesktopLink.fromJson(Map<String, dynamic> json) => DesktopLink(
    kind: json['kind'] as String? ?? '',
    family: json['family'] as String? ?? '',
    via: json['via'] as String? ?? '',
    peer: json['peer'] as String?,
    localIp: json['local_ip'] as String?,
    rttMs: _int(json['rtt_ms']),
    minRttMs: _int(json['min_rtt_ms']),
    setupMs: _int(json['setup_ms']),
    sameLan: json['same_lan'] as String?,
  );

  /// `lan`, `internet` or `private`.
  final String kind;

  /// `ipv4` or `ipv6`.
  final String family;

  /// How the peer's address was found: `host`, `public`,
  /// `peer_reflexive`, `mapped` or `unknown`.
  final String via;
  final String? peer, localIp;

  /// The QUIC path's own round trip, its lowest, and how long the path
  /// took to set up.
  final int? rttMs, minRttMs, setupMs;

  /// What showed both ends on one LAN: `ipv6_prefix`, `public_ip_subnet`
  /// or `subnet`.
  final String? sameLan;

  bool get lan => kind == 'lan';

  @override
  bool operator ==(Object other) =>
      other is DesktopLink &&
      other.kind == kind &&
      other.family == family &&
      other.via == via &&
      other.peer == peer &&
      other.localIp == localIp &&
      other.rttMs == rttMs &&
      other.minRttMs == minRttMs &&
      other.setupMs == setupMs &&
      other.sameLan == sameLan;
  @override
  int get hashCode => Object.hash(
    kind,
    family,
    via,
    peer,
    localIp,
    rttMs,
    minRttMs,
    setupMs,
    sameLan,
  );
}

/// The session's metrics, from `ViewerStatus.stats` and its decoder; the
/// store publishes them at most once a second and only while listened to.
/// What an older host or a relayed path does not report is null.
@immutable
class DesktopStats {
  const DesktopStats({
    this.path = '',
    this.relayTransport,
    this.rttMs,
    this.baseRttMs,
    this.queueDelayMs,
    this.pictureLatencyMs,
    this.hostMs,
    this.viewerMs,
    this.presentedFps = 0,
    this.receivedFps = 0,
    this.captureFps = 0,
    this.paceFps = 0,
    this.targetFps = 0,
    this.fpsCap = 0,
    this.preset,
    this.maxFps,
    this.skippedPerSecond,
    this.encoderDwellMs,
    this.bitrateBps = 0,
    this.encoderBitrateBps = 0,
    this.receivedBitrateBps = 0,
    this.sentBitrateBps,
    this.speedRxBps,
    this.speedTxBps,
    this.ackQueueMs,
    this.width = 0,
    this.height = 0,
    this.codec = '',
    this.decoder,
    this.hardwareDecoder,
    this.adaptation = DesktopAdaptation.startup,
    this.rateModel = '',
    this.degraded = false,
    this.paused = false,
    this.keyframes = 0,
    this.keyframesRequested = 0,
    this.backlogDrops = 0,
    this.droppedFrames = 0,
    this.presentInterval = const DesktopIntervals(),
    this.link,
  });

  /// [stats] is `ViewerStatus.stats`; the decoder and the pause come from
  /// the status around it, and so may the speed ([status]: `ViewerStatus`
  /// itself), which is read from either.
  factory DesktopStats.fromJson(
    Map<String, dynamic> stats, {
    Map<String, dynamic> decoder = const {},
    bool paused = false,
    Map<String, dynamic> status = const {},
  }) => DesktopStats(
    path: stats['path'] as String? ?? '',
    relayTransport: stats['relay_transport'] as String?,
    rttMs: stats['rtt_ms'] as int?,
    baseRttMs: stats['base_rtt_ms'] as int?,
    queueDelayMs: stats['queue_delay_ms'] as int?,
    pictureLatencyMs: stats['picture_latency_ms'] as int?,
    hostMs: stats['host_ms'] as int?,
    viewerMs: stats['viewer_ms'] as int?,
    presentedFps: _double(stats['presented_fps']),
    receivedFps: _double(stats['received_fps']),
    captureFps: _double(stats['capture_fps']),
    paceFps: stats['pace_fps'] as int? ?? 0,
    targetFps: stats['target_fps'] as int? ?? 0,
    fpsCap: stats['fps_cap'] as int? ?? 0,
    preset: stats['preset'] == null
        ? null
        : desktopQualityFromWire(stats['preset']),
    maxFps: _int(stats['max_fps']),
    skippedPerSecond: (stats['skipped_per_s'] as num?)?.toDouble(),
    encoderDwellMs: _int(stats['encoder_dwell_ms']),
    bitrateBps: stats['bitrate'] as int? ?? 0,
    encoderBitrateBps: stats['encoder_bitrate'] as int? ?? 0,
    receivedBitrateBps: stats['received_bitrate'] as int? ?? 0,
    sentBitrateBps: _int(stats['sent_bitrate']),
    speedRxBps: _int(stats['speed_rx_bps'] ?? status['speed_rx_bps']),
    speedTxBps: _int(stats['speed_tx_bps'] ?? status['speed_tx_bps']),
    ackQueueMs: _int(stats['ack_queue_ms']),
    width: stats['width'] as int? ?? 0,
    height: stats['height'] as int? ?? 0,
    codec: stats['codec'] as String? ?? '',
    decoder: decoder['name'] as String?,
    hardwareDecoder: decoder['hardware'] as bool?,
    adaptation: _adaptationFromWire(stats['adaptation']),
    rateModel: stats['rate_model'] as String? ?? '',
    degraded: stats['degraded'] == true,
    paused: paused,
    keyframes: stats['keyframes'] as int? ?? 0,
    keyframesRequested: stats['keyframes_requested'] as int? ?? 0,
    backlogDrops: stats['backlog_drops'] as int? ?? 0,
    droppedFrames: stats['dropped_frames'] as int? ?? 0,
    presentInterval: stats['present_interval'] is Map
        ? DesktopIntervals.fromJson(
            Map<String, dynamic>.from(stats['present_interval'] as Map),
          )
        : const DesktopIntervals(),
    link: stats['link'] is Map
        ? DesktopLink.fromJson(Map<String, dynamic>.from(stats['link'] as Map))
        : null,
  );

  static const none = DesktopStats();

  final String path;
  final String? relayTransport;

  /// The in-band probe's round trip, through the picture's own stream.
  final int? rttMs, baseRttMs, queueDelayMs, pictureLatencyMs;

  /// The picture delay's ends: the host's, and this viewer's.
  final int? hostMs, viewerMs;

  /// Presented here, received here, and captured by the host.
  final double presentedFps, receivedFps, captureFps;

  /// The host's pace, its controller's target and the viewer's cap (0:
  /// none).
  final int paceFps, targetFps, fpsCap;

  /// The preset this session asked for, and the viewer's display limit.
  final DesktopQuality? preset;
  final int? maxFps;

  /// Host capture ticks skipped a second, and how long a unit stays in its
  /// encoder.
  final double? skippedPerSecond;
  final int? encoderDwellMs;
  final int bitrateBps, encoderBitrateBps, receivedBitrateBps;
  final int? sentBitrateBps, ackQueueMs;

  /// Speed (D10): the session's whole transfer rate each way, in bytes a
  /// second, every message at the tunnel counted, as RustDesk's quality
  /// monitor counts it; null when not reported.
  final int? speedRxBps, speedTxBps;

  /// The encoded size, and `h264`, `h265` or `jpeg`.
  final int width, height;
  final String codec;
  final String? decoder;
  final bool? hardwareDecoder;
  final DesktopAdaptation adaptation;

  /// `nominal` or `timestamp`: how the host's encoder spends its bits.
  final String rateModel;
  final bool degraded, paused;
  final int keyframes, keyframesRequested, backlogDrops, droppedFrames;
  final DesktopIntervals presentInterval;

  /// The direct path's link; null on the relay.
  final DesktopLink? link;

  /// The direct path stays on one LAN.
  bool get lan => link?.lan == true;

  /// The host's rate for a JPEG session, whatever the preset (`qos.rs`
  /// `JPEG_MAX_FPS`).
  static const jpegMaxFps = 8;

  /// The highest rate this session can reach: the preset's ceiling, within
  /// the viewer's display limit, or the frame rate its user [chosen] (D8),
  /// which replaces every preset's ceiling; for JPEG at most [jpegMaxFps].
  int? ceilingFps({int chosen = 0}) {
    var ceiling = chosen > 0 ? chosen : preset?.ceilingFps;
    if (ceiling == null) return null;
    final limit = maxFps;
    if (chosen == 0 && limit != null && limit > 0 && limit < ceiling) {
      ceiling = limit;
    }
    if (codec == 'jpeg' && jpegMaxFps < ceiling) ceiling = jpegMaxFps;
    return ceiling;
  }

  @override
  bool operator ==(Object other) =>
      other is DesktopStats &&
      other.path == path &&
      other.relayTransport == relayTransport &&
      other.rttMs == rttMs &&
      other.baseRttMs == baseRttMs &&
      other.queueDelayMs == queueDelayMs &&
      other.pictureLatencyMs == pictureLatencyMs &&
      other.hostMs == hostMs &&
      other.viewerMs == viewerMs &&
      other.presentedFps == presentedFps &&
      other.receivedFps == receivedFps &&
      other.captureFps == captureFps &&
      other.paceFps == paceFps &&
      other.targetFps == targetFps &&
      other.fpsCap == fpsCap &&
      other.preset == preset &&
      other.maxFps == maxFps &&
      other.skippedPerSecond == skippedPerSecond &&
      other.encoderDwellMs == encoderDwellMs &&
      other.bitrateBps == bitrateBps &&
      other.encoderBitrateBps == encoderBitrateBps &&
      other.receivedBitrateBps == receivedBitrateBps &&
      other.sentBitrateBps == sentBitrateBps &&
      other.speedRxBps == speedRxBps &&
      other.speedTxBps == speedTxBps &&
      other.ackQueueMs == ackQueueMs &&
      other.width == width &&
      other.height == height &&
      other.codec == codec &&
      other.decoder == decoder &&
      other.hardwareDecoder == hardwareDecoder &&
      other.adaptation == adaptation &&
      other.rateModel == rateModel &&
      other.degraded == degraded &&
      other.paused == paused &&
      other.keyframes == keyframes &&
      other.keyframesRequested == keyframesRequested &&
      other.backlogDrops == backlogDrops &&
      other.droppedFrames == droppedFrames &&
      other.presentInterval == presentInterval &&
      other.link == link;
  @override
  int get hashCode => Object.hashAll([
    path,
    relayTransport,
    rttMs,
    baseRttMs,
    queueDelayMs,
    pictureLatencyMs,
    hostMs,
    viewerMs,
    presentedFps,
    receivedFps,
    captureFps,
    paceFps,
    targetFps,
    fpsCap,
    preset,
    maxFps,
    skippedPerSecond,
    encoderDwellMs,
    bitrateBps,
    encoderBitrateBps,
    receivedBitrateBps,
    sentBitrateBps,
    speedRxBps,
    speedTxBps,
    ackQueueMs,
    width,
    height,
    codec,
    decoder,
    hardwareDecoder,
    adaptation,
    rateModel,
    degraded,
    paused,
    keyframes,
    keyframesRequested,
    backlogDrops,
    droppedFrames,
    presentInterval,
    link,
  ]);
}

/// The native decoder's own figures over the last second, from two reads of
/// the `pb_mapper/desktop_video` channel's `stats`. A figure the platform
/// does not report is null: Windows has no timings, credits or refresh.
@immutable
class DesktopDecoderStats {
  const DesktopDecoderStats({
    this.decodedPerSecond,
    this.droppedPerSecond,
    this.surfaceConsumeMs,
    this.credits,
    this.lowLatency,
    this.refreshHz,
  });

  /// The rates between two reads [elapsed] apart. A counter that went back
  /// (another decoder) gives none.
  factory DesktopDecoderStats.between(
    Map<String, Object?> previous,
    Map<String, Object?> current,
    Duration elapsed,
  ) {
    final seconds = elapsed.inMicroseconds / Duration.microsecondsPerSecond;
    double? rate(String key) {
      final (a, b) = (previous[key], current[key]);
      if (a is! num || b is! num || b < a || seconds <= 0) return null;
      return (b - a) / seconds;
    }

    /// A timing's mean over the samples between the reads: each read
    /// carries its count and its mean since the timings began.
    double? meanMs(String key) {
      (num, num)? read(Map<String, Object?> stats) {
        final timings = stats['timings'];
        final timing = timings is Map ? timings[key] : null;
        if (timing is! Map) return null;
        final (n, mean) = (timing['samples'], timing['mean_ms']);
        return n is num && mean is num ? (n, mean) : null;
      }

      final b = read(current);
      if (b == null) return null;
      final a = read(previous) ?? (0, 0);
      final samples = b.$1 - a.$1;
      if (samples <= 0) return null;
      return (b.$1 * b.$2 - a.$1 * a.$2) / samples;
    }

    final credits = current['credits'], latency = current['lowLatency'];
    final refresh = current['refreshHz'];
    return DesktopDecoderStats(
      decodedPerSecond: rate('decoded'),
      droppedPerSecond: rate('dropped'),
      surfaceConsumeMs: meanMs('surface_consume'),
      credits: credits is num ? credits.toInt() : null,
      lowLatency: latency is bool ? latency : null,
      refreshHz: refresh is num ? refresh.toDouble() : null,
    );
  }

  final double? decodedPerSecond, droppedPerSecond;

  /// From a picture's release to the surface until Flutter consumed it.
  final double? surfaceConsumeMs;

  /// Surface credits in use: pictures released to the surface and not yet
  /// consumed.
  final int? credits;
  final bool? lowLatency;

  /// The display mode asked for while viewing.
  final double? refreshHz;

  @override
  bool operator ==(Object other) =>
      other is DesktopDecoderStats &&
      other.decodedPerSecond == decodedPerSecond &&
      other.droppedPerSecond == droppedPerSecond &&
      other.surfaceConsumeMs == surfaceConsumeMs &&
      other.credits == credits &&
      other.lowLatency == lowLatency &&
      other.refreshHz == refreshHz;
  @override
  int get hashCode => Object.hash(
    decodedPerSecond,
    droppedPerSecond,
    surfaceConsumeMs,
    credits,
    lowLatency,
    refreshHz,
  );
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
    this.otherRelay,
    this.unsupportedReason,
    this.displayMode,
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

  /// The relay this computer is shared or paused on, when the settings now
  /// name another relay or credential; null while it is where they publish.
  /// It stays there until moved, which changes its invitation.
  final String? otherRelay;

  /// Why this computer cannot host, as Rust says it: `platform`,
  /// `media_foundation_missing`, `no_display` or `permission_missing`.
  final String? unsupportedReason;

  /// A viewer changed this display's mode: what it is, and what the
  /// owner's Restore puts back.
  final DisplayModeList? displayMode;

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
      other.viewer == viewer &&
      other.otherRelay == otherRelay &&
      other.unsupportedReason == unsupportedReason &&
      other.displayMode == displayMode;
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
    otherRelay,
    unsupportedReason,
    displayMode,
  );

  @override
  String toString() =>
      'DesktopHostView(loaded: $loaded, sharing: $sharing, '
      'configured: $configured, enabling: $enabling, '
      'invitation: ${invitation == null ? 'none' : 'redacted'})';
}

double _double(Object? value) => (value as num?)?.toDouble() ?? 0;

int? _int(Object? value) => (value as num?)?.round();
