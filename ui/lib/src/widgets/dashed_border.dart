import 'dart:math' as math;
import 'dart:ui' show lerpDouble;

import 'package:flutter/material.dart';

/// A rounded rectangle drawn with a dashed [side], for a tile that invites
/// adding something. An [OutlinedBorder], so it works as `Material(shape:)`:
/// the material clips its ink to the same rounded rectangle.
class DashedBorder extends OutlinedBorder {
  const DashedBorder({
    super.side,
    this.borderRadius = BorderRadius.zero,
    this.dash = 4,
    this.gap = 3,
  }) : assert(dash > 0 && gap >= 0);

  final BorderRadiusGeometry borderRadius;

  /// The length of each dash and of the space after it.
  final double dash;
  final double gap;

  @override
  EdgeInsetsGeometry get dimensions => EdgeInsets.all(side.strokeInset);

  @override
  DashedBorder copyWith({
    BorderSide? side,
    BorderRadiusGeometry? borderRadius,
    double? dash,
    double? gap,
  }) => DashedBorder(
    side: side ?? this.side,
    borderRadius: borderRadius ?? this.borderRadius,
    dash: dash ?? this.dash,
    gap: gap ?? this.gap,
  );

  @override
  DashedBorder scale(double t) => DashedBorder(
    side: side.scale(t),
    borderRadius: borderRadius * t,
    dash: dash * t,
    gap: gap * t,
  );

  @override
  ShapeBorder? lerpFrom(ShapeBorder? a, double t) {
    if (a is DashedBorder) return _lerp(a, this, t);
    return super.lerpFrom(a, t);
  }

  @override
  ShapeBorder? lerpTo(ShapeBorder? b, double t) {
    if (b is DashedBorder) return _lerp(this, b, t);
    return super.lerpTo(b, t);
  }

  static DashedBorder _lerp(DashedBorder a, DashedBorder b, double t) =>
      DashedBorder(
        side: BorderSide.lerp(a.side, b.side, t),
        borderRadius: BorderRadiusGeometry.lerp(
          a.borderRadius,
          b.borderRadius,
          t,
        )!,
        dash: lerpDouble(a.dash, b.dash, t)!,
        gap: lerpDouble(a.gap, b.gap, t)!,
      );

  RRect _rrect(Rect rect, TextDirection? textDirection) =>
      borderRadius.resolve(textDirection).toRRect(rect);

  @override
  Path getInnerPath(Rect rect, {TextDirection? textDirection}) =>
      Path()..addRRect(_rrect(rect, textDirection).deflate(side.strokeInset));

  @override
  Path getOuterPath(Rect rect, {TextDirection? textDirection}) =>
      Path()..addRRect(_rrect(rect, textDirection));

  @override
  void paint(Canvas canvas, Rect rect, {TextDirection? textDirection}) {
    if (side.style == BorderStyle.none || side.width == 0) return;
    // The stroke's centre line, inside, outside or on the edge per
    // strokeAlign, as RoundedRectangleBorder draws its solid one.
    final outline = _rrect(rect, textDirection).inflate(side.strokeOffset / 2);
    final dashes = Path();
    for (final metric in (Path()..addRRect(outline)).computeMetrics()) {
      for (var at = 0.0; at < metric.length; at += dash + gap) {
        dashes.addPath(
          metric.extractPath(at, math.min(at + dash, metric.length)),
          Offset.zero,
        );
      }
    }
    canvas.drawPath(dashes, side.toPaint());
  }

  @override
  bool operator ==(Object other) =>
      other is DashedBorder &&
      other.side == side &&
      other.borderRadius == borderRadius &&
      other.dash == dash &&
      other.gap == gap;

  @override
  int get hashCode => Object.hash(side, borderRadius, dash, gap);
}
