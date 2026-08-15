import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_destination.dart';
import 'package:pb_mapper_ui/src/common/app_section.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/nav_transitions.dart';
import 'package:pb_mapper_ui/src/common/responsive_layout.dart';
import 'package:pb_mapper_ui/src/common/workspace_pane.dart';
import 'package:pb_mapper_ui/src/widgets/app_bottom_nav.dart';
import 'package:pb_mapper_ui/src/widgets/app_window_title_bar.dart';

/// The desktop shell: a title bar across the top, a sidebar, and the content.
///
/// Everything sits on one background. Separation comes from a slightly raised
/// content panel rather than divider lines, which is what made the old window
/// read as three boxed-off regions.
///
/// The sidebar shows only what belongs to the current zone. Its top slot says
/// how to leave, and that differs by zone: the two workspaces are peers, so it
/// swaps between them, while ops is somewhere you drop into and back out of.
/// Home itself is reached from the app mark in the title bar, which is the one
/// control that sits in the same place everywhere. Home has no sidebar at all.
class DesktopLayout extends StatefulWidget {
  final AppSection section;
  final OpsTab opsTab;
  final Widget child;
  final String? title;

  /// Names the current page in the compact toolbar. Workspaces show their
  /// switcher there instead, so this is only read outside them.
  final String? pageTitle;

  final List<Widget> titleBarActions;
  final VoidCallback onHome;
  final VoidCallback onBack;
  final ValueChanged<AppSection> onSwitchWorkspace;
  final VoidCallback onOps;
  final ValueChanged<OpsTab> onOpsTab;

  /// Which half of a workspace is showing, and how many items its list holds.
  final WorkspacePane pane;
  final ValueChanged<WorkspacePane> onPane;
  final int itemCount;

  const DesktopLayout({
    super.key,
    required this.section,
    required this.opsTab,
    required this.child,
    required this.onHome,
    required this.onBack,
    required this.onSwitchWorkspace,
    required this.onOps,
    required this.onOpsTab,
    required this.pane,
    required this.onPane,
    this.itemCount = 0,
    this.title,
    this.pageTitle,
    this.titleBarActions = const [],
  });

  /// How long the rail takes to become the bottom bar, and back.
  static const Duration transition = Duration(milliseconds: 700);

  static const double railWidth = 208;

  @override
  State<DesktopLayout> createState() => _DesktopLayoutState();
}

class _DesktopLayoutState extends State<DesktopLayout>
    with SingleTickerProviderStateMixin {
  /// 1 is the wide layout, 0 the compact one.
  late final AnimationController _controller;

  /// The rail moves on the back half, the bar and its toolbar on the front, so
  /// the outgoing one is gone before the incoming one starts to arrive.
  late final CurvedAnimation _railAnimation;
  late final Animation<double> _barAnimation;

  bool _settled = false;

  @override
  void initState() {
    super.initState();
    _controller = AnimationController(
      vsync: this,
      duration: DesktopLayout.transition,
      value: 1,
    );
    _railAnimation = CurvedAnimation(
      parent: _controller,
      curve: const Interval(0.5, 1),
    );
    _barAnimation = ReverseAnimation(
      CurvedAnimation(parent: _controller, curve: const Interval(0, 0.5)),
    );
  }

  @override
  void dispose() {
    _railAnimation.dispose();
    _controller.dispose();
    super.dispose();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final wide = !ResponsiveLayout.usesBottomNav(context);

    // The first frame is wherever the window already is. Animating into it
    // would play a transition nobody asked for every time the app opens.
    if (!_settled) {
      _settled = true;
      _controller.value = wide ? 1 : 0;
      return;
    }
    if (wide) {
      _controller.forward();
    } else {
      _controller.reverse();
    }
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _controller,
      builder: (context, _) => _buildShell(context),
    );
  }

  Widget _buildShell(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    // Home is a choice and setup is a guided flow: neither has anywhere to
    // navigate to, and navigation would only offer a way to get lost.
    final showNav =
        widget.section != AppSection.home && widget.section != AppSection.setup;

    final destinations = showNav
        ? destinationsFor(
            context,
            section: widget.section,
            opsTab: widget.opsTab,
            pane: widget.pane,
            itemCount: widget.itemCount,
            onPane: widget.onPane,
            onOpsTab: widget.onOpsTab,
          )
        : const <AppDestination>[];

    // Both navigations exist only while the layout is moving between them. At
    // rest the one that is folded away is left out of the tree entirely: it is
    // clipped to nothing either way, but a zero-height bar still built is a set
    // of destinations a screen reader would read out twice.
    final railPresent = showNav && _controller.value > 0;
    final barPresent = showNav && _controller.value < 1;

    return Scaffold(
      backgroundColor: scheme.surface,
      appBar: _chrome(context, showToolbar: showNav && barPresent),
      body: Row(
        children: [
          if (railPresent)
            RailTransition(
              animation: _railAnimation,
              backgroundColor: scheme.surface,
              child: SizedBox(
                width: DesktopLayout.railWidth,
                child: _Sidebar(
                  expanded: true,
                  section: widget.section,
                  opsTab: widget.opsTab,
                  onBack: widget.onBack,
                  onSwitchWorkspace: widget.onSwitchWorkspace,
                  onOps: widget.onOps,
                  onOpsTab: widget.onOpsTab,
                  pane: widget.pane,
                  onPane: widget.onPane,
                  itemCount: widget.itemCount,
                ),
              ),
            ),
          Expanded(child: _panel(context)),
        ],
      ),
      bottomNavigationBar: (destinations.isEmpty || !barPresent)
          ? null
          : BarTransition(
              animation: _barAnimation,
              backgroundColor: scheme.surface,
              child: AppBottomNav(destinations: destinations),
            ),
    );
  }

  /// The window title bar, and under it the toolbar the compact layout needs
  /// for the controls the rail's top slot carries when there is a rail.
  PreferredSizeWidget? _chrome(
    BuildContext context, {
    required bool showToolbar,
  }) {
    final hasTitleBar = AppWindowTitleBar.isSupported;
    final toolbarFactor = showToolbar ? _barAnimation.value : 0.0;
    final height =
        (hasTitleBar ? AppWindowTitleBar.height : 0) +
        kToolbarHeight * toolbarFactor;

    if (height == 0) return null;

    return PreferredSize(
      preferredSize: Size.fromHeight(height),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (hasTitleBar)
            AppWindowTitleBar(
              title: widget.title,
              actions: widget.titleBarActions,
              onHome: widget.onHome,
            ),
          if (showToolbar)
            ClipRect(
              child: Align(
                alignment: Alignment.topLeft,
                heightFactor: toolbarFactor,
                child: SizedBox(
                  height: kToolbarHeight,
                  child: _CompactToolbar(
                    section: widget.section,
                    pageTitle: widget.pageTitle,
                    onHome: widget.onHome,
                    onBack: widget.onBack,
                    onOps: widget.onOps,
                    onSwitchWorkspace: widget.onSwitchWorkspace,
                    // The title bar above already carries these where there is
                    // one; repeating them would put two theme toggles one row
                    // apart.
                    actions: hasTitleBar ? const [] : widget.titleBarActions,
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }

  Widget _panel(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Padding(
      // Home has no rail, so it needs a left gap too or the panel would sit
      // flush against the window edge. The gap follows the rail out.
      padding: EdgeInsets.only(
        left: 12 * (1 - _railAnimation.value),
        right: 12,
        bottom: 12,
      ),
      child: DecoratedBox(
        decoration: BoxDecoration(
          // A hair lighter than the shell in dark mode, a hair darker in light
          // mode: enough to read as a panel without a border.
          color: theme.brightness == Brightness.dark
              ? scheme.surfaceContainerLow
              : scheme.surfaceContainerLowest,
          borderRadius: BorderRadius.circular(12),
        ),
        child: ClipRRect(
          borderRadius: BorderRadius.circular(12),
          child: AnimatedSwitcher(
            duration: const Duration(milliseconds: 220),
            switchInCurve: Curves.easeOutCubic,
            switchOutCurve: Curves.easeInCubic,
            transitionBuilder: (child, animation) => FadeTransition(
              opacity: animation,
              child: SlideTransition(
                position: Tween<Offset>(
                  begin: const Offset(0, 0.015),
                  end: Offset.zero,
                ).animate(animation),
                child: child,
              ),
            ),
            // Keyed by zone, not by pane. A pane change stays inside the same
            // view, whose State holds the form's controllers — remounting it
            // to animate would empty a half-typed form. The views cross-fade
            // their own panes instead.
            child: KeyedSubtree(
              key: ValueKey('${widget.section.name}:${widget.opsTab.name}'),
              child: widget.child,
            ),
          ),
        ),
      ),
    );
  }
}

/// The controls the rail's top slot carries, for the layout that has no rail.
///
/// It carries the same ones, and no more. Drawing a control here that the wide
/// layout does not have makes the two layouts disagree about what the zone
/// offers, which is what a back arrow beside the workspace switcher did.
class _CompactToolbar extends StatelessWidget {
  const _CompactToolbar({
    required this.section,
    required this.pageTitle,
    required this.onHome,
    required this.onBack,
    required this.onOps,
    required this.onSwitchWorkspace,
    required this.actions,
  });

  final AppSection section;
  final String? pageTitle;
  final VoidCallback onHome;
  final VoidCallback onBack;
  final VoidCallback onOps;
  final ValueChanged<AppSection> onSwitchWorkspace;
  final List<Widget> actions;

  /// Mirrors the rail's top slot.
  ///
  /// A workspace swaps to its peer rather than backing out, and that swap is
  /// the title. A back arrow beside it would point where the switcher already
  /// goes, and the wide layout has no such control.
  Widget? _leading(BuildContext context) {
    if (!section.isWorkspace) {
      return IconButton(
        icon: const Icon(Icons.arrow_back),
        tooltip: context.l10n.back,
        onPressed: onBack,
      );
    }
    // Without a title bar there is no app mark, and so nothing anywhere that
    // goes home. The mark moves here rather than a second Back being invented
    // for it: home is a jump to the top, not a step backwards.
    if (!AppWindowTitleBar.isSupported) {
      return IconButton(
        icon: const Icon(Icons.hub_rounded),
        tooltip: context.l10n.home,
        onPressed: onHome,
      );
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    return AppBar(
      backgroundColor: Colors.transparent,
      surfaceTintColor: Colors.transparent,
      elevation: 0,
      scrolledUnderElevation: 0,
      automaticallyImplyLeading: false,
      leading: _leading(context),
      // No rail to hang the workspace swap on, so it lives on the title — the
      // same one-click swap, in the only slot this layout has for it.
      title: section.isWorkspace
          ? _CompactWorkspaceTitle(
              section: section,
              onSelected: onSwitchWorkspace,
            )
          : Text(pageTitle ?? context.l10n.appTitle),
      actions: [
        // Ops is a zone, not one of this zone's destinations, so it stays out
        // of the bottom bar the way it stays out of the rail's list. It had no
        // compact home at all before, which left it unreachable from a
        // workspace once the window was narrow enough to lose the rail.
        if (section != AppSection.ops)
          IconButton(
            icon: const Icon(Icons.tune_rounded),
            tooltip: context.l10n.navOps,
            onPressed: onOps,
          ),
        ...actions,
      ],
    );
  }
}

/// The toolbar title in a workspace: names it, and swaps to the other one.
class _CompactWorkspaceTitle extends StatelessWidget {
  const _CompactWorkspaceTitle({
    required this.section,
    required this.onSelected,
  });

  final AppSection section;
  final ValueChanged<AppSection> onSelected;

  static String _labelFor(BuildContext context, AppSection section) =>
      section == AppSection.register
      ? context.l10n.pageRegister
      : context.l10n.pageConnect;

  @override
  Widget build(BuildContext context) {
    return PopupMenuButton<AppSection>(
      initialValue: section,
      onSelected: onSelected,
      tooltip: context.l10n.switchWorkspace,
      position: PopupMenuPosition.under,
      itemBuilder: (context) => [
        for (final option in [AppSection.register, AppSection.connect])
          PopupMenuItem<AppSection>(
            value: option,
            child: Row(
              children: [
                Icon(
                  option == AppSection.register
                      ? Icons.upload_rounded
                      : Icons.download_rounded,
                  size: 18,
                ),
                const SizedBox(width: 10),
                Text(_labelFor(context, option)),
              ],
            ),
          ),
      ],
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Flexible(
            child: Text(
              _labelFor(context, section),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          const SizedBox(width: 4),
          const Icon(Icons.expand_more_rounded, size: 20),
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
    required this.onBack,
    required this.onSwitchWorkspace,
    required this.onOps,
    required this.onOpsTab,
    required this.pane,
    required this.onPane,
    required this.itemCount,
  });

  final bool expanded;
  final AppSection section;
  final OpsTab opsTab;
  final VoidCallback onBack;
  final ValueChanged<AppSection> onSwitchWorkspace;
  final VoidCallback onOps;
  final ValueChanged<OpsTab> onOpsTab;
  final WorkspacePane pane;
  final ValueChanged<WorkspacePane> onPane;
  final int itemCount;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final inOps = section == AppSection.ops;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: EdgeInsets.fromLTRB(expanded ? 12 : 8, 4, 8, 10),
          // Registering and connecting are peers — two ends of the same tunnel
          // — so the slot swaps between them rather than throwing the user out
          // to home to pick again. Ops is a detour, so it gets a way back to
          // wherever the detour started.
          child: section.isWorkspace
              ? _WorkspaceSwitcher(
                  expanded: expanded,
                  section: section,
                  onSelected: onSwitchWorkspace,
                )
              : _BackButton(expanded: expanded, onPressed: onBack),
        ),
        Padding(
          padding: EdgeInsets.symmetric(horizontal: expanded ? 12 : 8),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              // The zone's destinations, described once in destinationsFor and
              // drawn here as rows and by AppBottomNav as a bar. Spelling them
              // out in both places is what let the bottom bar drift to filled
              // icons and lose the count off the list entry.
              for (final destination in destinationsFor(
                context,
                section: section,
                opsTab: opsTab,
                pane: pane,
                itemCount: itemCount,
                onPane: onPane,
                onOpsTab: onOpsTab,
              ))
                _NavItem(destination: destination, expanded: expanded),
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
              expanded: expanded,
              // Not one of the zone's destinations: it leaves for another zone
              // rather than switching what this one shows, so it is never the
              // selected entry.
              destination: AppDestination(
                icon: Icons.tune_rounded,
                selectedIcon: Icons.tune_rounded,
                label: l10n.navOps,
                selected: false,
                onPressed: onOps,
              ),
            ),
          ),
      ],
    );
  }
}

/// Returns to whatever the user was on before this zone.
///
/// It used to say "Home" and always went there, which was a lie in the common
/// case: ops is reached from a workspace far more often than from home, and
/// landing on home meant picking the role again. Home is now the app mark.
class _BackButton extends StatelessWidget {
  const _BackButton({required this.expanded, required this.onPressed});

  final bool expanded;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Tooltip(
      message: expanded ? '' : context.l10n.back,
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
                    context.l10n.back,
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

/// Names the workspace you are in, and swaps to the other one.
///
/// The two workspaces are the same shape of job on opposite ends of a tunnel,
/// and a machine that publishes a service often subscribes to another. Making
/// the swap a menu here keeps it to one click from anywhere inside either
/// workspace, without adding a second role's entries to the sidebar.
class _WorkspaceSwitcher extends StatelessWidget {
  const _WorkspaceSwitcher({
    required this.expanded,
    required this.section,
    required this.onSelected,
  });

  final bool expanded;
  final AppSection section;
  final ValueChanged<AppSection> onSelected;

  static IconData _iconFor(AppSection section) => section == AppSection.register
      ? Icons.upload_rounded
      : Icons.download_rounded;

  static String _labelFor(BuildContext context, AppSection section) =>
      section == AppSection.register
      ? context.l10n.pageRegister
      : context.l10n.pageConnect;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final label = _labelFor(context, section);

    return PopupMenuButton<AppSection>(
      initialValue: section,
      onSelected: onSelected,
      tooltip: expanded ? context.l10n.switchWorkspace : label,
      position: PopupMenuPosition.under,
      offset: const Offset(0, 4),
      itemBuilder: (context) => [
        for (final option in [AppSection.register, AppSection.connect])
          PopupMenuItem<AppSection>(
            value: option,
            child: Row(
              children: [
                Icon(
                  _iconFor(option),
                  size: 18,
                  color: option == section
                      ? scheme.primary
                      : scheme.onSurfaceVariant,
                ),
                const SizedBox(width: 10),
                Text(
                  _labelFor(context, option),
                  style: theme.textTheme.bodyMedium?.copyWith(
                    fontWeight: option == section
                        ? FontWeight.w600
                        : FontWeight.w500,
                    color: option == section ? scheme.primary : null,
                  ),
                ),
              ],
            ),
          ),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 9),
        child: Row(
          mainAxisAlignment: expanded
              ? MainAxisAlignment.start
              : MainAxisAlignment.center,
          children: [
            Icon(_iconFor(section), size: 18, color: scheme.onSurfaceVariant),
            if (expanded) ...[
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodyMedium?.copyWith(
                    color: scheme.onSurface,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
            ],
            const SizedBox(width: 4),
            Icon(
              Icons.expand_more_rounded,
              size: 16,
              color: scheme.onSurfaceVariant,
            ),
          ],
        ),
      ),
    );
  }
}

class _NavItem extends StatelessWidget {
  const _NavItem({required this.destination, required this.expanded});

  final AppDestination destination;
  final bool expanded;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final selected = destination.selected;
    final label = destination.fullLabel;
    final icon = destination.icon;
    final selectedIcon = destination.selectedIcon;
    final onPressed = destination.onPressed;
    final foreground = selected
        ? scheme.onSecondaryContainer
        : scheme.onSurfaceVariant;

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
      // Animated, so the highlight grows into the row the way the bottom bar's
      // pill slides between destinations rather than blinking on.
      child: TweenAnimationBuilder<double>(
        tween: Tween(begin: selected ? 1 : 0, end: selected ? 1 : 0),
        duration: const Duration(milliseconds: 180),
        curve: Curves.easeOutCubic,
        builder: (context, t, child) => Material(
          color: Color.lerp(Colors.transparent, scheme.secondaryContainer, t),
          borderRadius: BorderRadius.circular(10),
          child: child,
        ),
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
    if (ResponsiveLayout.usesBottomNav(context)) {
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
