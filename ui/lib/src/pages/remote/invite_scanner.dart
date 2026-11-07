import 'dart:async';
import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';

/// Live camera QR reading. Android/iOS only (mobile_scanner 7.5.0, D21):
/// QR only, no duplicates, `returnImage: false`, so no frame bytes reach
/// Dart. The camera stops whenever the app leaves the front and starts
/// again when it returns; otherwise it runs until the scanner goes, which
/// the sheet does once an invitation is recognised.
class InviteScanner extends StatefulWidget {
  const InviteScanner({super.key, required this.onDetect});

  /// Each QR text read, once: a code the sheet rejects does not end
  /// scanning, and is not reported again while it stays in view.
  final ValueChanged<String> onDetect;

  static bool get supported =>
      debugSupportedOverride ?? (Platform.isAndroid || Platform.isIOS);

  @visibleForTesting
  static bool? debugSupportedOverride;

  /// Previews: a still viewfinder, no camera plugin.
  @visibleForTesting
  static bool debugFakeCamera = false;

  /// The viewfinder's height.
  static const double height = 330;

  @override
  State<InviteScanner> createState() => _InviteScannerState();
}

class _InviteScannerState extends State<InviteScanner> {
  MobileScannerController? _controller;

  /// The plugin follows the app's lifecycle only for a controller it made
  /// itself; this one is ours, so the camera's running is ours to follow.
  AppLifecycleListener? _lifecycle;

  /// The last text reported. The plugin forgets what it read each time the
  /// camera starts again; this does not, so a rejected code still in view
  /// is not inspected over and over.
  String? _reported;

  @override
  void initState() {
    super.initState();
    if (!InviteScanner.debugFakeCamera) {
      _controller = MobileScannerController(
        formats: const [BarcodeFormat.qrCode],
        detectionSpeed: DetectionSpeed.noDuplicates,
        returnImage: false,
      );
      _lifecycle = AppLifecycleListener(onStateChange: _onLifecycle);
    }
  }

  @override
  void dispose() {
    _lifecycle?.dispose();
    unawaited(_controller?.dispose());
    super.dispose();
  }

  /// Runs the camera only while the app is in front.
  void _onLifecycle(AppLifecycleState state) {
    final controller = _controller;
    if (controller == null) return;
    final change = state == AppLifecycleState.resumed
        ? controller.start()
        : controller.stop();
    // A refusal (no permission, no camera) shows in the error view.
    unawaited(
      change.catchError((Object error) => debugPrint('Camera: $error')),
    );
  }

  void _onDetect(BarcodeCapture capture) {
    final value = capture.barcodes
        .map((b) => b.rawValue)
        .firstWhere((v) => v != null && v.isNotEmpty, orElse: () => null);
    if (value == null || value == _reported) return;
    _reported = value;
    widget.onDetect(value);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final controller = _controller;
    return ClipRRect(
      borderRadius: BorderRadius.circular(20),
      child: SizedBox(
        height: InviteScanner.height,
        child: ColoredBox(
          color: const Color(0xFF161618),
          child: Stack(
            fit: StackFit.expand,
            children: [
              if (controller != null)
                MobileScanner(
                  controller: controller,
                  onDetect: _onDetect,
                  errorBuilder: (context, error) => _ScannerError(
                    message:
                        error.errorCode ==
                            MobileScannerErrorCode.permissionDenied
                        ? l10n.scanPermissionDenied
                        : l10n.scanUnavailable,
                  ),
                )
              else
                const ColoredBox(color: Color(0xFF1F2328)),
              // The reticle centres in the space the hint leaves, so a hint
              // that wraps never covers its corners.
              IgnorePointer(
                child: Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 14),
                  child: Column(
                    children: [
                      const Expanded(child: Center(child: _Reticle())),
                      Container(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 12,
                          vertical: 6,
                        ),
                        decoration: BoxDecoration(
                          color: Colors.black.withValues(alpha: 0.45),
                          borderRadius: BorderRadius.circular(14),
                        ),
                        child: Text(
                          l10n.scanHint,
                          textAlign: TextAlign.center,
                          style: const TextStyle(
                            fontSize: 13,
                            color: Colors.white,
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _ScannerError extends StatelessWidget {
  const _ScannerError({required this.message});
  final String message;

  @override
  Widget build(BuildContext context) => ColoredBox(
    color: const Color(0xFF161618),
    child: Padding(
      padding: const EdgeInsets.fromLTRB(28, 28, 28, 56),
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          const Icon(
            AppIcons.scanQrCodeLarge,
            size: AppIconSize.tile,
            color: ViewerPalette.textSecondary,
          ),
          const SizedBox(height: 12),
          Text(
            message,
            textAlign: TextAlign.center,
            style: const TextStyle(fontSize: 13.5, color: ViewerPalette.text),
          ),
        ],
      ),
    ),
  );
}

/// Four white corners and a scan line that sweeps between them; the line
/// rests under reduced motion.
class _Reticle extends StatefulWidget {
  const _Reticle();

  @override
  State<_Reticle> createState() => _ReticleState();
}

class _ReticleState extends State<_Reticle>
    with SingleTickerProviderStateMixin {
  late final _sweep = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1600),
  );

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (Motion.reduced(context)) {
      _sweep
        ..stop()
        ..value = 0.58;
    } else if (!_sweep.isAnimating) {
      _sweep.repeat(reverse: true);
    }
  }

  @override
  void dispose() {
    _sweep.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => SizedBox.square(
    dimension: 220,
    child: AnimatedBuilder(
      animation: _sweep,
      builder: (context, _) => CustomPaint(
        painter: _ReticlePainter(Curves.easeInOut.transform(_sweep.value)),
      ),
    ),
  );
}

class _ReticlePainter extends CustomPainter {
  _ReticlePainter(this.at);

  /// Where the scan line is, top 0 to bottom 1.
  final double at;

  @override
  void paint(Canvas canvas, Size size) {
    const arm = 28.0, radius = 10.0;
    final corner = Paint()
      ..color = Colors.white
      ..style = PaintingStyle.stroke
      ..strokeWidth = 3
      ..strokeCap = StrokeCap.round;
    final w = size.width, h = size.height;
    // Each corner as an L with a rounded outer bend, top-left first.
    for (final (x, y, sx, sy) in [
      (0.0, 0.0, 1.0, 1.0),
      (w, 0.0, -1.0, 1.0),
      (0.0, h, 1.0, -1.0),
      (w, h, -1.0, -1.0),
    ]) {
      final path = Path()
        ..moveTo(x, y + sy * arm)
        ..lineTo(x, y + sy * radius)
        ..arcToPoint(
          Offset(x + sx * radius, y),
          radius: const Radius.circular(radius),
          clockwise: sx * sy > 0,
        )
        ..lineTo(x + sx * arm, y);
      canvas.drawPath(path, corner);
    }
    final y = 14 + (h - 28) * at;
    final line = Rect.fromLTRB(14, y - 1, w - 14, y + 1);
    canvas.drawRRect(
      RRect.fromRectAndRadius(line, const Radius.circular(1)),
      Paint()
        ..color = ViewerPalette.signal
        ..maskFilter = const MaskFilter.blur(BlurStyle.outer, 5),
    );
    canvas.drawRRect(
      RRect.fromRectAndRadius(line, const Radius.circular(1)),
      Paint()..color = ViewerPalette.signal,
    );
  }

  @override
  bool shouldRepaint(_ReticlePainter old) => old.at != at;
}
