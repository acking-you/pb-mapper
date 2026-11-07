import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:qr/qr.dart';

/// The host's invitation as a QR code for a phone to scan: a white plate in
/// both themes, since a scanner wants dark modules on light. Shown only on
/// request, and never kept (§6.3).
class InviteQrPanel extends StatefulWidget {
  const InviteQrPanel({
    super.key = const Key('host-qr-panel'),
    required this.invitation,
  });

  /// A bearer secret: drawn, never written as text.
  final String invitation;

  /// The code's side, quiet zone included.
  static const double side = 220;

  @override
  State<InviteQrPanel> createState() => _InviteQrPanelState();
}

class _InviteQrPanelState extends State<InviteQrPanel> {
  late QrImage _image = _encode(widget.invitation);

  /// About version 7-M for a `pbd1` invitation of ~115 characters.
  static QrImage _encode(String invitation) => QrImage(
    QrCode.fromData(data: invitation, errorCorrectLevel: QrErrorCorrectLevel.M),
  );

  @override
  void didUpdateWidget(InviteQrPanel old) {
    super.didUpdateWidget(old);
    if (old.invitation != widget.invitation) {
      _image = _encode(widget.invitation);
    }
  }

  @override
  Widget build(BuildContext context) {
    final image = _image;
    return Container(
      padding: const EdgeInsets.all(16),
      decoration: BoxDecoration(
        color: Colors.white,
        borderRadius: BorderRadius.circular(14),
        border: Border.all(color: Colors.black.withValues(alpha: 0.06)),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Semantics(
            image: true,
            label: context.l10n.hostQr,
            child: CustomPaint(
              size: const Size.square(InviteQrPanel.side),
              painter: QrInvitePainter(
                image,
                devicePixelRatio: MediaQuery.devicePixelRatioOf(context),
              ),
            ),
          ),
          const SizedBox(height: 8),
          Text(
            context.l10n.hostQrCaption,
            textAlign: TextAlign.center,
            style: const TextStyle(fontSize: 12.5, color: Color(0xFF4A4A4D)),
          ),
        ],
      ),
    );
  }
}

/// Paints [image] with a four-module quiet zone, each module a whole number
/// of device pixels so no edge blurs between two modules, centred in the
/// space it is given.
class QrInvitePainter extends CustomPainter {
  QrInvitePainter(this.image, {required this.devicePixelRatio});

  final QrImage image;
  final double devicePixelRatio;

  static const _quiet = 4;

  @override
  void paint(Canvas canvas, Size size) {
    final n = image.moduleCount;
    final span = n + 2 * _quiet;
    final dpr = devicePixelRatio;
    final module = ((size.shortestSide * dpr) / span).floorToDouble() / dpr;
    if (module <= 0) return;
    final drawn = module * n;
    final origin = Offset(
      ((size.width - drawn) / 2 * dpr).roundToDouble() / dpr,
      ((size.height - drawn) / 2 * dpr).roundToDouble() / dpr,
    );
    final path = Path();
    for (var row = 0; row < n; row++) {
      for (var col = 0; col < n; col++) {
        if (!image.isDark(row, col)) continue;
        path.addRect(
          Rect.fromLTWH(
            origin.dx + col * module,
            origin.dy + row * module,
            module,
            module,
          ),
        );
      }
    }
    canvas.drawPath(
      path,
      Paint()
        ..color = const Color(0xFF111111)
        ..isAntiAlias = false,
    );
  }

  @override
  bool shouldRepaint(QrInvitePainter old) =>
      old.image != image || old.devicePixelRatio != devicePixelRatio;
}
