import 'dart:async';
import 'dart:io';

import 'package:flutter/services.dart';

/// Acknowledges consumption by the native texture, never queue acceptance.
class DesktopVideoEvent {
  const DesktopVideoEvent({
    required this.generation,
    required this.frameId,
    required this.width,
    required this.height,
    required this.presented,
    required this.needKeyframe,
    this.message = '',
  });
  final int generation, frameId, width, height;
  final bool presented, needKeyframe;
  final String message;

  factory DesktopVideoEvent.fromMap(Map<Object?, Object?> value) {
    int integer(String key) => (value[key] as num?)?.toInt() ?? 0;
    return DesktopVideoEvent(
      generation: integer('generation'),
      frameId: integer('frameId'),
      width: integer('width'),
      height: integer('height'),
      presented: value['presented'] == true,
      needKeyframe: value['needKeyframe'] == true,
      message: value['message'] as String? ?? '',
    );
  }
}

/// Native H.264 decoder and Flutter texture. Complete Annex-B access units are
/// submitted in order; reset before reusing a texture for a different session.
class DesktopVideoTexture {
  DesktopVideoTexture._(this.textureId);
  static const _channel = MethodChannel('pb_mapper/desktop_video');
  static final _textures = <int, DesktopVideoTexture>{};
  static bool _initialized = false;
  static bool get supported => Platform.isAndroid || Platform.isWindows;

  final int textureId;
  final _events = StreamController<DesktopVideoEvent>.broadcast();
  Stream<DesktopVideoEvent> get events => _events.stream;
  bool _disposed = false;

  static Future<DesktopVideoTexture> create() async {
    if (!supported) {
      throw UnsupportedError('Native desktop video unavailable');
    }
    if (!_initialized) {
      _channel.setMethodCallHandler((call) async {
        if (call.method != 'event' || call.arguments is! Map) return;
        final values = Map<Object?, Object?>.from(call.arguments as Map);
        final id = (values['textureId'] as num?)?.toInt();
        final texture = _textures[id];
        if (texture != null && !texture._disposed) {
          texture._events.add(DesktopVideoEvent.fromMap(values));
        }
      });
      _initialized = true;
    }
    final id = await _channel.invokeMethod<int>('create');
    if (id == null || id < 0) {
      throw StateError('Native texture creation failed');
    }
    return _textures[id] = DesktopVideoTexture._(id);
  }

  Future<void> submit({
    required Uint8List bytes,
    required int generation,
    required int frameId,
    required int ptsUs,
    required int width,
    required int height,
    required bool keyframe,
  }) async {
    if (_disposed) return;
    await _channel.invokeMethod<void>('submit', {
      'textureId': textureId,
      'bytes': bytes,
      'generation': generation,
      'frameId': frameId,
      'ptsUs': ptsUs,
      'width': width,
      'height': height,
      'keyframe': keyframe,
    });
  }

  Future<void> reset() async {
    if (!_disposed) {
      await _channel.invokeMethod<void>('reset', {'textureId': textureId});
    }
  }

  Future<Map<Object?, Object?>> stats() async {
    if (_disposed) return const {};
    return await _channel.invokeMapMethod<Object?, Object?>('stats', {
          'textureId': textureId,
        }) ??
        const {};
  }

  Future<void> dispose() async {
    if (_disposed) return;
    _disposed = true;
    _textures.remove(textureId);
    try {
      await _channel.invokeMethod<void>('dispose', {'textureId': textureId});
    } finally {
      await _events.close();
    }
  }
}
