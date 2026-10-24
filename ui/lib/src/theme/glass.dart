import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';

/// The app's layered material, back to front:
///
/// 1. [AppBackdrop] — the window's ground, a neutral lit softly from above;
/// 2. the navigation rail, drawn straight on the backdrop;
/// 3. [ContentSheet] — the opaque, rounded page the content lives on;
/// 4. [Glass] — translucent panels floating above the sheet, blurring what
///    moves beneath them, lit along a rim.
///
/// Every surface states its layer rather than picking a grey, which is what
/// gives the app depth without outlines.

/// How far a glass surface floats, which decides its recipe.
enum GlassDepth {
  /// Drawn on the backdrop or a sheet: a selected row. No blur.
  chrome,

  /// Floating over moving content: a toolbar over the galaxy. Blurs.
  floating,

  /// Over the whole page: a details panel, a popover.
  overlay,
}

/// One glass surface's recipe.
@immutable
class GlassRecipe {
  const GlassRecipe({
    required this.fill,
    required this.sheen,
    required this.rimLight,
    required this.rimShade,
    required this.hairline,
    required this.blur,
    required this.shadows,
  });

  /// The translucent body.
  final Color fill;

  /// The highlight washed down from the top edge.
  final Color sheen;

  /// The rim where the light catches it, and away from the light.
  final Color rimLight, rimShade;

  /// The outermost edge.
  final Color hairline;

  /// Backdrop blur sigma; zero for none.
  final double blur;

  /// Shadows cast outside the surface (never under it, where they would grey
  /// the glass).
  final List<BoxShadow> shadows;
}

/// The [GlassRecipe] for [depth] in [scheme]; [lift] scales the shadow.
GlassRecipe glassRecipe(
  ColorScheme scheme,
  GlassDepth depth, {
  double lift = 1,
}) {
  final light = scheme.brightness == Brightness.light;
  BoxShadow shadow(double alpha, double blur, double y) => BoxShadow(
    color: Colors.black.withValues(alpha: alpha),
    blurRadius: blur,
    offset: Offset(0, y),
  );
  if (light) {
    return switch (depth) {
      GlassDepth.chrome => GlassRecipe(
        fill: Colors.white.withValues(alpha: 0.66),
        sheen: Colors.white.withValues(alpha: 0.5),
        rimLight: Colors.white.withValues(alpha: 0.95),
        rimShade: Colors.white.withValues(alpha: 0.35),
        hairline: Colors.black.withValues(alpha: 0.06),
        blur: 0,
        shadows: [
          shadow(0.05, 2, 1),
          shadow(0.045 * lift, 12 * lift, 3 * lift),
        ],
      ),
      GlassDepth.floating => GlassRecipe(
        fill: Colors.white.withValues(alpha: 0.6),
        sheen: Colors.white.withValues(alpha: 0.55),
        rimLight: Colors.white,
        rimShade: Colors.white.withValues(alpha: 0.4),
        hairline: Colors.black.withValues(alpha: 0.07),
        blur: 24,
        shadows: [
          shadow(0.05, 3, 1),
          shadow(0.09 * lift, 36 * lift, 14 * lift),
        ],
      ),
      GlassDepth.overlay => GlassRecipe(
        fill: Colors.white.withValues(alpha: 0.82),
        sheen: Colors.white.withValues(alpha: 0.6),
        rimLight: Colors.white,
        rimShade: Colors.white.withValues(alpha: 0.45),
        hairline: Colors.black.withValues(alpha: 0.08),
        blur: 28,
        shadows: [
          shadow(0.06, 4, 1),
          shadow(0.11 * lift, 48 * lift, 20 * lift),
        ],
      ),
    };
  }
  return switch (depth) {
    GlassDepth.chrome => GlassRecipe(
      fill: Colors.white.withValues(alpha: 0.075),
      sheen: Colors.white.withValues(alpha: 0.06),
      rimLight: Colors.white.withValues(alpha: 0.17),
      rimShade: Colors.white.withValues(alpha: 0.03),
      hairline: Colors.black.withValues(alpha: 0.35),
      blur: 0,
      shadows: [shadow(0.35 * lift, 10 * lift, 3 * lift)],
    ),
    GlassDepth.floating => GlassRecipe(
      fill: const Color(0xFF28282B).withValues(alpha: 0.62),
      sheen: Colors.white.withValues(alpha: 0.07),
      rimLight: Colors.white.withValues(alpha: 0.2),
      rimShade: Colors.white.withValues(alpha: 0.04),
      hairline: Colors.black.withValues(alpha: 0.55),
      blur: 26,
      shadows: [shadow(0.4, 6, 2), shadow(0.5 * lift, 40 * lift, 16 * lift)],
    ),
    GlassDepth.overlay => GlassRecipe(
      fill: const Color(0xFF252528).withValues(alpha: 0.86),
      sheen: Colors.white.withValues(alpha: 0.07),
      rimLight: Colors.white.withValues(alpha: 0.2),
      rimShade: Colors.white.withValues(alpha: 0.05),
      hairline: Colors.black.withValues(alpha: 0.6),
      blur: 30,
      shadows: [shadow(0.45, 6, 2), shadow(0.6 * lift, 56 * lift, 24 * lift)],
    ),
  };
}

/// A translucent surface: a blurred view of what is behind it, a tinted body,
/// a sheen across its top and a rim catching the light from the top-left.
class Glass extends StatelessWidget {
  const Glass({
    super.key,
    required this.child,
    this.depth = GlassDepth.floating,
    this.borderRadius = const BorderRadius.all(Radius.circular(20)),
    this.padding = EdgeInsets.zero,
    this.lift = 1,
    this.tint,
  });

  final Widget child;
  final GlassDepth depth;

  /// The surface's corners; a large radius makes a capsule.
  final BorderRadius borderRadius;
  final EdgeInsetsGeometry padding;

  /// Scales the cast shadow.
  final double lift;

  /// Replaces the recipe's body colour.
  final Color? tint;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final recipe = glassRecipe(scheme, depth, lift: lift);
    Widget body = CustomPaint(
      painter: _GlassBodyPainter(
        borderRadius: borderRadius,
        fill: tint ?? recipe.fill,
        sheen: recipe.sheen,
      ),
      foregroundPainter: _GlassRimPainter(
        borderRadius: borderRadius,
        light: recipe.rimLight,
        shade: recipe.rimShade,
        hairline: recipe.hairline,
      ),
      child: Material(
        type: MaterialType.transparency,
        child: Padding(padding: padding, child: child),
      ),
    );
    if (recipe.blur > 0) {
      body = BackdropFilter(
        filter: ui.ImageFilter.blur(sigmaX: recipe.blur, sigmaY: recipe.blur),
        child: body,
      );
    }
    body = ClipRRect(borderRadius: borderRadius, child: body);
    return CustomPaint(
      painter: OuterShadowPainter(
        borderRadius: borderRadius,
        shadows: recipe.shadows,
      ),
      child: body,
    );
  }
}

RRect _rrect(BorderRadius radius, Rect rect) =>
    radius.toRRect(rect).scaleRadii();

class _GlassBodyPainter extends CustomPainter {
  const _GlassBodyPainter({
    required this.borderRadius,
    required this.fill,
    required this.sheen,
  });

  final BorderRadius borderRadius;
  final Color fill;
  final Color sheen;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    final rrect = _rrect(borderRadius, rect);
    canvas.drawRRect(rrect, Paint()..color = fill);
    // The sheen fades out by the middle, so tall panels keep a clear body and
    // short capsules read as lit from above.
    final reach = size.height < 72 ? 1.0 : 72 / size.height;
    canvas.drawRRect(
      rrect,
      Paint()
        ..shader = LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          colors: [sheen, sheen.withValues(alpha: 0)],
          stops: [0, 0.55 * reach.clamp(0.0, 1.0)],
        ).createShader(rect),
    );
  }

  @override
  bool shouldRepaint(_GlassBodyPainter old) =>
      old.borderRadius != borderRadius ||
      old.fill != fill ||
      old.sheen != sheen;
}

class _GlassRimPainter extends CustomPainter {
  const _GlassRimPainter({
    required this.borderRadius,
    required this.light,
    required this.shade,
    required this.hairline,
  });

  final BorderRadius borderRadius;
  final Color light;
  final Color shade;
  final Color hairline;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    if (rect.isEmpty) return;
    canvas.drawRRect(
      _rrect(borderRadius, rect.deflate(0.5)),
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1
        ..color = hairline,
    );
    canvas.drawRRect(
      _rrect(borderRadius, rect.deflate(1.5)),
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1
        ..shader = LinearGradient(
          begin: Alignment.topLeft,
          end: Alignment.bottomRight,
          colors: [
            light,
            shade,
            shade,
            light.withValues(alpha: light.a * 0.5),
          ],
          stops: const [0, 0.38, 0.7, 1],
        ).createShader(rect),
    );
  }

  @override
  bool shouldRepaint(_GlassRimPainter old) =>
      old.borderRadius != borderRadius ||
      old.light != light ||
      old.shade != shade ||
      old.hairline != hairline;
}

/// Paints [shadows] for a rounded box only outside it, so a translucent
/// surface over it is not greyed by its own shadow.
class OuterShadowPainter extends CustomPainter {
  const OuterShadowPainter({required this.borderRadius, required this.shadows});

  final BorderRadius borderRadius;
  final List<BoxShadow> shadows;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    if (rect.isEmpty || shadows.isEmpty) return;
    final rrect = _rrect(borderRadius, rect);
    var reach = 0.0;
    for (final s in shadows) {
      final r = s.blurRadius * 2 + s.spreadRadius + s.offset.distance;
      if (r > reach) reach = r;
    }
    canvas.save();
    canvas.clipPath(
      Path.combine(
        PathOperation.difference,
        Path()..addRect(rect.inflate(reach + 4)),
        Path()..addRRect(rrect),
      ),
    );
    for (final s in shadows) {
      canvas.drawRRect(
        rrect.shift(s.offset).inflate(s.spreadRadius),
        Paint()
          ..color = s.color
          ..maskFilter = MaskFilter.blur(BlurStyle.normal, s.blurSigma),
      );
    }
    canvas.restore();
  }

  @override
  bool shouldRepaint(OuterShadowPainter old) =>
      old.borderRadius != borderRadius || old.shadows != shadows;
}

/// The window's ground: [backdropBase] with [backdropGlows] washed over it.
class AppBackdrop extends StatelessWidget {
  const AppBackdrop({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Stack(
      fit: StackFit.expand,
      children: [
        RepaintBoundary(
          child: CustomPaint(
            painter: _BackdropPainter(
              base: backdropBase(scheme),
              glows: backdropGlows(scheme),
            ),
          ),
        ),
        child,
      ],
    );
  }
}

class _BackdropPainter extends CustomPainter {
  const _BackdropPainter({required this.base, required this.glows});

  final Color base;
  final List<({Alignment center, double radius, Color color})> glows;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    canvas.drawRect(rect, Paint()..color = base);
    for (final glow in glows) {
      canvas.drawRect(
        rect,
        Paint()
          ..shader = RadialGradient(
            center: glow.center,
            radius: glow.radius,
            colors: [glow.color, glow.color.withValues(alpha: 0)],
          ).createShader(rect),
      );
    }
  }

  @override
  bool shouldRepaint(_BackdropPainter old) =>
      old.base != base || old.glows != glows;
}

/// The opaque, rounded page content lives on, lifted off the [AppBackdrop].
/// [grouped] selects the grey ground that white cards sit on.
class ContentSheet extends StatelessWidget {
  const ContentSheet({super.key, required this.child, this.grouped = false});

  final Widget child;
  final bool grouped;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    const radius = BorderRadius.all(Radius.circular(kSheetRadius));
    return DecoratedBox(
      decoration: BoxDecoration(
        borderRadius: radius,
        boxShadow: sheetShadows(scheme),
      ),
      child: Material(
        color: sheetColor(scheme, grouped: grouped),
        clipBehavior: Clip.antiAlias,
        shape: RoundedRectangleBorder(
          borderRadius: radius,
          side: BorderSide(color: sheetEdge(scheme)),
        ),
        child: child,
      ),
    );
  }
}

/// Plays [child] in once when it first mounts: it fades up from a few pixels
/// below, so a page's content settles onto its sheet instead of appearing.
class EntranceMotion extends StatefulWidget {
  const EntranceMotion({
    super.key,
    required this.child,
    this.offset = 10,
    this.delay = Duration.zero,
  });

  final Widget child;
  final double offset;

  /// A wait before it starts, for a staggered group.
  final Duration delay;

  @override
  State<EntranceMotion> createState() => _EntranceMotionState();
}

class _EntranceMotionState extends State<EntranceMotion>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 420),
  );
  late final Animation<double> _t = CurvedAnimation(
    parent: _controller,
    curve: Motion.emphasized,
  );
  bool _started = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_started) return;
    _started = true;
    if (Motion.reduced(context)) {
      _controller.value = 1;
    } else if (widget.delay == Duration.zero) {
      _controller.forward();
    } else {
      Future.delayed(widget.delay, () {
        if (mounted) _controller.forward();
      });
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: _t,
    child: widget.child,
    builder: (context, child) => Opacity(
      opacity: _t.value.clamp(0.0, 1.0),
      child: Transform.translate(
        offset: Offset(0, (1 - _t.value) * widget.offset),
        child: child,
      ),
    ),
  );
}

/// The lit face the button themes lay over a button's fill: a sheen down from
/// the top and a highlight along the upper rim, so a capsule reads as a raised
/// piece of glass rather than a flat shape. Disabled buttons stay flat.
class LitFace extends StatelessWidget {
  const LitFace({
    super.key,
    required this.states,
    required this.child,
    this.glass = false,
  });

  final Set<WidgetState> states;
  final Widget? child;

  /// The translucent secondary style rather than a filled one.
  final bool glass;

  @override
  Widget build(BuildContext context) {
    if (states.contains(WidgetState.disabled)) {
      return child ?? const SizedBox.shrink();
    }
    final light = Theme.of(context).brightness == Brightness.light;
    final pressed = states.contains(WidgetState.pressed);
    final double sheen;
    final double rim;
    if (glass) {
      sheen = light ? 0.55 : 0.06;
      rim = light ? 0.95 : 0.16;
    } else {
      sheen = light ? 0.16 : 0.42;
      rim = light ? 0.22 : 0.6;
    }
    return CustomPaint(
      painter: _LitFacePainter(sheen: pressed ? sheen * 0.4 : sheen, rim: rim),
      child: child,
    );
  }
}

class _LitFacePainter extends CustomPainter {
  const _LitFacePainter({required this.sheen, required this.rim});

  final double sheen;
  final double rim;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    if (rect.isEmpty) return;
    // Buttons do not clip, so the sheen is clipped to the capsule here or its
    // square corners show past the curve on a dark page.
    final radius = Radius.circular(size.shortestSide / 2);
    canvas
      ..save()
      ..clipRRect(RRect.fromRectAndRadius(rect, radius))
      ..drawRect(
        rect,
        Paint()
          ..shader = LinearGradient(
            begin: Alignment.topCenter,
            end: Alignment.bottomCenter,
            colors: [
              Colors.white.withValues(alpha: sheen),
              Colors.white.withValues(alpha: 0),
            ],
            stops: const [0, 0.6],
          ).createShader(rect),
      )
      ..restore();
    canvas.drawRRect(
      RRect.fromRectAndRadius(rect.deflate(0.5), radius),
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1
        ..shader = LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          colors: [
            Colors.white.withValues(alpha: rim),
            Colors.white.withValues(alpha: 0),
          ],
          stops: const [0, 0.5],
        ).createShader(rect),
    );
  }

  @override
  bool shouldRepaint(_LitFacePainter old) =>
      old.sheen != sheen || old.rim != rim;
}

/// The border of the app's filled fields. At rest it draws nothing (the fill
/// is the field); focused or in error it draws a ring, with an optional soft
/// [glow] so focus reads as the field lighting up. Not an outline border, so
/// a floating label sits inside the fill instead of straddling its edge.
class FieldBorder extends InputBorder {
  const FieldBorder({
    super.borderSide = BorderSide.none,
    this.borderRadius = const BorderRadius.all(Radius.circular(4)),
    this.glow,
  });

  final BorderRadius borderRadius;
  final Color? glow;

  @override
  bool get isOutline => false;

  @override
  EdgeInsetsGeometry get dimensions => EdgeInsets.all(borderSide.width);

  @override
  FieldBorder copyWith({
    BorderSide? borderSide,
    BorderRadius? borderRadius,
    Color? glow,
  }) => FieldBorder(
    borderSide: borderSide ?? this.borderSide,
    borderRadius: borderRadius ?? this.borderRadius,
    glow: glow ?? this.glow,
  );

  @override
  FieldBorder scale(double t) => FieldBorder(
    borderSide: borderSide.scale(t),
    borderRadius: borderRadius * t,
    glow: glow,
  );

  RRect _shape(Rect rect) => borderRadius.toRRect(rect);

  @override
  Path getInnerPath(Rect rect, {TextDirection? textDirection}) =>
      Path()..addRRect(_shape(rect).deflate(borderSide.width));

  @override
  Path getOuterPath(Rect rect, {TextDirection? textDirection}) =>
      Path()..addRRect(_shape(rect));

  @override
  bool get preferPaintInterior => true;

  @override
  void paintInterior(
    Canvas canvas,
    Rect rect,
    Paint paint, {
    TextDirection? textDirection,
  }) => canvas.drawRRect(_shape(rect), paint);

  static FieldBorder _lerp(FieldBorder a, FieldBorder b, double t) =>
      FieldBorder(
        borderSide: BorderSide.lerp(a.borderSide, b.borderSide, t),
        borderRadius: BorderRadius.lerp(a.borderRadius, b.borderRadius, t)!,
        glow: Color.lerp(a.glow, b.glow, t),
      );

  @override
  ShapeBorder? lerpFrom(ShapeBorder? a, double t) =>
      a is FieldBorder ? _lerp(a, this, t) : super.lerpFrom(a, t);

  @override
  ShapeBorder? lerpTo(ShapeBorder? b, double t) =>
      b is FieldBorder ? _lerp(this, b, t) : super.lerpTo(b, t);

  @override
  void paint(
    Canvas canvas,
    Rect rect, {
    double? gapStart,
    double gapExtent = 0.0,
    double gapPercentage = 0.0,
    TextDirection? textDirection,
  }) {
    final shape = _shape(rect);
    if (glow case final glow? when glow.a > 0) {
      canvas.drawRRect(
        shape.inflate(2.5),
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = 4
          ..color = glow,
      );
    }
    if (borderSide.style == BorderStyle.none || borderSide.width == 0) return;
    canvas.drawRRect(shape.deflate(borderSide.width / 2), borderSide.toPaint());
  }

  @override
  bool operator ==(Object other) =>
      other is FieldBorder &&
      other.borderSide == borderSide &&
      other.borderRadius == borderRadius &&
      other.glow == glow;

  @override
  int get hashCode => Object.hash(borderSide, borderRadius, glow);
}

/// One choice of a [GlassSegmented].
@immutable
class GlassSegment<T> {
  const GlassSegment({
    required this.value,
    required this.label,
    this.icon,
    this.key,
  });

  final T value;
  final String label;
  final IconData? icon;

  /// The key of its tappable area.
  final Key? key;
}

/// A segmented control in the app's material: a recessed track with a pane of
/// glass that glides, with a little spring, to the chosen segment. [expand]
/// shares the full width; otherwise every segment takes the widest label's.
class GlassSegmented<T> extends StatelessWidget {
  const GlassSegmented({
    super.key,
    required this.segments,
    required this.selected,
    required this.onChanged,
    this.expand = true,
    this.height = 34,
  });

  final List<GlassSegment<T>> segments;
  final T selected;
  final ValueChanged<T> onChanged;
  final bool expand;
  final double height;

  static const double _inset = 3;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final light = scheme.brightness == Brightness.light;
    const labelStyle = TextStyle(fontSize: 13, fontWeight: FontWeight.w600);
    final index = segments.indexWhere((s) => s.value == selected);
    final scaled = MediaQuery.textScalerOf(context).scale(height);
    double natural(GlassSegment<T> s) =>
        measureTextLine(context, s.label, labelStyle).width.ceilToDouble() +
        (s.icon == null ? 0 : 22) +
        28;
    return LayoutBuilder(
      builder: (context, box) {
        final width = expand && box.maxWidth.isFinite
            ? (box.maxWidth - 2 * _inset) / segments.length
            : segments.map(natural).fold(0.0, (a, b) => a > b ? a : b);
        final radius = BorderRadius.circular(scaled / 2);
        return Container(
          height: scaled,
          width: width * segments.length + 2 * _inset,
          padding: const EdgeInsets.all(_inset),
          decoration: BoxDecoration(
            color: light
                ? Colors.black.withValues(alpha: 0.055)
                : Colors.white.withValues(alpha: 0.06),
            borderRadius: radius,
          ),
          child: Stack(
            children: [
              if (index >= 0)
                AnimatedPositioned(
                  duration: Motion.of(
                    context,
                    const Duration(milliseconds: 480),
                  ),
                  curve: Motion.spring,
                  left: index * width,
                  top: 0,
                  bottom: 0,
                  width: width,
                  child: IgnorePointer(
                    child: Glass(
                      depth: GlassDepth.chrome,
                      borderRadius: radius,
                      tint: light
                          ? Colors.white.withValues(alpha: 0.95)
                          : Colors.white.withValues(alpha: 0.13),
                      child: const SizedBox.expand(),
                    ),
                  ),
                ),
              Row(
                children: [
                  for (final segment in segments)
                    SizedBox(
                      width: width,
                      child: _segment(context, segment, labelStyle, radius),
                    ),
                ],
              ),
            ],
          ),
        );
      },
    );
  }

  Widget _segment(
    BuildContext context,
    GlassSegment<T> segment,
    TextStyle style,
    BorderRadius radius,
  ) {
    final scheme = Theme.of(context).colorScheme;
    final chosen = segment.value == selected;
    final ink = chosen ? scheme.onSurface : scheme.onSurfaceVariant;
    return Semantics(
      selected: chosen,
      button: true,
      child: Material(
        type: MaterialType.transparency,
        child: InkWell(
          key: segment.key,
          mouseCursor: clickable,
          borderRadius: radius,
          onTap: chosen ? null : () => onChanged(segment.value),
          child: Center(
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                if (segment.icon != null) ...[
                  Icon(segment.icon, size: 16, color: ink),
                  const SizedBox(width: 6),
                ],
                Flexible(
                  child: Text(
                    segment.label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: style.copyWith(
                      color: ink,
                      fontWeight: chosen ? FontWeight.w600 : FontWeight.w500,
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
