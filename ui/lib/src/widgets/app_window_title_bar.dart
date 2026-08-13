import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';

/// The single title surface for every desktop platform.
///
/// The native title bar is hidden (see `main.dart`), so this row is what the
/// user drags. It shares the shell's background instead of drawing its own, so
/// there is no seam between the window chrome and the content: macOS keeps its
/// real traffic lights inside the surface, Windows and Linux get matching
/// caption buttons rendered by `window_manager`.
class AppWindowTitleBar extends StatefulWidget implements PreferredSizeWidget {
  const AppWindowTitleBar({super.key, this.title, this.actions = const []});

  static const double height = 44;

  /// Leading inset that clears the macOS traffic lights.
  static const double macControlsInset = 78;

  final String? title;
  final List<Widget> actions;

  static bool get isSupported =>
      Platform.isWindows || Platform.isLinux || Platform.isMacOS;

  @override
  Size get preferredSize => const Size.fromHeight(height);

  @override
  State<AppWindowTitleBar> createState() => _AppWindowTitleBarState();
}

class _AppWindowTitleBarState extends State<AppWindowTitleBar>
    with WindowListener {
  bool _isMaximized = false;

  bool get _usesNativeMacControls => Platform.isMacOS;

  @override
  void initState() {
    super.initState();
    windowManager.addListener(this);
    _refreshMaximizedState();
  }

  @override
  void dispose() {
    windowManager.removeListener(this);
    super.dispose();
  }

  Future<void> _refreshMaximizedState() async {
    final isMaximized = await windowManager.isMaximized();
    if (mounted && isMaximized != _isMaximized) {
      setState(() => _isMaximized = isMaximized);
    }
  }

  @override
  void onWindowMaximize() => setState(() => _isMaximized = true);

  @override
  void onWindowUnmaximize() => setState(() => _isMaximized = false);

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return SizedBox(
      height: AppWindowTitleBar.height,
      child: Row(
        children: [
          if (_usesNativeMacControls)
            const SizedBox(width: AppWindowTitleBar.macControlsInset),
          Expanded(
            child: DragToMoveArea(
              // A double click on the title bar is the platform gesture for
              // maximise, and DragToMoveArea does not provide it.
              child: GestureDetector(
                onDoubleTap: () async {
                  if (await windowManager.isMaximized()) {
                    await windowManager.unmaximize();
                  } else {
                    await windowManager.maximize();
                  }
                },
                child: Align(
                  alignment: Alignment.center,
                  child: Text(
                    widget.title ?? '',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.titleSmall?.copyWith(
                      fontWeight: FontWeight.w600,
                      color: theme.colorScheme.onSurface,
                    ),
                  ),
                ),
              ),
            ),
          ),
          ...widget.actions,
          if (!_usesNativeMacControls) ...[
            const SizedBox(width: 4),
            WindowCaptionButton.minimize(
              brightness: theme.brightness,
              onPressed: windowManager.minimize,
            ),
            _isMaximized
                ? WindowCaptionButton.unmaximize(
                    brightness: theme.brightness,
                    onPressed: windowManager.unmaximize,
                  )
                : WindowCaptionButton.maximize(
                    brightness: theme.brightness,
                    onPressed: windowManager.maximize,
                  ),
            // Closing hides to the tray; main.dart owns that decision through
            // setPreventClose, so route through close() rather than exiting.
            WindowCaptionButton.close(
              brightness: theme.brightness,
              onPressed: windowManager.close,
            ),
          ] else
            const SizedBox(width: 8),
        ],
      ),
    );
  }
}
