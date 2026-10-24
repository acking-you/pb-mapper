import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
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

/// Minimize, maximize-or-restore and close, drawn for Windows and Linux,
/// which lose theirs with the native title bar.
class CaptionButtons extends StatefulWidget {
  const CaptionButtons({super.key, this.height = 40});

  final double height;

  @override
  State<CaptionButtons> createState() => _CaptionButtonsState();
}

class _CaptionButtonsState extends State<CaptionButtons> with WindowListener {
  bool _maximized = false;

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
    try {
      final maximized = await windowManager.isMaximized();
      if (mounted && maximized != _maximized) {
        setState(() => _maximized = maximized);
      }
    } catch (_) {}
  }

  @override
  void onWindowMaximize() => setState(() => _maximized = true);

  @override
  void onWindowUnmaximize() => setState(() => _maximized = false);

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        _CaptionButton(
          icon: Icons.remove,
          tooltip: l10n.windowMinimize,
          height: widget.height,
          onTap: () => windowManager.minimize(),
        ),
        _CaptionButton(
          icon: _maximized ? Icons.filter_none : Icons.crop_square,
          iconSize: _maximized ? 12.5 : 14,
          tooltip: _maximized ? l10n.windowRestore : l10n.windowMaximize,
          height: widget.height,
          onTap: _toggleMaximize,
        ),
        // Closing hides to the tray: main.dart owns that through
        // setPreventClose, so this goes through close() rather than exiting.
        _CaptionButton(
          icon: Icons.close,
          tooltip: l10n.windowClose,
          height: widget.height,
          close: true,
          onTap: () => windowManager.close(),
        ),
      ],
    );
  }
}

class _CaptionButton extends StatefulWidget {
  const _CaptionButton({
    required this.icon,
    required this.tooltip,
    required this.onTap,
    required this.height,
    this.close = false,
    this.iconSize = 15,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback onTap;
  final double height;
  final bool close;
  final double iconSize;

  @override
  State<_CaptionButton> createState() => _CaptionButtonState();
}

class _CaptionButtonState extends State<_CaptionButton> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final Color bg;
    final Color fg;
    if (_hover && widget.close) {
      bg = const Color(0xFFC42B1C);
      fg = Colors.white;
    } else if (_hover) {
      bg = scheme.onSurface.withValues(alpha: 0.08);
      fg = scheme.onSurface;
    } else {
      bg = Colors.transparent;
      fg = scheme.onSurfaceVariant;
    }
    return Tooltip(
      message: widget.tooltip,
      child: MouseRegion(
        onEnter: (_) => setState(() => _hover = true),
        onExit: (_) => setState(() => _hover = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onTap,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 90),
            width: 44,
            height: widget.height,
            decoration: BoxDecoration(
              color: bg,
              borderRadius: BorderRadius.circular(10),
            ),
            alignment: Alignment.center,
            child: Icon(widget.icon, size: widget.iconSize, color: fg),
          ),
        ),
      ),
    );
  }
}
