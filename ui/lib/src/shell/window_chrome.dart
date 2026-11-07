import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';
import 'package:window_manager/window_manager.dart';

/// The native title bar is hidden on every desktop (see `main.dart`), so the
/// app draws one continuous surface and its own chrome: the top of the page
/// drags the window, macOS keeps its traffic lights inside the surface, and
/// Windows and Linux get caption buttons drawn here.
///
/// Gated on [defaultTargetPlatform] rather than `dart:io`, so `flutter test` —
/// forced to android — never talks to a window that does not exist.
bool get isFramelessDesktop =>
    !kIsWeb &&
    (defaultTargetPlatform == TargetPlatform.windows ||
        defaultTargetPlatform == TargetPlatform.macOS ||
        defaultTargetPlatform == TargetPlatform.linux);

/// Whether the app draws its own minimize / maximize / close.
bool get drawsCaptionButtons =>
    isFramelessDesktop && defaultTargetPlatform != TargetPlatform.macOS;

/// Room the macOS traffic lights take at the window's leading edge.
double get trafficLightInset =>
    isFramelessDesktop && defaultTargetPlatform == TargetPlatform.macOS
    ? 76
    : 0;

/// Empty chrome that drags the window, and maximizes on a double click as the
/// platform's own title bar would.
class WindowDragArea extends StatelessWidget {
  const WindowDragArea({super.key, this.child});

  final Widget? child;

  @override
  Widget build(BuildContext context) {
    final content = child ?? const SizedBox.expand();
    if (!isFramelessDesktop) return content;
    return DragToMoveArea(
      child: GestureDetector(
        behavior: HitTestBehavior.translucent,
        onDoubleTap: _toggleMaximize,
        child: content,
      ),
    );
  }
}

Future<void> _toggleMaximize() async {
  try {
    if (await windowManager.isMaximized()) {
      await windowManager.unmaximize();
    } else {
      await windowManager.maximize();
    }
  } catch (_) {
    // Best effort: no window (a test) or a platform that refuses.
  }
}

/// What the caption buttons are drawn over: the app's own surfaces, or the
/// remote desktop viewer's dark title bar.
enum CaptionTone { surface, viewer }

/// The four marks a caption button draws.
enum CaptionGlyph { minimize, maximize, restore, close }

/// One caption button is Windows' own 46 px wide. It is as tall as the title
/// bar it sits over, as Windows' are in a tall title bar that holds
/// controls: a page's or the viewer's. Where there is no bar (loading and
/// first-run setup) it is Windows' standard 32.
const double kCaptionButtonWidth = 46;
const double kCaptionButtonHeight = 32;

/// The three together: what a title bar keeps clear at its end.
const double kCaptionButtonsWidth = 3 * kCaptionButtonWidth;

/// The band along a restored window's top edge that resizes it on Windows,
/// over the caption buttons too, as a native window's top edge does. The
/// hidden title bar (window_manager 0.5.1) leaves Windows at most one
/// physical pixel there; the other edges are Windows' own.
const double kTopResizeBand = 4;

/// The glyph's box, in logical pixels.
const double kCaptionGlyphExtent = 10;

/// Windows' close red.
const Color _closeRed = Color(0xFFC42B1C);

/// Windows' TextFillColorDisabled: caption glyphs in a window that is not
/// the active one.
const double _inactiveAlpha = 0.36;

/// The window's own controls on Windows and Linux, which lose theirs with
/// the native title bar: minimize, maximize-or-restore and close, and on
/// Windows the top edge that resizes a restored window. One for the app's
/// whole life, above the navigator (`app.dart`), so no page transition moves
/// or fades the buttons and no dialog's barrier dims them, as none touches
/// Windows' own.
///
/// Fill the window with it: the buttons sit flush in its top-right corner,
/// [height] tall, the title bar on screen keeping [kCaptionButtonsWidth]
/// clear at its end. Their hover fills run to both edges and the close red
/// into the corner, which Windows rounds itself; a maximised window's close
/// is where the pointer stops at the screen's corner.
///
/// It owns the window state they show: maximised (the middle glyph),
/// focused (idle glyphs go grey, as Windows' do) and full screen, where it
/// hides.
class WindowControls extends StatefulWidget {
  const WindowControls({
    super.key,
    required this.height,
    this.tone = CaptionTone.surface,
  });

  /// The title bar's height, or [kCaptionButtonHeight] where there is none.
  final double height;
  final CaptionTone tone;

  @override
  State<WindowControls> createState() => _WindowControlsState();
}

class _WindowControlsState extends State<WindowControls> with WindowListener {
  bool _maximized = false;
  bool _fullScreen = false;
  bool _focused = true;

  @override
  void initState() {
    super.initState();
    windowManager.addListener(this);
    _sync();
  }

  @override
  void dispose() {
    windowManager.removeListener(this);
    super.dispose();
  }

  Future<void> _sync() async {
    Future<bool> ask(Future<bool> Function() question, bool known) async {
      try {
        return await question();
      } catch (_) {
        // No window to ask (a test) or a platform that refuses.
        return known;
      }
    }

    final (maximized, fullScreen, focused) = await (
      ask(windowManager.isMaximized, _maximized),
      ask(windowManager.isFullScreen, _fullScreen),
      ask(windowManager.isFocused, _focused),
    ).wait;
    if (!mounted) return;
    setState(() {
      _maximized = maximized;
      _fullScreen = fullScreen;
      _focused = focused;
    });
  }

  @override
  void onWindowMaximize() => setState(() => _maximized = true);

  @override
  void onWindowUnmaximize() => setState(() => _maximized = false);

  // Leaving a minimized or full-screen window can land either way.
  @override
  void onWindowRestore() => _sync();

  @override
  void onWindowEnterFullScreen() {
    setState(() => _fullScreen = true);
    _sync();
  }

  @override
  void onWindowLeaveFullScreen() {
    setState(() => _fullScreen = false);
    _sync();
  }

  @override
  void onWindowFocus() => setState(() => _focused = true);

  @override
  void onWindowBlur() => setState(() => _focused = false);

  @override
  Widget build(BuildContext context) {
    if (_fullScreen) return const SizedBox.shrink();
    final buttons = _CaptionButtons(
      tone: widget.tone,
      height: widget.height,
      maximized: _maximized,
      active: _focused,
    );
    return Stack(
      children: [
        Positioned(
          top: 0,
          right: 0,
          // Over the viewer, its tooltips wear the viewer's palette.
          child: widget.tone == CaptionTone.viewer
              ? Theme(data: viewerTheme(context), child: buttons)
              : buttons,
        ),
        if (defaultTargetPlatform == TargetPlatform.windows && !_maximized)
          const Positioned(
            key: Key('window-resize-top'),
            top: 0,
            left: 0,
            right: 0,
            height: kTopResizeBand,
            child: DragToResizeArea(
              resizeEdgeSize: kTopResizeBand,
              enableResizeEdges: [
                ResizeEdge.topLeft,
                ResizeEdge.top,
                ResizeEdge.topRight,
              ],
              child: SizedBox.expand(),
            ),
          ),
      ],
    );
  }
}

class _CaptionButtons extends StatelessWidget {
  const _CaptionButtons({
    required this.tone,
    required this.height,
    required this.maximized,
    required this.active,
  });

  final CaptionTone tone;
  final double height;
  final bool maximized, active;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    // Whole device pixels, so every plate's edges land on the pixel grid at
    // 125 % and 150 % and the close red has no fringe. The group still ends
    // at the window's right edge, which is a whole device pixel too.
    final dpr = MediaQuery.devicePixelRatioOf(context);
    final size = Size(
      (kCaptionButtonWidth * dpr).roundToDouble() / dpr,
      (height * dpr).roundToDouble() / dpr,
    );
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        _CaptionButton(
          key: const Key('caption-minimize'),
          glyph: CaptionGlyph.minimize,
          tooltip: l10n.windowMinimize,
          tone: tone,
          size: size,
          active: active,
          onTap: () => windowManager.minimize(),
        ),
        _CaptionButton(
          key: const Key('caption-maximize'),
          glyph: maximized ? CaptionGlyph.restore : CaptionGlyph.maximize,
          tooltip: maximized ? l10n.windowRestore : l10n.windowMaximize,
          tone: tone,
          size: size,
          active: active,
          onTap: _toggleMaximize,
        ),
        // Closing hides to the tray: main.dart owns that through
        // setPreventClose, so this goes through close() rather than exiting.
        _CaptionButton(
          key: const Key('caption-close'),
          glyph: CaptionGlyph.close,
          tooltip: l10n.windowClose,
          tone: tone,
          size: size,
          active: active,
          close: true,
          onTap: () => windowManager.close(),
        ),
      ],
    );
  }
}

class _CaptionButton extends StatefulWidget {
  const _CaptionButton({
    super.key,
    required this.glyph,
    required this.tooltip,
    required this.onTap,
    required this.tone,
    required this.size,
    required this.active,
    this.close = false,
  });

  final CaptionGlyph glyph;
  final String tooltip;
  final VoidCallback onTap;
  final CaptionTone tone;
  final Size size;

  /// Whether the window is the active one; if not, the idle glyph greys.
  final bool active;
  final bool close;

  @override
  State<_CaptionButton> createState() => _CaptionButtonState();
}

class _CaptionButtonState extends State<_CaptionButton> {
  bool _hover = false;
  bool _pressed = false;

  void _press(bool pressed) => setState(() => _pressed = pressed);

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    // Idle and lit ink, and the washes under hover and press, per tone.
    final (idle, lit, hover, pressed) = switch (widget.tone) {
      CaptionTone.surface => (
        scheme.onSurfaceVariant,
        scheme.onSurface,
        scheme.onSurface.withValues(alpha: 0.08),
        scheme.onSurface.withValues(alpha: 0.12),
      ),
      CaptionTone.viewer => (
        ViewerPalette.textSecondary,
        ViewerPalette.text,
        Colors.white.withValues(alpha: 0.09),
        Colors.white.withValues(alpha: 0.14),
      ),
    };
    final Color bg;
    final Color fg;
    if (widget.close && _pressed) {
      bg = _closeRed.withValues(alpha: 0.9);
      fg = Colors.white.withValues(alpha: 0.9);
    } else if (widget.close && _hover) {
      bg = _closeRed;
      fg = Colors.white;
    } else if (_pressed) {
      bg = pressed;
      fg = lit;
    } else if (_hover) {
      bg = hover;
      fg = lit;
    } else {
      bg = Colors.transparent;
      fg = widget.active ? idle : lit.withValues(alpha: _inactiveAlpha);
    }
    return Tooltip(
      message: widget.tooltip,
      excludeFromSemantics: true,
      child: Semantics(
        button: true,
        label: widget.tooltip,
        child: MouseRegion(
          onEnter: (_) => setState(() => _hover = true),
          onExit: (_) => setState(() => _hover = false),
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTapDown: (_) => _press(true),
            onTapUp: (_) => _press(false),
            onTapCancel: () => _press(false),
            onTap: widget.onTap,
            child: AnimatedContainer(
              duration: Motion.of(context, const Duration(milliseconds: 90)),
              width: widget.size.width,
              height: widget.size.height,
              color: bg,
              child: Center(
                child: CaptionGlyphIcon(glyph: widget.glyph, color: fg),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// A caption glyph, drawn rather than taken from an icon font so its one
/// pixel stroke lands on whole device pixels at every scale factor.
class CaptionGlyphIcon extends LeafRenderObjectWidget {
  const CaptionGlyphIcon({super.key, required this.glyph, required this.color});

  final CaptionGlyph glyph;
  final Color color;

  @override
  RenderCaptionGlyph createRenderObject(BuildContext context) =>
      RenderCaptionGlyph(
        glyph: glyph,
        color: color,
        devicePixelRatio: MediaQuery.devicePixelRatioOf(context),
      );

  @override
  void updateRenderObject(
    BuildContext context,
    RenderCaptionGlyph renderObject,
  ) {
    renderObject
      ..glyph = glyph
      ..color = color
      ..devicePixelRatio = MediaQuery.devicePixelRatioOf(context);
  }
}

/// Paints a [CaptionGlyph] in a [kCaptionGlyphExtent] box, in device pixels:
/// the box starts on a device pixel, its side and stroke are whole device
/// pixels, and odd strokes sit on pixel centres, so the lines stay sharp at
/// 125 % and 150 % rather than smearing across two rows.
class RenderCaptionGlyph extends RenderBox {
  RenderCaptionGlyph({
    required CaptionGlyph glyph,
    required Color color,
    required double devicePixelRatio,
  }) : _glyph = glyph,
       _color = color,
       _devicePixelRatio = devicePixelRatio;

  CaptionGlyph get glyph => _glyph;
  CaptionGlyph _glyph;
  set glyph(CaptionGlyph value) {
    if (value == _glyph) return;
    _glyph = value;
    markNeedsPaint();
  }

  Color get color => _color;
  Color _color;
  set color(Color value) {
    if (value == _color) return;
    _color = value;
    markNeedsPaint();
  }

  double get devicePixelRatio => _devicePixelRatio;
  double _devicePixelRatio;
  set devicePixelRatio(double value) {
    if (value == _devicePixelRatio) return;
    _devicePixelRatio = value;
    markNeedsPaint();
  }

  @override
  bool get sizedByParent => true;

  @override
  Size computeDryLayout(BoxConstraints constraints) =>
      constraints.constrain(const Size.square(kCaptionGlyphExtent));

  @override
  void paint(PaintingContext context, Offset offset) {
    final dpr = _devicePixelRatio;
    // Nudge the box onto the device pixel grid.
    final g = localToGlobal(Offset.zero) * dpr;
    final snap = Offset(
      (g.dx.roundToDouble() - g.dx) / dpr,
      (g.dy.roundToDouble() - g.dy) / dpr,
    );
    final s = math.max(1, dpr.round());
    final b = (kCaptionGlyphExtent * dpr).roundToDouble();
    final h = s / 2;
    final r = Radius.circular(math.max(1, dpr.round()).toDouble());
    final o = (2 * dpr).roundToDouble();
    final paint = Paint()
      ..color = _color
      ..style = PaintingStyle.stroke
      ..strokeWidth = s.toDouble()
      ..strokeCap = StrokeCap.butt
      ..isAntiAlias = true;

    final canvas = context.canvas
      ..save()
      ..translate(offset.dx + snap.dx, offset.dy + snap.dy)
      ..scale(1 / dpr);
    switch (_glyph) {
      case CaptionGlyph.minimize:
        final y = (b / 2).floorToDouble() + (s.isOdd ? 0.5 : 0);
        canvas.drawLine(Offset(0, y), Offset(b, y), paint);
      case CaptionGlyph.maximize:
        canvas.drawRRect(RRect.fromLTRBR(h, h, b - h, b - h, r), paint);
      case CaptionGlyph.restore:
        // The window in front, and the visible top-right of the one behind.
        canvas.drawRRect(RRect.fromLTRBR(h, o + h, b - o - h, b - h, r), paint);
        canvas.drawPath(
          Path()
            ..moveTo(o + h, o + h)
            ..lineTo(o + h, h + r.x)
            ..arcToPoint(Offset(o + h + r.x, h), radius: r)
            ..lineTo(b - h - r.x, h)
            ..arcToPoint(Offset(b - h, h + r.x), radius: r)
            ..lineTo(b - h, b - o - h - r.x)
            ..arcToPoint(Offset(b - h - r.x, b - o - h), radius: r)
            ..lineTo(b - o - h, b - o - h),
          paint,
        );
      case CaptionGlyph.close:
        canvas
          ..drawLine(Offset(h, h), Offset(b - h, b - h), paint)
          ..drawLine(Offset(b - h, h), Offset(h, b - h), paint);
    }
    canvas.restore();
  }
}
