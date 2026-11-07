import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import 'remote_input.dart';
import 'remote_viewport.dart';

/// The remote cursor over the picture (D11): the host's latest cursor, as
/// the store publishes it from the view leaf, or the local prediction while
/// this viewer moves it, since the host does not echo a mover's cursor back
/// for [kCursorEchoWindow]. The local OS pointer stays visible on desktop.
///
/// Its own layer: the arrow moves with every stroke, hover and sample, and
/// repaints nothing around it.
class RemoteCursorLayer extends StatelessWidget {
  const RemoteCursorLayer({
    super.key,
    required this.hostCursor,
    required this.viewport,
    required this.input,
  });

  /// The host's cursor, normalised; null while hidden or unknown.
  final ValueListenable<Offset?> hostCursor;
  final RemoteViewport viewport;
  final RemoteInput input;

  /// Where to draw, in the stage's coordinates.
  Offset? _at() {
    final n = input.predicting ? input.cursor : hostCursor.value;
    return n == null ? null : viewport.toLocal(n);
  }

  @override
  Widget build(BuildContext context) => RepaintBoundary(
    child: IgnorePointer(
      child: ClipRect(
        child: CustomPaint(
          size: Size.infinite,
          painter: RemoteCursorPainter(
            at: _at,
            repaint: Listenable.merge([
              hostCursor,
              viewport,
              input.cursorChanges,
            ]),
          ),
        ),
      ),
    ),
  );
}

/// An arrow, 18×27 whatever the zoom: black with a white edge, its tip at
/// the point.
class RemoteCursorPainter extends CustomPainter {
  RemoteCursorPainter({required this.at, required Listenable repaint})
    : super(repaint: repaint);

  final Offset? Function() at;

  static const size = Size(18, 27);

  static final _arrow = Path()
    ..moveTo(1, 1)
    ..lineTo(1, 20)
    ..lineTo(6, 16)
    ..lineTo(10, 25)
    ..lineTo(14, 23)
    ..lineTo(10, 15)
    ..lineTo(17, 15)
    ..close();

  static final _fill = Paint()..color = const Color(0xFF000000);
  static final _edge = Paint()
    ..color = const Color(0xFFFFFFFF)
    ..style = PaintingStyle.stroke
    ..strokeWidth = 1.5;

  @override
  void paint(Canvas canvas, Size _) {
    final point = at();
    if (point == null) return;
    // The arrow's tip is at (1, 1).
    final path = _arrow.shift(point - const Offset(1, 1));
    canvas
      ..drawPath(path, _fill)
      ..drawPath(path, _edge);
  }

  @override
  bool shouldRepaint(covariant RemoteCursorPainter oldDelegate) =>
      oldDelegate.at != at;
}
