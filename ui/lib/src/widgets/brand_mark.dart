import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';

/// The pb-mapper mark — a plug and its cable on the logo's blue tile — drawn
/// as vectors from `assets/icon_design/app_icon_blue_clean.svg`, so it stays
/// crisp at every size and never needs a bitmap per theme.
///
/// [plated] draws the tile; without it the glyph takes the ambient ink, for a
/// place where the tile would be one coloured block too many.
class BrandMark extends StatelessWidget {
  const BrandMark({super.key, this.size = 28, this.plated = true});

  final double size;
  final bool plated;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SizedBox.square(
      dimension: size,
      child: CustomPaint(
        painter: _MarkPainter(
          plated: plated,
          tile: brandGradient(scheme),
          ink: plated ? Colors.white : scheme.onSurface,
        ),
      ),
    );
  }
}

class _MarkPainter extends CustomPainter {
  const _MarkPainter({
    required this.plated,
    required this.tile,
    required this.ink,
  });

  final bool plated;
  final LinearGradient tile;
  final Color ink;

  @override
  void paint(Canvas canvas, Size size) {
    // The source is drawn on a 1024 grid with the tile inset by 64.
    final s = size.shortestSide / (plated ? 1024 : 640);
    canvas.save();
    if (plated) {
      canvas.scale(s);
      final rect = const Rect.fromLTWH(64, 64, 896, 896);
      canvas.drawRRect(
        RRect.fromRectAndRadius(rect, const Radius.circular(196)),
        Paint()..shader = tile.createShader(rect),
      );
    } else {
      // Without the tile, frame the glyph itself (x 256–808, y 360–838).
      canvas.scale(s);
      canvas.translate(-212, -280);
    }
    final fill = Paint()..color = ink;
    canvas.drawRRect(
      RRect.fromRectAndRadius(
        const Rect.fromLTWH(360, 420, 304, 192),
        const Radius.circular(36),
      ),
      fill,
    );
    for (final x in [408.0, 584.0]) {
      canvas.drawRRect(
        RRect.fromRectAndRadius(
          Rect.fromLTWH(x, 360, 32, 72),
          const Radius.circular(10),
        ),
        fill,
      );
    }
    final cable = Path()
      ..moveTo(664, 516)
      ..quadraticBezierTo(760, 516, 792, 584)
      ..quadraticBezierTo(824, 652, 792, 720)
      ..quadraticBezierTo(792, 788, 724, 820)
      ..quadraticBezierTo(656, 852, 588, 820)
      ..quadraticBezierTo(520, 820, 488, 752)
      ..quadraticBezierTo(456, 684, 388, 684)
      ..quadraticBezierTo(320, 684, 288, 716)
      ..quadraticBezierTo(256, 748, 256, 816);
    canvas.drawPath(
      cable,
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = 36
        ..strokeCap = StrokeCap.round
        ..color = ink,
    );
    canvas.restore();
  }

  @override
  bool shouldRepaint(_MarkPainter old) =>
      old.plated != plated || old.tile != tile || old.ink != ink;
}
