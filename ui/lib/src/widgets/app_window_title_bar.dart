import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:window_manager/window_manager.dart';

/// The single title surface for every desktop platform.
///
/// The native title bar is hidden (see `main.dart`), so this row is what the
/// user drags. It shares the shell's background instead of drawing its own, so
/// there is no seam between the window chrome and the content: macOS keeps its
/// real traffic lights inside the surface, Windows and Linux get matching
/// caption buttons rendered by `window_manager`.
class AppWindowTitleBar extends StatefulWidget implements PreferredSizeWidget {
  const AppWindowTitleBar({
    super.key,
    this.title,
    this.actions = const [],
    this.onHome,
  });

  static const double height = 44;

  /// Leading inset that clears the macOS traffic lights.
  static const double macControlsInset = 78;

  final String? title;
  final List<Widget> actions;

  /// Makes the app mark the way home. Null leaves it as a plain label.
  final VoidCallback? onHome;

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
          // Left aligned. A centred title reads as a page heading and competed
          // with the one the content already shows; beside the icon it reads
          // as what the window is. It is also the way home, so it sits outside
          // the drag area — a control cannot share a surface with a gesture
          // that swallows the press.
          _AppMark(title: widget.title, onHome: widget.onHome),
          Expanded(
            child: DragToMoveArea(
              // A double click on the title bar is the platform gesture for
              // maximise, and DragToMoveArea does not provide it.
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onDoubleTap: () async {
                  if (await windowManager.isMaximized()) {
                    await windowManager.unmaximize();
                  } else {
                    await windowManager.maximize();
                  }
                },
                child: const SizedBox.expand(),
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

/// The app icon and name, and the one way home.
///
/// Home used to be a sidebar entry, which put it in a different place in every
/// zone and made it compete with the entries that are actual destinations. The
/// mark is the one thing on screen that never moves, so it carries the trip to
/// the top level instead.
class _AppMark extends StatelessWidget {
  const _AppMark({required this.title, this.onHome});

  /// Keeps a long name from pushing the drag area off the bar.
  static const double _maxWidth = 220;

  final String? title;
  final VoidCallback? onHome;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    final mark = ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: _maxWidth),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 5),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.hub_rounded, size: 15, color: theme.colorScheme.primary),
            const SizedBox(width: 7),
            Flexible(
              child: Text(
                title ?? '',
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.labelLarge?.copyWith(
                  fontWeight: FontWeight.w600,
                  color: theme.colorScheme.onSurface,
                ),
              ),
            ),
          ],
        ),
      ),
    );

    if (onHome == null) {
      return Padding(padding: const EdgeInsets.only(left: 2), child: mark);
    }

    return Padding(
      padding: const EdgeInsets.only(left: 2),
      child: Material(
        color: Colors.transparent,
        borderRadius: BorderRadius.circular(8),
        child: InkWell(
          onTap: onHome,
          borderRadius: BorderRadius.circular(8),
          child: Tooltip(message: context.l10n.home, child: mark),
        ),
      ),
    );
  }
}
