import 'dart:io';

import 'package:flutter/services.dart';

import '../state/desktop_models.dart';

/// The native decoder (H.264 or H.265, as each unit says) and its Flutter
/// texture. The decoder pulls video from Rust itself and receipts
/// presentations there; Dart only binds it to a source, snapshots it, and
/// disposes it.
abstract interface class DesktopVideoTexture {
  int get textureId;

  /// Binds the decoder to a viewer's video source. A reconnect binds the new
  /// source; the texture and its last picture persist.
  Future<void> attach(int videoSource);

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
  return decodersFromProbe(reply, android: Platform.isAndroid);
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
DesktopDecoders decodersFromProbe(
  Map<String, Object?>? reply, {
  required bool android,
}) {
  final probed = reply == null
      ? const DesktopDecoders()
      : DesktopDecoders.fromJson(reply);
  if (!android || probed.h264 != null) return probed;
  return DesktopDecoders(
    h264: DesktopDecoderLimit.p1080,
    h265: probed.h265,
    displayMaxHz: probed.displayMaxHz,
  );
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
  final id = await _ChannelVideoTexture._channel.invokeMethod<int>('create', {
    'instance': instanceToken,
    'decoderHints': decoderHints,
  });
  if (id == null || id < 0) {
    throw StateError('Native texture creation failed');
  }
  return _ChannelVideoTexture(id);
}

class _ChannelVideoTexture implements DesktopVideoTexture {
  _ChannelVideoTexture(this.textureId);
  static const _channel = MethodChannel('pb_mapper/desktop_video');

  @override
  final int textureId;
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
