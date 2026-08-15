import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_section.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/workspace_pane.dart';

/// One place the user can go inside the zone they are in.
///
/// The side rail and the bottom bar are two drawings of this one list. They
/// used to each spell their destinations out, which is how the bottom bar ended
/// up with filled icons where the rail used outlined ones, and with no count on
/// the entry whose whole job is to say how many things are there.
@immutable
class AppDestination {
  const AppDestination({
    required this.icon,
    required this.selectedIcon,
    required this.label,
    required this.selected,
    required this.onPressed,
    this.count,
  });

  final IconData icon;

  /// Drawn instead of [icon] while selected. Filled against outlined is the
  /// selection cue that survives being the only one on a small screen.
  final IconData selectedIcon;

  final String label;
  final bool selected;
  final VoidCallback onPressed;

  /// How many items the destination holds, where that is worth saying.
  final int? count;

  /// The label as both navigations render it. An empty list says nothing
  /// rather than "(0)", which reads as a broken counter.
  String get fullLabel => (count ?? 0) > 0 ? '$label ($count)' : label;
}

/// The destinations of the current zone, or none for the zones without any.
///
/// Home and setup deliberately return an empty list: one is a choice and the
/// other a guided flow, and neither has anywhere to navigate to.
List<AppDestination> destinationsFor(
  BuildContext context, {
  required AppSection section,
  required OpsTab opsTab,
  required WorkspacePane pane,
  required int itemCount,
  required ValueChanged<WorkspacePane> onPane,
  required ValueChanged<OpsTab> onOpsTab,
}) {
  final l10n = context.l10n;

  if (section.isWorkspace) {
    final register = section == AppSection.register;
    return [
      AppDestination(
        icon: Icons.add_rounded,
        selectedIcon: Icons.add_rounded,
        label: register ? l10n.navNewRegister : l10n.navNewConnect,
        selected: pane == WorkspacePane.form,
        onPressed: () => onPane(WorkspacePane.form),
      ),
      AppDestination(
        icon: register ? Icons.dns_outlined : Icons.cable_outlined,
        selectedIcon: register ? Icons.dns : Icons.cable,
        label: register ? l10n.navRegisteredList : l10n.navConnectionList,
        count: itemCount,
        selected: pane == WorkspacePane.list,
        onPressed: () => onPane(WorkspacePane.list),
      ),
      AppDestination(
        icon: Icons.terminal_outlined,
        selectedIcon: Icons.terminal,
        label: l10n.navLogs,
        selected: pane == WorkspacePane.logs,
        onPressed: () => onPane(WorkspacePane.logs),
      ),
    ];
  }

  if (section == AppSection.ops) {
    return [
      AppDestination(
        icon: Icons.monitor_outlined,
        selectedIcon: Icons.monitor,
        label: l10n.navStatus,
        selected: opsTab == OpsTab.status,
        onPressed: () => onOpsTab(OpsTab.status),
      ),
      AppDestination(
        icon: Icons.dns_outlined,
        selectedIcon: Icons.dns,
        label: l10n.navServices,
        selected: opsTab == OpsTab.services,
        onPressed: () => onOpsTab(OpsTab.services),
      ),
      AppDestination(
        icon: Icons.settings_outlined,
        selectedIcon: Icons.settings,
        label: l10n.navConfig,
        selected: opsTab == OpsTab.config,
        onPressed: () => onOpsTab(OpsTab.config),
      ),
    ];
  }

  return const [];
}
