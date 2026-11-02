/// The galaxy's sky: a backdrop rendered once per size and theme (ground,
/// nebula, vignette), and three layers of stars that move against the camera.
library;

import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_frame.dart';

/// The nebula program, loaded once; null where shaders are unavailable.
final Future<ui.FragmentProgram?> _program = ui.FragmentProgram.fromAsset(
  'shaders/nebula.frag',
).then<ui.FragmentProgram?>((p) => p, onError: (_) => null);

/// The ground colours of the sky.
({Color top, Color bottom, Color glow}) skyColors(bool dark) => dark
    ? (
        top: const Color(0xFF0B0C0E),
        bottom: const Color(0xFF131418),
        glow: const Color(0xFF1C1D22),
      )
    : (
        top: const Color(0xFFF6F6F4),
        bottom: const Color(0xFFEDEDEA),
        glow: const Color(0xFFFFFFFF),
      );

/// The sky's ground without the nebula: what shows until the image is ready,
/// and where shaders are unavailable.
void paintGround(Canvas canvas, Size size, bool dark) {
  final colors = skyColors(dark);
  final rect = Offset.zero & size;
  canvas.drawRect(
    rect,
    Paint()
      ..shader = LinearGradient(
        begin: Alignment.topCenter,
        end: Alignment.bottomCenter,
        colors: [colors.top, colors.bottom],
      ).createShader(rect),
  );
  // Lit from the centre of the disk.
  canvas.drawRect(
    rect,
    Paint()
      ..shader = RadialGradient(
        center: const Alignment(0, 0.08),
        radius: 0.75,
        colors: [
          colors.glow.withValues(alpha: dark ? 0.55 : 0.9),
          colors.glow.withValues(alpha: 0),
        ],
      ).createShader(rect),
  );
}

/// One canvas owns its backdrop, including an in-flight rasterization.
class BackdropCache {
  Backdrop? _current;
  bool _enabled = false;

  set enabled(bool enabled) {
    _enabled = enabled;
    if (!enabled) dispose();
  }

  Backdrop? of(Size size, bool dark, double ratio, VoidCallback ready) {
    if (!_enabled) return null;
    final current = _current;
    if (current != null &&
        current.size == size &&
        current.dark == dark &&
        current.ratio == ratio) {
      return current.._ready = ready;
    }
    dispose();
    final next = _current = Backdrop._(size, dark, ratio).._ready = ready;
    next._rendered = next._render();
    return next;
  }

  void dispose() {
    _current?._dispose();
    _current = null;
  }
}

/// The backdrop image for one size and theme, rendered once and drawn as an
/// image every frame after: the nebula's six octaves of noise per pixel are
/// far too dear to run sixty times a second.
class Backdrop {
  Backdrop._(this.size, this.dark, this.ratio);

  final Size size;
  final bool dark;
  final double ratio;

  ui.Image? _image;
  bool _disposed = false;
  VoidCallback? _ready;
  late final Future<void> _rendered;

  /// Allows tests to wait for a pending render after its canvas is hidden.
  @visibleForTesting
  Future<void> get debugRendered => _rendered;

  Future<void> _render() async {
    final program = await _program;
    if (_disposed || size.isEmpty) return;
    // Half resolution is plenty for smoke and keeps the render quick.
    final scale = math.min(ratio, 1.5) * 0.5;
    final width = math.max(1, (size.width * scale).round());
    final height = math.max(1, (size.height * scale).round());
    final recorder = ui.PictureRecorder();
    final canvas = Canvas(recorder);
    ui.FragmentShader? shader;
    canvas.scale(scale);
    paintGround(canvas, size, dark);
    if (program != null) {
      shader = program.fragmentShader()
        ..setFloat(0, size.width)
        ..setFloat(1, size.height)
        ..setFloat(2, 17.0)
        ..setFloat(3, dark ? 1 : 0);
      canvas.drawRect(Offset.zero & size, Paint()..shader = shader);
    }
    // A vignette keeps the eye on the disk.
    canvas.drawRect(
      Offset.zero & size,
      Paint()
        ..shader = RadialGradient(
          radius: 0.95,
          colors: [
            Colors.transparent,
            (dark ? Colors.black : const Color(0xFF8A8A86)).withValues(
              alpha: dark ? 0.55 : 0.10,
            ),
          ],
          stops: const [0.55, 1],
        ).createShader(Offset.zero & size),
    );
    final picture = recorder.endRecording();
    try {
      final image = await picture.toImage(width, height);
      if (_disposed) {
        image.dispose();
        return;
      }
      _image = image;
      _ready?.call();
    } catch (_) {
      // No rasteriser (tests): the ground is painted live instead.
    } finally {
      picture.dispose();
      shader?.dispose();
    }
  }

  void _dispose() {
    _disposed = true;
    _ready = null;
    _image?.dispose();
    _image = null;
  }

  /// Draw it, or the plain ground until it is ready.
  void paint(Canvas canvas) {
    final image = _image;
    if (image == null) {
      paintGround(canvas, size, dark);
      return;
    }
    canvas.drawImageRect(
      image,
      Rect.fromLTWH(0, 0, image.width.toDouble(), image.height.toDouble()),
      Offset.zero & size,
      Paint()..filterQuality = FilterQuality.medium,
    );
  }
}

class _StarLayer {
  _StarLayer(this.points, this.parallax, this.size, this.phases);

  /// Per bucket: interleaved unit x, y.
  final List<Float32List> points;
  final double parallax;
  final double size;
  final List<double> phases;
}

/// Three layers of stars, far to near, twinkling in four brightness buckets
/// so a whole layer is one draw call. Where each star is this frame is
/// computed with the rest of the frame; this draws the result.
class Starfield {
  Starfield() : _layers = _generate();

  final List<_StarLayer> _layers;

  static List<_StarLayer> _generate() {
    final random = math.Random(4242);
    _StarLayer layer(int count, double parallax, double size) {
      final buckets = List.generate(4, (_) => <double>[]);
      for (var i = 0; i < count; i++) {
        buckets[i % 4]
          ..add(random.nextDouble())
          ..add(random.nextDouble());
      }
      return _StarLayer(
        [for (final b in buckets) Float32List.fromList(b)],
        parallax,
        size,
        [for (var i = 0; i < 4; i++) random.nextDouble() * math.pi * 2],
      );
    }

    return [layer(320, 0.12, 0.9), layer(150, 0.3, 1.35), layer(56, 0.62, 2.0)];
  }

  /// The layers as the frame computation takes them.
  List<StarLayerData> get data => [
    for (final layer in _layers)
      (
        parallax: layer.parallax,
        buckets: [
          for (var b = 0; b < layer.points.length; b++)
            (phase: layer.phases[b], points: layer.points[b]),
        ],
      ),
  ];

  /// Paint every layer from the frame at [frame]'s position.
  void paint(Canvas canvas, FrameReader frame, {required bool dark}) {
    final base = dark ? Colors.white : const Color(0xFF2A2A2C);
    for (final layer in _layers) {
      final strength = dark
          ? 0.32 + layer.parallax * 0.75
          : 0.10 + layer.parallax * 0.32;
      for (var b = 0; b < layer.points.length; b++) {
        final twinkle = frame.next();
        final points = frame.points(frame.count());
        canvas.drawRawPoints(
          ui.PointMode.points,
          points,
          Paint()
            ..color = base.withValues(alpha: (strength * twinkle).clamp(0, 1))
            ..strokeWidth = layer.size
            ..strokeCap = StrokeCap.round,
        );
      }
    }
  }
}
