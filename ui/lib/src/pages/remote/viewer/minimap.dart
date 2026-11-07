import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../theme/viewer_palette.dart';

/// Where the visible part of a zoomed or occluded picture lies in the
/// whole (ui §9.5). Only an outline: no second texture, so it costs nothing
/// at the picture's rate. It never takes a touch.
class Minimap extends StatelessWidget {
  const Minimap({
    super.key = const Key('minimap'),
    required this.region,
    required this.aspectRatio,
  });

  /// The visible part, normalised to the picture.
  final Rect region;

  /// The picture's width over its height.
  final double aspectRatio;

  static const width = 96.0;

  /// The region's centre, on a 3×3 grid, as `minimapA11y` names it.
  static String area(Rect region) {
    final c = region.center;
    int third(double v) => v < 1 / 3 ? 0 : (v > 2 / 3 ? 2 : 1);
    return const [
      ['topLeft', 'top', 'topRight'],
      ['left', 'centre', 'right'],
      ['bottomLeft', 'bottom', 'bottomRight'],
    ][third(c.dy)][third(c.dx)];
  }

  @override
  Widget build(BuildContext context) => Semantics(
    label: context.l10n.minimapA11y(area(region)),
    child: IgnorePointer(
      child: Container(
        width: width,
        height: width / (aspectRatio > 0 ? aspectRatio : 16 / 9),
        decoration: BoxDecoration(
          color: const Color(0xCC141416),
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: Colors.white.withValues(alpha: 0.25)),
        ),
        child: CustomPaint(painter: MinimapPainter(region)),
      ),
    ),
  );
}

/// The visible region as a blue outline, kept inside the box.
class MinimapPainter extends CustomPainter {
  const MinimapPainter(this.region);

  final Rect region;

  static const _stroke = 1.5;

  @override
  void paint(Canvas canvas, Size size) {
    const half = _stroke / 2;
    final box = Rect.fromLTRB(
      half,
      half,
      size.width - half,
      size.height - half,
    );
    if (box.isEmpty) return;
    var r = Rect.fromLTRB(
      region.left * size.width,
      region.top * size.height,
      region.right * size.width,
      region.bottom * size.height,
    ).intersect(box);
    // A sliver still shows where it is.
    const least = 4.0;
    if (r.width < least || r.height < least) {
      r = Rect.fromCenter(
        center: r.center,
        width: r.width < least ? least : r.width,
        height: r.height < least ? least : r.height,
      );
    }
    canvas.drawRRect(
      RRect.fromRectAndRadius(r, const Radius.circular(2)),
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = _stroke
        ..color = ViewerPalette.signal,
    );
  }

  @override
  bool shouldRepaint(MinimapPainter oldDelegate) =>
      oldDelegate.region != region;
}
