import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/widgets/app_window_title_bar.dart';

/// The desktop shell: a title bar across the top, a sidebar, and the content.
///
/// Everything sits on one background. Separation comes from a slightly raised
/// content panel rather than divider lines, which is what made the old window
/// read as three boxed-off regions.
class DesktopLayout extends StatefulWidget {
  final int selectedIndex;
  final Function(int) onNavigationChanged;
  final Widget child;
  final String? title;
  final List<Widget> titleBarActions;

  const DesktopLayout({
    super.key,
    required this.selectedIndex,
    required this.onNavigationChanged,
    required this.child,
    this.title,
    this.titleBarActions = const [],
  });

  @override
  State<DesktopLayout> createState() => _DesktopLayoutState();
}

class _DesktopLayoutState extends State<DesktopLayout> {
  static const _destinations = [
    (Icons.app_registration_outlined, Icons.app_registration, 'Register'),
    (Icons.cable_outlined, Icons.cable, 'Connect'),
    (Icons.monitor_outlined, Icons.monitor, 'Status'),
    (Icons.settings_outlined, Icons.settings, 'Config'),
    (Icons.terminal_outlined, Icons.terminal, 'Logs'),
  ];

  @override
  Widget build(BuildContext context) {
    if (ResponsiveLayout.isMobile(context)) {
      return widget.child;
    }

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final expanded = ResponsiveLayout.isDesktop(context);
    final railWidth = expanded ? 208.0 : 76.0;

    return Scaffold(
      backgroundColor: scheme.surface,
      body: Column(
        children: [
          if (AppWindowTitleBar.isSupported)
            AppWindowTitleBar(
              title: widget.title,
              actions: widget.titleBarActions,
            ),
          Expanded(
            child: Row(
              children: [
                SizedBox(
                  width: railWidth,
                  child: _Sidebar(
                    expanded: expanded,
                    selectedIndex: widget.selectedIndex,
                    destinations: _destinations,
                    onNavigationChanged: widget.onNavigationChanged,
                  ),
                ),
                Expanded(
                  child: Padding(
                    // No right/bottom gap on the inset panel would put content
                    // flush against the window edge.
                    padding: const EdgeInsets.only(right: 12, bottom: 12),
                    child: DecoratedBox(
                      decoration: BoxDecoration(
                        // A hair lighter than the shell in dark mode, a hair
                        // darker in light mode: enough to read as a panel
                        // without a border.
                        color: theme.brightness == Brightness.dark
                            ? scheme.surfaceContainerLow
                            : scheme.surfaceContainerLowest,
                        borderRadius: BorderRadius.circular(12),
                      ),
                      child: ClipRRect(
                        borderRadius: BorderRadius.circular(12),
                        child: widget.child,
                      ),
                    ),
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _Sidebar extends StatelessWidget {
  const _Sidebar({
    required this.expanded,
    required this.selectedIndex,
    required this.destinations,
    required this.onNavigationChanged,
  });

  final bool expanded;
  final int selectedIndex;
  final List<(IconData, IconData, String)> destinations;
  final Function(int) onNavigationChanged;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: EdgeInsets.fromLTRB(expanded ? 16 : 8, 4, 8, 12),
          child: _HomeButton(
            expanded: expanded,
            selected: selectedIndex == 0,
            onPressed: () => onNavigationChanged(0),
          ),
        ),
        for (var i = 0; i < destinations.length; i++)
          Padding(
            padding: EdgeInsets.symmetric(horizontal: expanded ? 12 : 8),
            child: _NavItem(
              icon: destinations[i].$1,
              selectedIcon: destinations[i].$2,
              label: destinations[i].$3,
              expanded: expanded,
              selected: selectedIndex == i + 1,
              onPressed: () => onNavigationChanged(i + 1),
            ),
          ),
        const Spacer(),
        if (expanded)
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 0, 12, 14),
            child: Text(
              'pb-mapper',
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.onSurfaceVariant.withValues(alpha: 0.6),
              ),
            ),
          ),
      ],
    );
  }
}

class _HomeButton extends StatelessWidget {
  const _HomeButton({
    required this.expanded,
    required this.selected,
    required this.onPressed,
  });

  final bool expanded;
  final bool selected;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return InkWell(
      onTap: onPressed,
      borderRadius: BorderRadius.circular(10),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 8),
        child: Row(
          mainAxisAlignment: expanded
              ? MainAxisAlignment.start
              : MainAxisAlignment.center,
          children: [
            Icon(
              Icons.hub_rounded,
              size: 20,
              color: selected ? scheme.primary : scheme.onSurfaceVariant,
            ),
            if (expanded) ...[
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  'pb-mapper',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.titleSmall?.copyWith(
                    fontWeight: FontWeight.w700,
                    color: scheme.onSurface,
                  ),
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _NavItem extends StatelessWidget {
  const _NavItem({
    required this.icon,
    required this.selectedIcon,
    required this.label,
    required this.expanded,
    required this.selected,
    required this.onPressed,
  });

  final IconData icon;
  final IconData selectedIcon;
  final String label;
  final bool expanded;
  final bool selected;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final foreground = selected ? scheme.onSecondaryContainer : scheme.onSurfaceVariant;

    final content = Row(
      mainAxisAlignment: expanded
          ? MainAxisAlignment.start
          : MainAxisAlignment.center,
      children: [
        Icon(selected ? selectedIcon : icon, size: 20, color: foreground),
        if (expanded) ...[
          const SizedBox(width: 12),
          Expanded(
            child: Text(
              label,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodyMedium?.copyWith(
                color: foreground,
                fontWeight: selected ? FontWeight.w600 : FontWeight.w500,
              ),
            ),
          ),
        ],
      ],
    );

    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Material(
        color: selected ? scheme.secondaryContainer : Colors.transparent,
        borderRadius: BorderRadius.circular(10),
        child: InkWell(
          onTap: onPressed,
          borderRadius: BorderRadius.circular(10),
          child: Tooltip(
            message: expanded ? '' : label,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 11),
              child: content,
            ),
          ),
        ),
      ),
    );
  }
}

class ResponsiveScaffold extends StatelessWidget {
  final String? title;
  final Widget body;
  final List<Widget>? actions;
  final Widget? floatingActionButton;
  final Widget? bottomNavigationBar;
  final bool showBackButton;

  const ResponsiveScaffold({
    super.key,
    this.title,
    required this.body,
    this.actions,
    this.floatingActionButton,
    this.bottomNavigationBar,
    this.showBackButton = false,
  });

  @override
  Widget build(BuildContext context) {
    if (ResponsiveLayout.isMobile(context)) {
      return Scaffold(
        appBar: AppBar(
          title: title != null ? Text(title!) : null,
          actions: actions,
          automaticallyImplyLeading: showBackButton,
        ),
        body: body,
        floatingActionButton: floatingActionButton,
        bottomNavigationBar: bottomNavigationBar,
      );
    }

    // The desktop shell already draws the title bar, so this only carries the
    // page. A second AppBar here is what produced the stacked headers.
    return Scaffold(
      backgroundColor: Colors.transparent,
      appBar: title != null
          ? AppBar(
              title: Text(title!),
              actions: actions,
              automaticallyImplyLeading: false,
              backgroundColor: Colors.transparent,
              surfaceTintColor: Colors.transparent,
              elevation: 0,
              scrolledUnderElevation: 0,
            )
          : null,
      body: ResponsiveLayout.wrapWithMaxWidth(context: context, child: body),
      floatingActionButton: floatingActionButton,
    );
  }
}
