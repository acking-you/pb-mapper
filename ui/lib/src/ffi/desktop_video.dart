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

/// Android and Windows can decode H.264 natively; the session's codec decides
/// (Windows without Media Foundation takes JPEG).
bool get nativeDesktopVideo => Platform.isAndroid || Platform.isWindows;

/// What this device decodes natively, from the channel's `decoders` (I4),
/// which enumerates and never creates a decoder: a bad driver can hang
/// there. Never throws: no answer, or no channel, is H.264 only.
Future<DesktopDecoders> probeDesktopDecoders() async {
  if (!nativeDesktopVideo) return DesktopDecoders.h264Only;
  try {
    final reply = await _ChannelVideoTexture._channel
        .invokeMapMethod<String, Object?>(
          'decoders',
          const <String, Object?>{},
        );
    return reply == null
        ? DesktopDecoders.h264Only
        : DesktopDecoders.fromJson(reply);
  } catch (_) {
    return DesktopDecoders.h264Only;
  }
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
