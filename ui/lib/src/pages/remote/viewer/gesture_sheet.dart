import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../state/viewer_prefs.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/glass.dart';
import '../../../theme/viewer_palette.dart';

/// The gestures the sheet draws.
enum GestureGlyphKind {
  slide,
  tap,
  longPress,
  doubleTapHold,
  threeFinger,
  twoFingerMove,
  pinch,
}

/// How touch drives the remote computer, and the switch between trackpad
/// and direct touch (ui §9.7): a centred panel over the picture, which
/// scrolls on a short landscape screen.
class GestureSheet extends StatelessWidget {
  const GestureSheet({super.key, required this.prefs});

  final ViewerPrefs prefs;

  /// Shows the sheet; the future completes when it closes.
  static Future<void> show(BuildContext context, ViewerPrefs prefs) =>
      showDialog<void>(
        context: context,
        builder: (_) => GestureSheet(prefs: prefs),
      );

  @override
  Widget build(BuildContext context) => Dialog(
    backgroundColor: ViewerPalette.card,
    insetPadding: const EdgeInsets.all(16),
    shape: const RoundedRectangleBorder(
      borderRadius: BorderRadius.all(Radius.circular(20)),
      side: BorderSide(color: ViewerPalette.edge),
    ),
    child: ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 390),
      child: ListenableBuilder(listenable: prefs, builder: _content),
    ),
  );

  Widget _content(BuildContext context, Widget? _) {
    final l10n = context.l10n;
    final direct = prefs.touchMode == TouchMode.direct;
    final rows = [
      (
        GestureGlyphKind.slide,
        l10n.gestureSlide,
        direct ? l10n.gestureDragResult : l10n.gestureSlideResult,
      ),
      (
        GestureGlyphKind.tap,
        l10n.gestureTap,
        direct ? l10n.gestureTapHereResult : l10n.gestureTapResult,
      ),
      (
        GestureGlyphKind.longPress,
        l10n.gestureLongPress,
        l10n.gestureLongPressResult,
      ),
      if (!direct)
        (
          GestureGlyphKind.doubleTapHold,
          l10n.gestureDoubleTapHold,
          l10n.gestureDragResult,
        ),
      (
        GestureGlyphKind.threeFinger,
        l10n.gestureThreeFinger,
        l10n.gestureThreeFingerResult,
      ),
      (
        GestureGlyphKind.twoFingerMove,
        l10n.gestureTwoFingerMove,
        l10n.gestureTwoFingerMoveResult,
      ),
      (GestureGlyphKind.pinch, l10n.gesturePinch, l10n.gesturePinchResult),
    ];
    return SingleChildScrollView(
      padding: const EdgeInsets.fromLTRB(20, 14, 20, 20),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(
                child: Semantics(
                  header: true,
                  child: Text(
                    l10n.gestureTitle,
                    style: const TextStyle(
                      fontSize: 18,
                      fontWeight: FontWeight.w600,
                      color: ViewerPalette.text,
                    ),
                  ),
                ),
              ),
              Semantics(
                button: true,
                label: l10n.close,
                excludeSemantics: true,
                child: Material(
                  color: ViewerPalette.fill,
                  shape: const CircleBorder(),
                  child: InkWell(
                    customBorder: const CircleBorder(),
                    onTap: () => Navigator.pop(context),
                    child: const SizedBox.square(
                      dimension: 36,
                      child: Icon(
                        AppIcons.close,
                        size: AppIconSize.button,
                        color: ViewerPalette.textSecondary,
                      ),
                    ),
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: 14),
          GlassSegmented<TouchMode>(
            key: const Key('gesture-mode'),
            height: 44,
            segments: [
              GlassSegment(
                key: const Key('gesture-mode-trackpad'),
                value: TouchMode.trackpad,
                label: l10n.touchModeTrackpad,
              ),
              GlassSegment(
                key: const Key('gesture-mode-direct'),
                value: TouchMode.direct,
                label: l10n.touchModeDirect,
              ),
            ],
            selected: prefs.touchMode,
            onChanged: prefs.setTouchMode,
          ),
          const SizedBox(height: 12),
          Text(
            direct
                ? l10n.gestureDirectDescription
                : l10n.gestureTrackpadDescription,
            style: const TextStyle(
              fontSize: 13,
              height: 1.5,
              color: ViewerPalette.textSecondary,
            ),
          ),
          const SizedBox(height: 6),
          for (var i = 0; i < rows.length; i++)
            _GestureRow(
              glyph: rows[i].$1,
              name: rows[i].$2,
              result: rows[i].$3,
              last: i == rows.length - 1,
            ),
          const SizedBox(height: 8),
          MergeSemantics(
            child: Row(
              children: [
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        l10n.virtualMouse,
                        style: TextStyle(
                          fontSize: 15,
                          color: direct
                              ? ViewerPalette.text
                              : ViewerPalette.textQuiet,
                        ),
                      ),
                      const SizedBox(height: 2),
                      Text(
                        l10n.virtualMouseHelp,
                        style: const TextStyle(
                          fontSize: 12.5,
                          color: ViewerPalette.textQuiet,
                        ),
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 12),
                Switch(
                  value: prefs.virtualMouse,
                  onChanged: direct ? prefs.setVirtualMouse : null,
                ),
              ],
            ),
          ),
          const SizedBox(height: 18),
          FilledButton(
            key: const Key('gesture-got-it'),
            style: FilledButton.styleFrom(
              minimumSize: const Size.fromHeight(48),
              shape: const StadiumBorder(),
              backgroundColor: ViewerPalette.text,
              foregroundColor: const Color(0xFF1D1D1F),
              // From the theme's label style: a bare TextStyle here would
              // replace it and lose the UI typeface.
              textStyle: Theme.of(context).textTheme.labelLarge?.copyWith(
                fontSize: 15,
                fontWeight: FontWeight.w600,
              ),
            ),
            onPressed: () {
              unawaited(prefs.setGestureSheetSeen(true));
              Navigator.pop(context);
            },
            child: Text(l10n.gotIt),
          ),
        ],
      ),
    );
  }
}

class _GestureRow extends StatelessWidget {
  const _GestureRow({
    required this.glyph,
    required this.name,
    required this.result,
    required this.last,
  });

  final GestureGlyphKind glyph;
  final String name;
  final String result;
  final bool last;

  @override
  Widget build(BuildContext context) => MergeSemantics(
    child: Container(
      constraints: const BoxConstraints(minHeight: 54),
      padding: const EdgeInsets.symmetric(vertical: 6),
      decoration: last
          ? null
          : const BoxDecoration(
              border: Border(bottom: BorderSide(color: ViewerPalette.edge)),
            ),
      child: Row(
        children: [
          GestureGlyph(glyph),
          const SizedBox(width: 14),
          Expanded(
            child: Text(
              name,
              style: const TextStyle(
                fontSize: 15,
                fontWeight: FontWeight.w600,
                color: ViewerPalette.text,
              ),
            ),
          ),
          const SizedBox(width: 12),
          Text(
            result,
            style: const TextStyle(
              fontSize: 14,
              color: ViewerPalette.textSecondary,
            ),
          ),
        ],
      ),
    ),
  );
}

/// A gesture drawn on a 40 tile.
class GestureGlyph extends StatelessWidget {
  const GestureGlyph(this.kind, {super.key});

  final GestureGlyphKind kind;

  @override
  Widget build(BuildContext context) => Container(
    width: 40,
    height: 40,
    decoration: const BoxDecoration(
      color: ViewerPalette.fill,
      borderRadius: BorderRadius.all(Radius.circular(12)),
    ),
    alignment: Alignment.center,
    child: CustomPaint(
      size: const Size.square(24),
      painter: GestureGlyphPainter(kind, color: ViewerPalette.text),
    ),
  );
}

/// The design's 24-unit gesture glyphs: fingers as dots, motion as arrows,
/// a tap as a dashed ring, a hold as rings.
class GestureGlyphPainter extends CustomPainter {
  const GestureGlyphPainter(this.kind, {required this.color});

  final GestureGlyphKind kind;
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.save();
    canvas.scale(size.width / 24, size.height / 24);
    final stroke = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.5
      ..strokeCap = StrokeCap.round
      ..strokeJoin = StrokeJoin.round
      ..color = color;
    final fill = Paint()..color = color;
    void dot(double x, double y, double r) =>
        canvas.drawCircle(Offset(x, y), r, fill);
    void ring(double x, double y, double r, {double opacity = 1}) =>
        canvas.drawCircle(
          Offset(x, y),
          r,
          Paint()
            ..style = PaintingStyle.stroke
            ..strokeWidth = 1.5
            ..color = color.withValues(alpha: color.a * opacity),
        );
    void dashed(double x, double y, double r, double on, double off) {
      final step = (on + off) / r;
      final sweep = on / r;
      for (var a = -math.pi / 2; a < 3 * math.pi / 2 - 1e-6; a += step) {
        canvas.drawArc(
          Rect.fromCircle(center: Offset(x, y), radius: r),
          a,
          sweep,
          false,
          stroke,
        );
      }
    }

    void line(List<Offset> points) {
      final path = Path()..moveTo(points.first.dx, points.first.dy);
      for (final p in points.skip(1)) {
        path.lineTo(p.dx, p.dy);
      }
      canvas.drawPath(path, stroke);
    }

    switch (kind) {
      case GestureGlyphKind.slide:
        dot(7, 16, 3);
        line(const [Offset(10, 13), Offset(19, 6)]);
        line(const [Offset(15, 6), Offset(19, 6), Offset(19, 10)]);
      case GestureGlyphKind.tap:
        dot(12, 12, 3);
        dashed(12, 12, 7, 2, 3);
      case GestureGlyphKind.longPress:
        dot(12, 12, 3);
        ring(12, 12, 6.5);
        ring(12, 12, 9.5, opacity: 0.4);
      case GestureGlyphKind.doubleTapHold:
        dot(7, 16, 3);
        dashed(7, 16, 5.5, 2, 2.5);
        line(const [Offset(12, 11), Offset(19, 5)]);
        line(const [Offset(15, 5), Offset(19, 5), Offset(19, 9)]);
      case GestureGlyphKind.threeFinger:
        dot(6, 12, 2.2);
        dot(12, 12, 2.2);
        dot(18, 12, 2.2);
        line(const [Offset(12, 7), Offset(12, 2)]);
        line(const [Offset(10, 4), Offset(12, 2), Offset(14, 4)]);
        line(const [Offset(12, 17), Offset(12, 22)]);
        line(const [Offset(10, 20), Offset(12, 22), Offset(14, 20)]);
      case GestureGlyphKind.twoFingerMove:
        dot(9, 13, 2.4);
        dot(15, 13, 2.4);
        line(const [Offset(12, 3), Offset(12, 7)]);
        line(const [Offset(10, 5), Offset(12, 3), Offset(14, 5)]);
        line(const [Offset(3, 13), Offset(5, 13)]);
        line(const [Offset(21, 13), Offset(19, 13)]);
        line(const [Offset(4, 11), Offset(3, 13), Offset(4, 15)]);
        line(const [Offset(20, 11), Offset(21, 13), Offset(20, 15)]);
      case GestureGlyphKind.pinch:
        dot(9, 15, 2.4);
        dot(15, 9, 2.4);
        line(const [Offset(6, 18), Offset(3, 21)]);
        line(const [Offset(3, 18), Offset(3, 21), Offset(6, 21)]);
        line(const [Offset(18, 6), Offset(21, 3)]);
        line(const [Offset(18, 3), Offset(21, 3), Offset(21, 6)]);
    }
    canvas.restore();
  }

  @override
  bool shouldRepaint(GestureGlyphPainter oldDelegate) =>
      oldDelegate.kind != kind || oldDelegate.color != color;
}
