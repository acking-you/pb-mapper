import 'package:flutter/material.dart';

import '../../../common/l10n_extension.dart';
import '../../../state/desktop_input.dart';
import '../../../theme/app_icons.dart';
import '../../../theme/viewer_palette.dart';
import 'remote_input.dart';

/// Finger travel per wheel notch on the virtual mouse's scroll strip.
const kVirtualWheelNotchPx = 16.0;

/// Mouse buttons and a wheel for direct touch (ui §9.6, D18): left and
/// right click at the cursor, a strip that scrolls, and a grip that moves
/// the pad. Opaque, so a press on it never also lands on the picture.
class VirtualMousePad extends StatefulWidget {
  const VirtualMousePad({
    super.key = const Key('virtual-mouse'),
    required this.input,
    required this.onMove,
  });

  final RemoteInput input;

  /// The grip was dragged by this much: the owner moves the pad, within
  /// the stage.
  final ValueChanged<Offset> onMove;

  static const size = Size(72, 120);

  @override
  State<VirtualMousePad> createState() => _VirtualMousePadState();
}

class _VirtualMousePadState extends State<VirtualMousePad> {
  double _travel = 0;
  bool _scrolling = false;

  RemoteInput get _input => widget.input;

  @override
  void dispose() {
    _endScroll();
    super.dispose();
  }

  void _click(RemoteButton button) => _input.click(button, _input.cursor);

  /// A burst takes the latches at its start and lets them go at its end.
  void _startScroll() {
    _endScroll();
    _travel = 0;
    _scrolling = true;
    _input.wheelStart();
  }

  /// A finger moving down pulls the content down: the wheel goes up.
  void _scroll(double dy) {
    if (!_scrolling) return;
    _travel += dy;
    final notches = (_travel / kVirtualWheelNotchPx).truncate();
    if (notches == 0) return;
    _travel -= notches * kVirtualWheelNotchPx;
    _input.wheel(0, -notches);
  }

  void _endScroll() {
    if (!_scrolling) return;
    _scrolling = false;
    _input.wheelEnd();
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    const divider = BorderSide(color: ViewerPalette.edge);
    Widget button(String face, String label, RemoteButton b) => Expanded(
      child: Semantics(
        button: true,
        label: label,
        excludeSemantics: true,
        onTap: () => _click(b),
        child: InkWell(
          onTap: () => _click(b),
          child: Center(
            child: Text(
              face,
              style: const TextStyle(
                fontSize: 13,
                fontWeight: FontWeight.w500,
                color: ViewerPalette.text,
              ),
            ),
          ),
        ),
      ),
    );
    // Even its hairlines take a press away from the picture.
    final pad = Material(
      color: ViewerPalette.chrome,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.all(Radius.circular(16)),
        side: BorderSide(color: ViewerPalette.edge),
      ),
      clipBehavior: Clip.antiAlias,
      child: SizedBox.fromSize(
        size: VirtualMousePad.size,
        child: Column(
          children: [
            SizedBox(
              height: 48,
              child: Row(
                children: [
                  button('L', l10n.gestureTapResult, RemoteButton.left),
                  const VerticalDivider(
                    width: 1,
                    thickness: 1,
                    color: ViewerPalette.edge,
                  ),
                  button('R', l10n.touchRightClick, RemoteButton.right),
                ],
              ),
            ),
            Semantics(
              label: l10n.gestureThreeFingerResult,
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onVerticalDragStart: (_) => _startScroll(),
                onVerticalDragUpdate: (d) => _scroll(d.delta.dy),
                onVerticalDragEnd: (_) => _endScroll(),
                onVerticalDragCancel: _endScroll,
                child: Container(
                  height: 44,
                  decoration: const BoxDecoration(
                    border: Border(top: divider, bottom: divider),
                  ),
                  child: const Column(
                    mainAxisAlignment: MainAxisAlignment.center,
                    children: [
                      Icon(
                        AppIcons.chevronUp,
                        size: AppIconSize.inline,
                        color: ViewerPalette.textSecondary,
                      ),
                      Icon(
                        AppIcons.chevronDown,
                        size: AppIconSize.inline,
                        color: ViewerPalette.textSecondary,
                      ),
                    ],
                  ),
                ),
              ),
            ),
            Expanded(
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onPanUpdate: (d) => widget.onMove(d.delta),
                child: Center(
                  child: Container(
                    width: 24,
                    height: 4,
                    decoration: BoxDecoration(
                      color: Colors.white.withValues(alpha: 0.3),
                      borderRadius: BorderRadius.circular(2),
                    ),
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
    return Listener(behavior: HitTestBehavior.opaque, child: pad);
  }
}
