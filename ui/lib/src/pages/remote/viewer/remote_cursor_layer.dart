import 'package:flutter/widgets.dart';

import 'remote_input.dart';
import 'remote_viewport.dart';

/// The remote cursor over the picture (D11, D12): this viewer's own, which
/// follows the host's reports only as [RemoteInput.shownCursor] says, so it
/// never jumps back to where a lagging report left it. The local OS pointer
/// stays visible on desktop.
///
/// Its own layer: the arrow moves with every stroke, hover and report, and
/// repaints nothing around it.
class RemoteCursorLayer extends StatelessWidget {
  const RemoteCursorLayer({
    super.key,
    required this.viewport,
    required this.input,
  });

  final RemoteViewport viewport;
  final RemoteInput input;

  /// Where to draw, in the stage's coordinates.
  Offset? _at() {
    final n = input.shownCursor;
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
            repaint: Listenable.merge([viewport, input.cursorChanges]),
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
