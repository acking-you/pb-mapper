import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';

/// Which layout a page is being shown in, for the pieces that differ: a title
/// bar inside the rounded sheet beside the rail, or across the top of the
/// window above a bottom bar.
class ShellLayout extends InheritedWidget {
  const ShellLayout({super.key, required this.wide, required super.child});

  final bool wide;

  static bool wideOf(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<ShellLayout>()?.wide ?? false;

  @override
  bool updateShouldNotify(ShellLayout old) => old.wide != wide;
}

/// A page's main action in the title bar: labelled when the bar has room,
/// a filled icon button carrying the label as its tooltip when it does not.
class PrimaryAction {
  const PrimaryAction({
    required this.key,
    required this.icon,
    required this.label,
    required this.onPressed,
  });

  final Key key;
  final IconData icon;
  final String label;
  final VoidCallback onPressed;
}

/// A page: its title bar and its body. The bar is window chrome too — its
/// empty space drags the window, and on Windows and Linux it carries the
/// caption buttons.
class PageFrame extends StatelessWidget {
  const PageFrame({
    super.key,
    required this.title,
    required this.body,
    this.actions = const [],
    this.primary,
    this.overlay = false,
  });

  final String title;
  final Widget body;

  /// Quiet actions for this page, before [primary].
  final List<Widget> actions;
  final PrimaryAction? primary;

  /// Let [body] run under the bar, for a page that is one picture (the
  /// galaxy): the bar floats over it instead of cutting a strip off the top.
  final bool overlay;

  /// The bar's height, for a body that lays things out beneath it.
  static double barHeight(BuildContext context) =>
      ShellLayout.wideOf(context) ? 58 : 54 + MediaQuery.paddingOf(context).top;

  @override
  Widget build(BuildContext context) {
    final wide = ShellLayout.wideOf(context);
    final bar = _TitleBar(
      title: title,
      actions: actions,
      primary: primary,
      inSheet: wide,
    );
    if (overlay) {
      return Stack(
        children: [
          Positioned.fill(child: body),
          Positioned(top: 0, left: 0, right: 0, child: bar),
        ],
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        bar,
        Expanded(child: body),
      ],
    );
  }
}

class _TitleBar extends StatelessWidget {
  const _TitleBar({
    required this.title,
    required this.actions,
    required this.primary,
    required this.inSheet,
  });

  final String title;
  final List<Widget> actions;
  final PrimaryAction? primary;
  final bool inSheet;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final height = inSheet ? 58.0 : 54.0;
    // Across the top of the window the traffic lights sit at its corner;
    // inside the sheet the rail has already made room for them.
    final lead = (inSheet ? 0 : trafficLightInset) + (inSheet ? 22.0 : 16.0);
    final roomy = MediaQuery.sizeOf(context).width >= 560;
    final primary = this.primary;
    final bar = SizedBox(
      height: height,
      child: Stack(
        // Centred: a bar with no actions is only as tall as its title.
        alignment: AlignmentDirectional.centerStart,
        children: [
          const Positioned.fill(child: WindowDragArea()),
          Padding(
            padding: EdgeInsetsDirectional.only(start: lead, end: 8),
            child: Row(
              children: [
                Expanded(
                  child: IgnorePointer(
                    child: Text(
                      title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.titleLarge?.copyWith(
                        fontSize: inSheet ? 19 : 18,
                      ),
                    ),
                  ),
                ),
                ...actions,
                if (primary != null) ...[
                  const SizedBox(width: 6),
                  if (roomy)
                    FilledButton.icon(
                      key: primary.key,
                      onPressed: primary.onPressed,
                      style: FilledButton.styleFrom(
                        minimumSize: const Size(0, 38),
                        padding: const EdgeInsetsDirectional.fromSTEB(
                          14,
                          0,
                          18,
                          0,
                        ),
                      ),
                      icon: Icon(primary.icon, size: 18),
                      label: Text(primary.label),
                    )
                  else
                    IconButton.filled(
                      key: primary.key,
                      tooltip: primary.label,
                      onPressed: primary.onPressed,
                      style: IconButton.styleFrom(
                        backgroundColor: theme.colorScheme.primary,
                        foregroundColor: theme.colorScheme.onPrimary,
                      ),
                      icon: Icon(primary.icon),
                    ),
                ],
                if (drawsCaptionButtons) ...[
                  const SizedBox(width: 10),
                  const CaptionButtons(height: 36),
                ],
              ],
            ),
          ),
        ],
      ),
    );
    if (inSheet) return bar;
    return SafeArea(bottom: false, child: bar);
  }
}
