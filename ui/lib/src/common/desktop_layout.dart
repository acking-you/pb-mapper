import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_section.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/widgets/app_window_title_bar.dart';

/// The desktop shell: a title bar across the top, a sidebar, and the content.
///
/// Everything sits on one background. Separation comes from a slightly raised
/// content panel rather than divider lines, which is what made the old window
/// read as three boxed-off regions.
///
/// The sidebar shows only what belongs to the current zone: a workspace lists
/// its own job, not the other role, so registering never puts a connect button
/// in reach. Home has no sidebar at all.
class DesktopLayout extends StatelessWidget {
  final AppSection section;
  final OpsTab opsTab;
  final Widget child;
  final String? title;
  final List<Widget> titleBarActions;
  final VoidCallback onHome;
  final VoidCallback onOps;
  final ValueChanged<OpsTab> onOpsTab;

  const DesktopLayout({
    super.key,
    required this.section,
    required this.opsTab,
    required this.child,
    required this.onHome,
    required this.onOps,
    required this.onOpsTab,
    this.title,
    this.titleBarActions = const [],
  });

  @override
  Widget build(BuildContext context) {
    if (ResponsiveLayout.isMobile(context)) {
      return child;
    }

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final expanded = ResponsiveLayout.isDesktop(context);
    // Home is a choice and setup is a guided flow: neither has anywhere to
    // navigate to, and a sidebar would only offer a way to get lost.
    final showSidebar =
        section != AppSection.home && section != AppSection.setup;
    final railWidth = expanded ? 208.0 : 76.0;

    return Scaffold(
      backgroundColor: scheme.surface,
      body: Column(
        children: [
          if (AppWindowTitleBar.isSupported)
            AppWindowTitleBar(title: title, actions: titleBarActions),
          Expanded(
            child: Row(
              children: [
                if (showSidebar)
                  SizedBox(
                    width: railWidth,
                    child: _Sidebar(
                      expanded: expanded,
                      section: section,
                      opsTab: opsTab,
                      onHome: onHome,
                      onOps: onOps,
                      onOpsTab: onOpsTab,
                    ),
                  ),
                Expanded(
                  child: Padding(
                    // Home has no sidebar, so it needs a left gap too or the
                    // panel would sit flush against the window edge.
                    padding: EdgeInsets.only(
                      left: showSidebar ? 0 : 12,
                      right: 12,
                      bottom: 12,
                    ),
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
                        child: child,
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
    required this.section,
    required this.opsTab,
    required this.onHome,
    required this.onOps,
    required this.onOpsTab,
  });

  final bool expanded;
  final AppSection section;
  final OpsTab opsTab;
  final VoidCallback onHome;
  final VoidCallback onOps;
  final ValueChanged<OpsTab> onOpsTab;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final inOps = section == AppSection.ops;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: EdgeInsets.fromLTRB(expanded ? 12 : 8, 4, 8, 10),
          child: _BackHome(expanded: expanded, onPressed: onHome),
        ),
        Padding(
          padding: EdgeInsets.symmetric(horizontal: expanded ? 12 : 8),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              // In a workspace this single item names the job you are in. In
              // ops it becomes the three ops tabs.
              if (section == AppSection.register)
                _NavItem(
                  icon: Icons.upload_rounded,
                  selectedIcon: Icons.upload_rounded,
                  label: l10n.navRegister,
                  expanded: expanded,
                  selected: true,
                  onPressed: () {},
                )
              else if (section == AppSection.connect)
                _NavItem(
                  icon: Icons.download_rounded,
                  selectedIcon: Icons.download_rounded,
                  label: l10n.navConnect,
                  expanded: expanded,
                  selected: true,
                  onPressed: () {},
                )
              else if (inOps) ...[
                _NavItem(
                  icon: Icons.monitor_outlined,
                  selectedIcon: Icons.monitor,
                  label: l10n.navStatus,
                  expanded: expanded,
                  selected: opsTab == OpsTab.status,
                  onPressed: () => onOpsTab(OpsTab.status),
                ),
                _NavItem(
                  icon: Icons.settings_outlined,
                  selectedIcon: Icons.settings,
                  label: l10n.navConfig,
                  expanded: expanded,
                  selected: opsTab == OpsTab.config,
                  onPressed: () => onOpsTab(OpsTab.config),
                ),
                _NavItem(
                  icon: Icons.terminal_outlined,
                  selectedIcon: Icons.terminal,
                  label: l10n.navLogs,
                  expanded: expanded,
                  selected: opsTab == OpsTab.logs,
                  onPressed: () => onOpsTab(OpsTab.logs),
                ),
              ],
            ],
          ),
        ),
        const Spacer(),
        // Ops is always one click away from a workspace, and never mixed into
        // it. Inside ops the entry is redundant, so it is not drawn.
        if (!inOps)
          Padding(
            padding: EdgeInsets.fromLTRB(
              expanded ? 12 : 8,
              0,
              expanded ? 12 : 8,
              12,
            ),
            child: _NavItem(
              icon: Icons.tune_rounded,
              selectedIcon: Icons.tune_rounded,
              label: l10n.navOps,
              expanded: expanded,
              selected: false,
              onPressed: onOps,
            ),
          ),
      ],
    );
  }
}

/// Leaves the current zone. Reads as a way out, not as a destination.
class _BackHome extends StatelessWidget {
  const _BackHome({required this.expanded, required this.onPressed});

  final bool expanded;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Tooltip(
      message: expanded ? '' : context.l10n.home,
      child: InkWell(
        onTap: onPressed,
        borderRadius: BorderRadius.circular(10),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 9),
          child: Row(
            mainAxisAlignment: expanded
                ? MainAxisAlignment.start
                : MainAxisAlignment.center,
            children: [
              Icon(
                Icons.arrow_back_rounded,
                size: 18,
                color: scheme.onSurfaceVariant,
              ),
              if (expanded) ...[
                const SizedBox(width: 10),
                Expanded(
                  child: Text(
                    context.l10n.home,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodyMedium?.copyWith(
                      color: scheme.onSurfaceVariant,
                      fontWeight: FontWeight.w500,
                    ),
                  ),
                ),
              ],
            ],
          ),
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
