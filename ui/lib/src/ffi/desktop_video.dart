import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../state/desktop_models.dart';

/// Where the viewer shows a plane's picture ([DesktopVideoTexture.plane]),
/// in the Flutter view's physical pixels: the picture's rectangle, and the
/// part of it on screen, each on whole pixels. Hidden when the viewer did not
/// leave its hole in the last frame, or left it where no hole reaches the
/// window (inside an opacity or a filter, as a route transition draws it).
@immutable
class VideoPlacement {
  const VideoPlacement({
    required this.visible,
    this.picture = Rect.zero,
    this.clip = Rect.zero,
  });

  static const hidden = VideoPlacement(visible: false);

  final bool visible;
  final Rect picture;
  final Rect clip;

  /// The `place` call's arguments for [textureId].
  Map<String, Object?> toChannel(int textureId) => {
    'textureId': textureId,
    'visible': visible,
    if (visible) ...{
      'x': picture.left.round(),
      'y': picture.top.round(),
      'w': picture.width.round(),
      'h': picture.height.round(),
      'clipX': clip.left.round(),
      'clipY': clip.top.round(),
      'clipW': clip.width.round(),
      'clipH': clip.height.round(),
    },
  };

  @override
  bool operator ==(Object other) =>
      other is VideoPlacement &&
      other.visible == visible &&
      (!visible || (other.picture == picture && other.clip == clip));

  @override
  int get hashCode =>
      visible ? Object.hash(visible, picture, clip) : visible.hashCode;

  @override
  String toString() => visible
      ? 'VideoPlacement(picture: $picture, clip: $clip)'
      : 'VideoPlacement.hidden';
}

/// The native decoder (H.264 or H.265, as each unit says) and how its
/// pictures reach the screen: a Flutter texture, or, where the runner
/// presents them itself, a platform view or a plane under the Flutter view.
/// The decoder pulls video from Rust itself and receipts presentations
/// there; Dart only binds it to a source, places, snapshots, greys and
/// enhances it, and disposes it.
abstract interface class DesktopVideoTexture {
  /// The texture's id, which a presenting runner gives its platform view in
  /// the `textureId` creation parameter.
  int get textureId;

  /// The platform view that shows the pictures where the runner presents
  /// them itself (the macOS layer presenter); null for a Flutter texture.
  String? get platformViewType;

  /// Whether the runner draws the pictures on its own plane under the
  /// Flutter view (the Linux video plane): the viewer leaves a transparent
  /// hole where the picture goes and [place]s the plane there.
  bool get plane;

  /// Binds the decoder to a viewer's video source. A reconnect binds the new
  /// source; the texture and its last picture persist.
  Future<void> attach(int videoSource);

  /// Where the plane shows the picture; nothing without a [plane].
  Future<void> place(VideoPlacement placement);

  /// Greys the picture a runner presents itself, which no Flutter filter
  /// reaches (the viewer's grey still while it reconnects); nothing for a
  /// texture.
  Future<void> grey(bool on);

  /// Whether the runner can sharpen the picture while it shows it enlarged
  /// ([enhance]): the macOS layer presenter and the Linux video plane.
  bool get enhances;

  /// How much the runner sharpens the picture while it shows it at least
  /// its own size; nothing where it cannot ([enhances]). Every texture
  /// starts at [PictureEnhancement.off].
  Future<void> enhance(PictureEnhancement level);

  /// Writes the last presented picture as a JPEG at most [maxWidth] wide
  /// (D23). False when there is none. The caller bounds the wait.
  Future<bool> snapshot(String path, {int maxWidth = 320});

  /// The decoder's counters. [timings] starts its timing accumulators, or
  /// keeps them; false drops them until the next read that asks.
  Future<Map<String, Object?>> stats({bool timings = true});
  Future<void> dispose();
}

/// [instanceToken] proves the native side uses the library Dart loaded (D9).
typedef DesktopTextureFactory =
    Future<DesktopVideoTexture> Function({required int instanceToken});

/// The runners that decode H.264 natively, and H.265 where they can: Android
/// (MediaCodec), Windows (Media Foundation), macOS and iOS (VideoToolbox),
/// Linux (GStreamer). The session's codec decides; a device whose probe
/// offers nothing (Windows without Media Foundation, Linux without GStreamer
/// or a decoder plugin) takes JPEG ([decodersFromProbe]).
bool get nativeDesktopVideo =>
    Platform.isAndroid ||
    Platform.isWindows ||
    Platform.isMacOS ||
    Platform.isIOS ||
    Platform.isLinux;

/// What this device decodes natively, from the channel's `decoders` (I4),
/// which enumerates and never creates a decoder: a bad driver can hang
/// there. Never throws: no answer, or no channel, is
/// [unprobedDesktopDecoders].
Future<DesktopDecoders> probeDesktopDecoders() async {
  if (!nativeDesktopVideo) return const DesktopDecoders();
  Map<String, Object?>? reply;
  try {
    reply = await _videoChannel.invokeMapMethod<String, Object?>(
      'decoders',
      const <String, Object?>{},
    );
  } catch (_) {
    reply = null;
  }
  return decodersFromProbe(
    reply,
    android: Platform.isAndroid,
    linux:
        Platform.isLinux &&
        !_linuxSoftwareRenderer &&
        _linuxDecoderModuleBundled,
  );
}

/// Flutter's software renderer on Linux, which draws no external texture:
/// the runner then decodes nothing natively, whatever is installed
/// (`SoftwareRenderer` in desktop_video.cc).
bool get _linuxSoftwareRenderer =>
    Platform.environment['FLUTTER_LINUX_RENDERER'] == 'software';

/// Whether this app was built with its GStreamer decoder module, which the
/// runner loads from the bundle's `lib` (`LoadModule` in desktop_video.cc;
/// every release bundle has it). An app built where GStreamer's development
/// files were missing has none and decodes nothing natively, whatever is
/// installed, so an install would add nothing.
bool get _linuxDecoderModuleBundled {
  try {
    final bundle = File(Platform.resolvedExecutable).parent.path;
    return File('$bundle/lib/libpb_mapper_desktop_video.so').existsSync();
  } catch (_) {
    return false;
  }
}

/// What a probe that failed, or has not answered yet, stands for on this
/// device ([decodersFromProbe]).
DesktopDecoders get unprobedDesktopDecoders =>
    decodersFromProbe(null, android: Platform.isAndroid);

/// The decoders a `decoders` [reply] reports; null is a probe that failed or
/// never answered. Android requires an AVC decoder of every device, so where
/// its probe found no hardware one, or failed, MediaCodec still decodes H.264
/// in software, at 1080p, as before the probe. Anywhere else a failed probe
/// offers no decoder and the picture is JPEG, which Dart decodes: a runner
/// that cannot answer may have no decoder at all.
///
/// [linux]: a Linux runner's answer, whose missing decoders a package
/// install may add ([DesktopDecoders.missing]): no H.264 is GStreamer's
/// decoders missing, no H.265 a hardware decoder. A probe that failed says
/// nothing of what is installed.
DesktopDecoders decodersFromProbe(
  Map<String, Object?>? reply, {
  required bool android,
  bool linux = false,
}) {
  final probed = reply == null
      ? const DesktopDecoders()
      : DesktopDecoders.fromJson(reply);
  final missing = {
    if (linux && reply != null) ...{
      if (probed.h264 == null) 'h264',
      if (probed.h265 == null) 'h265',
    },
  };
  if (missing.isEmpty && (!android || probed.h264 != null)) return probed;
  return DesktopDecoders(
    h264: android ? probed.h264 ?? DesktopDecoderLimit.p1080 : probed.h264,
    h265: probed.h265,
    displayMaxHz: probed.displayMaxHz,
    windowRefresh: probed.windowRefresh,
    installable: probed.installable,
    missing: missing,
  );
}

/// The platform view a macOS runner shows its layer presenter in.
const desktopVideoViewType = 'pb_mapper/desktop_video_view';

/// How a viewer presents: Flutter's texture, unless the build says
/// `--dart-define=presenter=layer` (macOS) or `--dart-define=presenter=plane`
/// (Linux). Each stays opt-in until a run on a lit display has measured its
/// time to the screen and checked it there
/// (docs/benchmarks/2026-11-24-mac-viewer-layer, and the Linux viewer in
/// scripts/test/desktop-ui-acceptance.md).
const _presenter = String.fromEnvironment('presenter', defaultValue: 'texture');

/// What `create` asks of this platform's runner: with the layer define, the
/// macOS runner commits each decoded picture to its own Core Animation
/// layer, which a platform view shows; with the plane define, the Linux
/// runner draws them on its own plane under the Flutter view, which it may
/// refuse; otherwise, and on every other platform, the runner hands Flutter
/// a texture.
String get desktopVideoPresenter =>
    presenterFor(_presenter, macOS: Platform.isMacOS, linux: Platform.isLinux);

/// [desktopVideoPresenter] for the define [asked] on a platform.
@visibleForTesting
String presenterFor(String asked, {required bool macOS, required bool linux}) =>
    switch (asked) {
      'layer' when macOS => 'layer',
      'plane' when linux => 'plane',
      _ => 'texture',
    };

/// The texture a `create` [reply] names: the id, as every runner but Linux's
/// answers, or `{textureId, plane}`, as the Linux runner does, `plane` true
/// only when its video plane shows the pictures. Null for no texture.
@visibleForTesting
({int textureId, bool plane})? createdTexture(Object? reply) {
  if (reply is int) return reply < 0 ? null : (textureId: reply, plane: false);
  if (reply is! Map) return null;
  final id = reply['textureId'];
  if (id is! int || id < 0) return null;
  return (textureId: id, plane: reply['plane'] == true);
}

/// The channel-backed texture (`pb_mapper/desktop_video` v2). [decoderHints]
/// false configures Android's decoder without any of its decoder hints (low
/// latency, realtime priority, operating rate, frame rate); only the
/// acceptance tool's case F sends it.
Future<DesktopVideoTexture> createDesktopVideoTexture({
  required int instanceToken,
  bool decoderHints = true,
}) async {
  if (!nativeDesktopVideo) {
    throw UnsupportedError('Native desktop video unavailable');
  }
  final presenter = desktopVideoPresenter;
  final reply = await _videoChannel.invokeMethod<Object?>('create', {
    'instance': instanceToken,
    'decoderHints': decoderHints,
    // Only the macOS and Linux runners read it.
    if (Platform.isMacOS || Platform.isLinux) 'presenter': presenter,
  });
  final created = createdTexture(reply);
  if (created == null) {
    throw StateError('Native texture creation failed');
  }
  return _ChannelVideoTexture(
    created.textureId,
    platformViewType: presenter == 'layer' ? desktopVideoViewType : null,
    plane: created.plane,
  );
}

const _videoChannel = MethodChannel('pb_mapper/desktop_video');

/// The channel's texture [textureId] as `create` makes it for a presenter:
/// a platform view's ([platformViewType]), a [plane], or Flutter's.
@visibleForTesting
DesktopVideoTexture channelVideoTexture(
  int textureId, {
  String? platformViewType,
  bool plane = false,
}) => _ChannelVideoTexture(
  textureId,
  platformViewType: platformViewType,
  plane: plane,
);

class _ChannelVideoTexture implements DesktopVideoTexture {
  _ChannelVideoTexture(
    this.textureId, {
    this.platformViewType,
    this.plane = false,
  });
  static const _channel = _videoChannel;

  @override
  final int textureId;
  @override
  final String? platformViewType;
  @override
  final bool plane;
  bool _disposed = false;

  @override
  Future<void> attach(int videoSource) async {
    if (_disposed) return;
    await _channel.invokeMethod<void>('attach', {
      'textureId': textureId,
      'source': videoSource,
    });
  }

  @override
  Future<void> place(VideoPlacement placement) async {
    if (_disposed || !plane) return;
    await _channel.invokeMethod<void>('place', placement.toChannel(textureId));
  }

  @override
  Future<void> grey(bool on) async {
    if (_disposed || (platformViewType == null && !plane)) return;
    await _channel.invokeMethod<void>('grey', {
      'textureId': textureId,
      'on': on,
    });
  }

  /// The macOS layer presenter's enhancement (DesktopVideo.swift), and the
  /// Linux video plane's (desktop_video_plane.cc), which also area-averages
  /// a picture it shows smaller whatever the level.
  @override
  bool get enhances => platformViewType != null || plane;

  @override
  Future<void> enhance(PictureEnhancement level) async {
    if (_disposed || !enhances) return;
    await _channel.invokeMethod<void>('enhance', {
      'textureId': textureId,
      'level': level.name,
    });
  }

  @override
  Future<bool> snapshot(String path, {int maxWidth = 320}) async {
    if (_disposed) return false;
    return await _channel.invokeMethod<bool>('snapshot', {
          'textureId': textureId,
          'path': path,
          'maxWidth': maxWidth,
        }) ==
        true;
  }

  @override
  Future<Map<String, Object?>> stats({bool timings = true}) async {
    if (_disposed) return const {};
    final stats = await _channel.invokeMapMethod<String, Object?>('stats', {
      'textureId': textureId,
      'timings': timings,
    });
    return stats ?? const {};
  }

  @override
  Future<void> dispose() async {
    if (_disposed) return;
    _disposed = true;
    await _channel.invokeMethod<void>('dispose', {'textureId': textureId});
  }
}
