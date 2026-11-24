import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../state/desktop_models.dart';

/// This computer's own pointer in a bitmap of the remote computer's, from
/// the desktop runners' `pb_mapper/desktop_pointer` channel: Windows
/// (ui/windows/runner/desktop_pointer.cpp), macOS
/// (ui/darwin/DesktopPointer.swift) and Linux
/// (ui/linux/runner/desktop_pointer.cc).
///
/// A cursor shows the moment [set] reaches the platform thread, with no
/// Flutter frame in between, and stays until Flutter activates another
/// cursor, as its mouse tracker does when the pointer enters or leaves a
/// region. The runner holds every cursor until [delete]; the caller bounds
/// how many it keeps.
abstract interface class DesktopPointerCursors {
  /// Creates cursor [id] from [bitmap], made for [devicePixelRatio]
  /// physical pixels a logical one: macOS and Linux show it that many times
  /// smaller in logical pixels, whatever the window's scale is by then, and
  /// Windows, whose cursors are physical pixels, as it is. A cursor the
  /// runner already holds as [id] is replaced: a hot restart starts the ids
  /// again while the runner keeps its cursors. Throws where the runner
  /// cannot.
  Future<void> create(
    int id,
    PointerBitmap bitmap, {
    required double devicePixelRatio,
  });

  /// Shows cursor [id]. Throws for an id [create] did not make.
  Future<void> set(int id);

  /// Frees cursor [id]; one already freed, or never made, is no error.
  Future<void> delete(int id);
}

/// The runners that serve `pb_mapper/desktop_pointer`: the desktop ones.
/// Elsewhere a viewer paints the shapes the OS has no pointer for.
bool get nativePointerCursors =>
    !kIsWeb &&
    switch (defaultTargetPlatform) {
      TargetPlatform.windows ||
      TargetPlatform.macOS ||
      TargetPlatform.linux => true,
      TargetPlatform.android ||
      TargetPlatform.iOS ||
      TargetPlatform.fuchsia => false,
    };

/// The channel-backed cursors.
const DesktopPointerCursors desktopPointerCursors = _ChannelPointerCursors();

/// The lines a notch of this computer's mouse wheel scrolls, where its
/// runner says: Windows' `SPI_GETWHEELSCROLLLINES`, which the runner reads
/// as it starts, when Flutter's embedder reads it for the scroll distance
/// it reports. Null where no runner answers, or it could not read it.
Future<int?> desktopWheelLines() async {
  if (kIsWeb || defaultTargetPlatform != TargetPlatform.windows) return null;
  try {
    return await _ChannelPointerCursors._channel.invokeMethod<int>(
      'wheel_lines',
    );
  } on PlatformException {
    return null;
  } on MissingPluginException {
    return null;
  }
}

final class _ChannelPointerCursors implements DesktopPointerCursors {
  const _ChannelPointerCursors();

  static const _channel = MethodChannel('pb_mapper/desktop_pointer');

  @override
  Future<void> create(
    int id,
    PointerBitmap bitmap, {
    required double devicePixelRatio,
  }) => _channel.invokeMethod<void>('cursor_create', {
    'id': id,
    'rgba': bitmap.rgba,
    'width': bitmap.width,
    'height': bitmap.height,
    'hot_x': bitmap.hotX,
    'hot_y': bitmap.hotY,
    'scale': devicePixelRatio,
  });

  @override
  Future<void> set(int id) =>
      _channel.invokeMethod<void>('cursor_set', {'id': id});

  @override
  Future<void> delete(int id) =>
      _channel.invokeMethod<void>('cursor_delete', {'id': id});
}

/// A cursor bitmap in this computer's physical pixels: straight (not
/// premultiplied) RGBA8, `width × height × 4` bytes, the hotspot inside.
typedef PointerBitmap = ({
  Uint8List rgba,
  int width,
  int height,
  int hotX,
  int hotY,
});

/// [shape] with each of its pixels [scale] of this computer's physical
/// pixels: area-averaged (in premultiplied space, so a transparent pixel
/// lends no colour to its neighbours), which keeps an integer enlargement's
/// edges sharp and a reduction smooth. At 1 the host's own bytes.
PointerBitmap scalePointerBitmap(DesktopCursorShape shape, double scale) {
  final width = math.max(1, (shape.width * scale).round());
  final height = math.max(1, (shape.height * scale).round());
  final hotX = (shape.hotX * width / shape.width).round().clamp(0, width - 1);
  final hotY = (shape.hotY * height / shape.height).round().clamp(
    0,
    height - 1,
  );
  if (width == shape.width && height == shape.height) {
    return (
      rgba: shape.rgba,
      width: width,
      height: height,
      hotX: hotX,
      hotY: hotY,
    );
  }
  final xs = _Taps(shape.width, width), ys = _Taps(shape.height, height);
  final source = shape.rgba;
  final out = Uint8List(width * height * 4);
  for (var oy = 0; oy < height; oy++) {
    for (var ox = 0; ox < width; ox++) {
      var r = 0.0, g = 0.0, b = 0.0, a = 0.0;
      for (var ty = ys.start[oy]; ty < ys.start[oy + 1]; ty++) {
        final row = ys.index[ty] * shape.width;
        final wy = ys.weight[ty];
        for (var tx = xs.start[ox]; tx < xs.start[ox + 1]; tx++) {
          final at = (row + xs.index[tx]) * 4;
          final alpha = source[at + 3] * wy * xs.weight[tx];
          if (alpha == 0) continue;
          r += source[at] * alpha;
          g += source[at + 1] * alpha;
          b += source[at + 2] * alpha;
          a += alpha;
        }
      }
      if (a <= 0) continue;
      final at = (oy * width + ox) * 4;
      out[at] = (r / a).round().clamp(0, 255);
      out[at + 1] = (g / a).round().clamp(0, 255);
      out[at + 2] = (b / a).round().clamp(0, 255);
      out[at + 3] = a.round().clamp(0, 255);
    }
  }
  return (rgba: out, width: width, height: height, hotX: hotX, hotY: hotY);
}

/// The source pixels each output pixel along one axis covers, with the
/// share of it each covers; output pixel `o`'s taps are
/// `start[o]..start[o + 1]`.
final class _Taps {
  _Taps(int from, int to) : start = Int32List(to + 1) {
    final span = from / to;
    final index = <int>[], weight = <double>[];
    for (var o = 0; o < to; o++) {
      start[o] = index.length;
      final lo = o * span, hi = (o + 1) * span;
      for (var s = lo.floor(); s < hi && s < from; s++) {
        final cover = math.min(hi, s + 1.0) - math.max(lo, s.toDouble());
        if (cover <= 0) continue;
        index.add(s);
        weight.add(cover / span);
      }
    }
    start[to] = index.length;
    this.index = Int32List.fromList(index);
    this.weight = Float64List.fromList(weight);
  }

  final Int32List start;
  late final Int32List index;
  late final Float64List weight;
}
