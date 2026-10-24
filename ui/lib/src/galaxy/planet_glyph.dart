/// A small lit sphere: how a service looks wherever it appears, in a list row
/// or in the galaxy. Its colour is the service's state.
library;

import 'dart:math' as math;

import 'package:flutter/material.dart';

/// Paint one sphere of [radius] at [center], lit from the upper left.
///
/// Dark themes let it glow in its colour; light themes give it a glossy
/// ceramic body with a soft shadow below, which reads on paper-white. Each
/// layer keeps its own opacity, so a faded sphere stays as translucent as its
/// parts instead of hiding its glow behind its body.
void paintSphere(
  Canvas canvas,
  Offset center,
  double radius,
  Color color, {
  required bool dark,
  double glow = 1,
  double opacity = 1,
}) {
  if (radius <= 0 || opacity <= 0) return;
  final alpha = opacity.clamp(0.0, 1.0);
  if (dark && glow > 0) {
    softDisc(
      canvas,
      center,
      radius * 1.6,
      radius * 1.1,
      color.withValues(alpha: 0.30 * glow * alpha),
    );
  } else if (!dark) {
    canvas.drawOval(
      Rect.fromCenter(
        center: center + Offset(radius * 0.15, radius * 0.95),
        width: radius * 1.9,
        height: radius * 0.55,
      ),
      Paint()
        ..color = Colors.black.withValues(alpha: 0.10 * alpha)
        ..maskFilter = MaskFilter.blur(BlurStyle.normal, radius * 0.35),
    );
  }
  final highlight = Color.lerp(color, Colors.white, dark ? 0.55 : 0.75)!;
  final shade = Color.lerp(color, Colors.black, dark ? 0.62 : 0.30)!;
  final body = Rect.fromCircle(center: center, radius: radius);
  canvas.drawCircle(
    center,
    radius,
    Paint()
      ..shader = RadialGradient(
        center: const Alignment(-0.38, -0.42),
        radius: 1.05,
        colors: [
          highlight.withValues(alpha: alpha),
          color.withValues(alpha: alpha),
          shade.withValues(alpha: alpha),
        ],
        stops: const [0, 0.48, 1],
      ).createShader(body),
  );
  // Rim light from the far side keeps the edge readable on any ground.
  final rimAlpha = (dark ? 0.22 : 0.5) * alpha;
  canvas.drawCircle(
    center,
    radius * 0.94,
    Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = math.max(0.6, radius * 0.08)
      ..shader = SweepGradient(
        colors: [
          Colors.white.withValues(alpha: 0),
          Colors.white.withValues(alpha: rimAlpha),
          Colors.white.withValues(alpha: 0),
        ],
        stops: const [0.05, 0.22, 0.42],
      ).createShader(body),
  );
  // The specular point.
  softDisc(
    canvas,
    center + Offset(-radius * 0.36, -radius * 0.40),
    radius * 0.18,
    radius * 0.12,
    Colors.white.withValues(alpha: (dark ? 0.55 : 0.8) * alpha),
  );
}

/// A disc of [radius] blurred by [sigma]: glows and halos.
void softDisc(
  Canvas canvas,
  Offset center,
  double radius,
  double sigma,
  Color color,
) {
  canvas.drawCircle(
    center,
    radius,
    Paint()
      ..color = color
      ..maskFilter = MaskFilter.blur(BlurStyle.normal, math.max(0.1, sigma)),
  );
}

/// A sphere in a square box, for list rows and panel headers, so a tunnel
/// looks the same in a list as it does in the sky.
class PlanetGlyph extends StatelessWidget {
  const PlanetGlyph({
    super.key,
    required this.color,
    this.size = 30,
    this.ring = false,
  });

  final Color color;

  /// Edge of the box.
  final double size;

  /// Draw an orbit ring around it: a star with planets of its own.
  final bool ring;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return SizedBox.square(
      dimension: size,
      child: CustomPaint(
        painter: _GlyphPainter(
          color: color,
          dark: theme.brightness == Brightness.dark,
          ring: ring ? theme.colorScheme.onSurface : null,
        ),
      ),
    );
  }
}

class _GlyphPainter extends CustomPainter {
  _GlyphPainter({required this.color, required this.dark, this.ring});

  final Color color;
  final bool dark;
  final Color? ring;

  @override
  void paint(Canvas canvas, Size size) {
    final center = size.center(Offset.zero);
    final radius = size.shortestSide * (ring != null ? 0.22 : 0.3);
    if (ring case final ink?) {
      canvas.drawOval(
        Rect.fromCenter(
          center: center,
          width: size.width * 0.94,
          height: size.height * 0.4,
        ),
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = 1
          ..color = ink.withValues(alpha: dark ? 0.3 : 0.24),
      );
    }
    paintSphere(canvas, center, radius, color, dark: dark);
  }

  @override
  bool shouldRepaint(_GlyphPainter old) =>
      old.color != color || old.dark != dark || old.ring != ring;
}
