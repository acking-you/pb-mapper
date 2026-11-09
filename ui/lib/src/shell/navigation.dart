import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/sharing_indicator.dart';
import 'package:pb_mapper_ui/src/shell/window_chrome.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';
import 'package:pb_mapper_ui/src/widgets/brand_mark.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';

/// The rail's width on a wide window.
const double kRailWidth = 212;

const double _rowHeight = 38;
const double _rowGap = 2;

/// The six pages as a rail on the backdrop: the selection is a pane of glass
/// that glides, with a little spring, to the chosen row. At its foot, the one
/// fact every page depends on — whether the relay answers.
class NavRail extends StatelessWidget {
  const NavRail({super.key, required this.page, required this.onSelect});

  final AppPage page;
  final ValueChanged<AppPage> onSelect;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final step = MediaQuery.textScalerOf(context).scale(_rowHeight);
    final index = AppPage.railEntries.indexOf(page);
    return Material(
      type: MaterialType.transparency,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // The window's corner: the traffic lights live here, and it drags.
          SizedBox(
            height: isFramelessDesktop ? 46 : 18,
            child: const WindowDragArea(),
          ),
          WindowDragArea(
            child: Padding(
              padding: const EdgeInsets.fromLTRB(20, 0, 12, 18),
              child: Row(
                children: [
                  const BrandMark(size: 26),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Text(
                      context.l10n.appTitle,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 15,
                        fontWeight: FontWeight.w600,
                        letterSpacing: -0.1,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 10),
            child: Stack(
              children: [
                AnimatedPositioned(
                  duration: Motion.of(
                    context,
                    const Duration(milliseconds: 520),
                  ),
                  curve: Motion.spring,
                  top: index * (step + _rowGap),
                  left: 0,
                  right: 0,
                  height: step,
                  child: const IgnorePointer(
                    child: Glass(
                      depth: GlassDepth.chrome,
                      borderRadius: BorderRadius.all(Radius.circular(999)),
                      child: SizedBox.expand(),
                    ),
                  ),
                ),
                Column(
                  children: [
                    for (final entry in AppPage.railEntries) ...[
                      SizedBox(
                        height: step,
                        child: _RailRow(
                          page: entry,
                          selected: entry == page,
                          onTap: () => onSelect(entry),
                          hover: backdropHover(scheme),
                          badge: entry == AppPage.remote
                              ? const HostSharingDot()
                              : null,
                        ),
                      ),
                      const SizedBox(height: _rowGap),
                    ],
                  ],
                ),
              ],
            ),
          ),
          const Expanded(child: WindowDragArea()),
          Padding(
            padding: const EdgeInsets.fromLTRB(10, 0, 10, 12),
            child: _RelayFoot(onTap: () => onSelect(AppPage.relay)),
          ),
        ],
      ),
    );
  }
}

class _RailRow extends StatelessWidget {
  const _RailRow({
    required this.page,
    required this.selected,
    required this.onTap,
    required this.hover,
    this.badge,
  });

  final AppPage page;
  final bool selected;
  final VoidCallback onTap;
  final Color hover;

  /// After the label: Remote desktop's sharing dot.
  final Widget? badge;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final ink = selected ? scheme.onSurface : scheme.onSurfaceVariant;
    final badge = this.badge;
    return Semantics(
      selected: selected,
      button: true,
      child: InkWell(
        key: Key('nav-${page.name}'),
        mouseCursor: clickable,
        hoverColor: hover,
        borderRadius: BorderRadius.circular(999),
        onTap: selected ? null : onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 14),
          child: Row(
            children: [
              Icon(page.icon, size: AppIconSize.nav, color: ink),
              const SizedBox(width: 11),
              Expanded(
                child: Text(
                  page.label(context),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(
                    fontSize: 13.5,
                    fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                    color: ink,
                  ),
                ),
              ),
              if (badge != null) ...[const SizedBox(width: 8), badge],
            ],
          ),
        ),
      ),
    );
  }
}

/// Whether this computer is shared, and watched: a dot while sharing, a
/// breathing one while a viewer is connected and the picture flows.
class HostSharingDot extends StatelessWidget {
  const HostSharingDot({super.key});

  @override
  Widget build(BuildContext context) {
    final desktop = AppScope.of(context).store.desktop;
    final color = signalColor(Theme.of(context).colorScheme);
    return ValueListenableBuilder(
      valueListenable: desktop.hostView,
      builder: (context, host, _) {
        if (!host.sharing) return const SizedBox.shrink();
        final viewer = host.viewer;
        return Semantics(
          value: viewer == null
              ? context.l10n.hostSharing
              : sharingLabel(context, viewer),
          child: viewer == null || viewer.paused
              ? StatusDot(color: color, size: 7)
              : PulsingDot(color: color, size: 7),
        );
      },
    );
  }
}

/// The relay's state, address and answer time at the foot of the rail;
/// opens Relay.
class _RelayFoot extends StatelessWidget {
  const _RelayFoot({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final store = AppScope.of(context).store;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return ListenableBuilder(
      listenable: store,
      builder: (context, _) {
        final state = store.relayState;
        final latency = state == RelayState.online
            ? store.relay?.latencyMs
            : null;
        return Glass(
          depth: GlassDepth.chrome,
          borderRadius: BorderRadius.circular(kRowRadius),
          child: InkWell(
            key: const Key('rail-relay-status'),
            mouseCursor: clickable,
            borderRadius: BorderRadius.circular(kRowRadius),
            onTap: onTap,
            child: Padding(
              padding: const EdgeInsets.fromLTRB(12, 10, 12, 10),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  RelayChip(state: state, filled: false, shrinks: true),
                  if (store.hasRelay) ...[
                    const SizedBox(height: 3),
                    Padding(
                      padding: const EdgeInsets.only(left: 14),
                      child: Text(
                        [
                          store.relayAddress,
                          if (latency != null)
                            context.l10n.relayLatency(latency),
                        ].join(' · '),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: onSurfaceMuted(scheme),
                          fontSize: 11.5,
                        ),
                      ),
                    ),
                  ],
                ],
              ),
            ),
          ),
        );
      },
    );
  }
}

/// The pages along the bottom of a compact window: five, because Logs lives
/// in Settings there.
class NavBar extends StatelessWidget {
  const NavBar({super.key, required this.page, required this.onSelect});

  final AppPage page;
  final ValueChanged<AppPage> onSelect;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: scheme.surfaceContainerLow,
      child: DecoratedBox(
        decoration: BoxDecoration(
          border: Border(top: BorderSide(color: scheme.outlineVariant)),
        ),
        child: SafeArea(
          top: false,
          child: Padding(
            padding: const EdgeInsets.fromLTRB(8, 6, 8, 6),
            child: Row(
              children: [
                for (final entry in AppPage.barEntries)
                  Expanded(
                    child: _BarItem(
                      page: entry,
                      selected: entry == page.barEntry,
                      // Settings stays tappable while Logs, inside it, shows.
                      onTap: entry == page ? null : () => onSelect(entry),
                      badge: entry == AppPage.remote
                          ? const HostSharingDot()
                          : null,
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _BarItem extends StatelessWidget {
  const _BarItem({
    required this.page,
    required this.selected,
    required this.onTap,
    this.badge,
  });

  final AppPage page;
  final bool selected;
  final VoidCallback? onTap;

  /// Over the pill's corner: Remote desktop's sharing dot.
  final Widget? badge;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final ink = selected ? scheme.onSurface : scheme.onSurfaceVariant;
    final badge = this.badge;
    return Semantics(
      selected: selected,
      button: true,
      child: InkWell(
        key: Key('nav-${page.name}'),
        mouseCursor: clickable,
        borderRadius: BorderRadius.circular(16),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 4),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              AnimatedContainer(
                duration: Motion.of(context, Motion.medium),
                curve: Motion.move,
                width: 54,
                height: 30,
                decoration: BoxDecoration(
                  color: selected
                      ? softWash(scheme, strength: 1.6)
                      : Colors.transparent,
                  borderRadius: BorderRadius.circular(999),
                ),
                child: Stack(
                  clipBehavior: Clip.none,
                  alignment: Alignment.center,
                  children: [
                    Icon(page.icon, size: AppIconSize.nav, color: ink),
                    if (badge != null)
                      Positioned(top: 3, right: 8, child: badge),
                  ],
                ),
              ),
              const SizedBox(height: 3),
              Text(
                page.label(context, short: true),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(
                  fontSize: 11.5,
                  fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                  color: ink,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
