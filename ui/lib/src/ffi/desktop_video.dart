import 'dart:io';

import 'package:flutter/services.dart';

import '../state/desktop_models.dart';

/// The native decoder (H.264 or H.265, as each unit says) and how its
/// pictures reach the screen: a Flutter texture, or, where the runner
/// presents them itself, a platform view. The decoder pulls video from Rust
/// itself and receipts presentations there; Dart only binds it to a source,
/// snapshots it, greys it, and disposes it.
abstract interface class DesktopVideoTexture {
  /// The texture's id, which a presenting runner gives its platform view in
  /// the `textureId` creation parameter.
  int get textureId;

  /// The platform view that shows the pictures where the runner presents
  /// them itself (the macOS layer presenter); null for a Flutter texture.
  String? get platformViewType;

  /// Binds the decoder to a viewer's video source. A reconnect binds the new
  /// source; the texture and its last picture persist.
  Future<void> attach(int videoSource);

  /// Greys the picture a runner presents itself, which no Flutter filter
  /// reaches (the viewer's grey still while it reconnects); nothing for a
  /// texture.
  Future<void> grey(bool on);

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
    reply = await _ChannelVideoTexture._channel
        .invokeMapMethod<String, Object?>(
          'decoders',
          const <String, Object?>{},
        );
  } catch (_) {
    reply = null;
  }
  return decodersFromProbe(
    reply,
    android: Platform.isAndroid,
    linux: Platform.isLinux && !_linuxSoftwareRenderer,
  );
}

/// Flutter's software renderer on Linux, which draws no external texture:
/// the runner then decodes nothing natively, whatever is installed
/// (`SoftwareRenderer` in desktop_video.cc).
bool get _linuxSoftwareRenderer =>
    Platform.environment['FLUTTER_LINUX_RENDERER'] == 'software';

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

/// How a macOS viewer presents: Flutter's texture, unless the build says
/// `--dart-define=presenter=layer`. The layer stays opt-in until a run with
/// the display on has measured its time to the screen and checked it there
/// (docs/benchmarks/2026-11-24-mac-viewer-layer).
const _macPresenter = String.fromEnvironment(
  'presenter',
  defaultValue: 'texture',
);

/// What `create` asks of this platform's runner: with the layer define, the
/// macOS runner commits each decoded picture to its own Core Animation
/// layer, which a platform view shows; otherwise, and on every other
/// platform, the runner hands Flutter a texture.
String get desktopVideoPresenter =>
    Platform.isMacOS && _macPresenter == 'layer' ? 'layer' : 'texture';

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
  final id = await _ChannelVideoTexture._channel.invokeMethod<int>('create', {
    'instance': instanceToken,
    'decoderHints': decoderHints,
    // Only the macOS runner reads it.
    if (Platform.isMacOS) 'presenter': presenter,
  });
  if (id == null || id < 0) {
    throw StateError('Native texture creation failed');
  }
  return _ChannelVideoTexture(
    id,
    platformViewType: presenter == 'layer' ? desktopVideoViewType : null,
  );
}

class _ChannelVideoTexture implements DesktopVideoTexture {
  _ChannelVideoTexture(this.textureId, {this.platformViewType});
  static const _channel = MethodChannel('pb_mapper/desktop_video');

  @override
  final int textureId;
  @override
  final String? platformViewType;
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
  Future<void> grey(bool on) async {
    if (_disposed || platformViewType == null) return;
    await _channel.invokeMethod<void>('grey', {
      'textureId': textureId,
      'on': on,
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
