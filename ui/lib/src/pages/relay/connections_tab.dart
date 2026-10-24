import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/pages/relay/relay_format.dart';
import 'package:pb_mapper_ui/src/pages/relay/services_tab.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// What the relay is carrying: the control connections it holds (an
/// administrator's view), the direct sessions it has heard about, and its
/// routing table as it reports it.
class ConnectionsTab extends StatelessWidget {
  const ConnectionsTab({super.key});

  @override
  Widget build(BuildContext context) => RelayListener(builder: _build);

  Widget _build(BuildContext context) {
    final l10n = context.l10n;
    final scope = AppScope.of(context);
    final store = scope.store;
    final relay = scope.relay;
    if (store.relayState != RelayState.online) {
      return EmptyNote(icon: AppIcons.offline, text: l10n.sectionRelayOffline);
    }
    final held = [...relay.connections]
      ..sort((a, b) {
        final byService = a.service.compareTo(b.service);
        return byService != 0 ? byService : a.connId.compareTo(b.connId);
      });
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (relay.isAdmin) ...[
          SectionHeader(title: l10n.relayHeld, count: held.length),
          if (held.isEmpty)
            EmptyNote(icon: AppIcons.link, text: l10n.relayHeldEmpty)
          else
            ListCard(
              padding: const EdgeInsets.fromLTRB(14, 8, 4, 8),
              child: Column(
                children: [
                  for (final c in held)
                    RelayConnectionRow(
                      connection: c,
                      relay: relay,
                      showService: true,
                    ),
                ],
              ),
            ),
        ],
        SectionHeader(title: l10n.relaySessions, count: relay.sessions.length),
        if (relay.sessionsError != null)
          EmptyNote(icon: AppIcons.warning, text: relay.sessionsError!)
        else if (relay.sessions.isEmpty)
          EmptyNote(icon: AppIcons.link, text: l10n.relaySessionsEmpty)
        else
          ListCard(
            padding: const EdgeInsets.fromLTRB(14, 8, 14, 8),
            child: Column(
              children: [
                for (final s in relay.sessions) _SessionRow(session: s),
              ],
            ),
          ),
        SectionHeader(title: l10n.relayRouting),
        _RoutingTable(status: store.relay),
      ],
    );
  }
}

class _SessionRow extends StatelessWidget {
  const _SessionRow({required this.session});

  final DirectSession session;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final s = session;
    final closed = s.state == 'closed';
    final facts = [
      s.transport,
      if (s.ageMs != null)
        l10n.relayHeard(formatAgo(context, Duration(milliseconds: s.ageMs!))),
      if (s.reason case final reason? when reason.isNotEmpty) reason,
      '#${s.id}',
    ].join('  ·  ');
    return Padding(
      key: Key('relay-session-${s.id}'),
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  s.service,
                  style: mono(
                    fontSize: 13,
                    fontWeight: FontWeight.w600,
                    color: scheme.onSurface,
                  ),
                ),
                Text(
                  facts,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.onSurfaceVariant,
                    fontSize: 12,
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(width: 8),
          Wrap(
            spacing: 6,
            runSpacing: 4,
            alignment: WrapAlignment.end,
            children: [
              if (s.stale)
                StatusChip(
                  color: cautionColor(scheme),
                  label: l10n.relaySessionStale,
                ),
              if (s.conflicting)
                StatusChip(
                  color: scheme.error,
                  label: l10n.relaySessionConflicting,
                ),
              if (closed)
                StatusChip(
                  color: availableColor(scheme),
                  label: l10n.relaySessionClosed,
                )
              else
                StatusChip(
                  color: s.isDirect
                      ? successColor(scheme)
                      : signalColor(scheme),
                  label: s.isDirect
                      ? l10n.relayPathDirect
                      : l10n.relayPathRelay,
                ),
            ],
          ),
        ],
      ),
    );
  }
}

/// The relay's routing table, raw: what a bug report wants, folded away.
class _RoutingTable extends StatefulWidget {
  const _RoutingTable({required this.status});

  final ServerStatusDetail? status;

  @override
  State<_RoutingTable> createState() => _RoutingTableState();
}

class _RoutingTableState extends State<_RoutingTable> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final status = widget.status;
    final lines = [
      if (status != null && status.serverMap.isNotEmpty)
        'map    ${status.serverMap}',
      if (status != null && status.activeConnections.isNotEmpty)
        'active ${status.activeConnections}',
      if (status != null && status.idleConnections.isNotEmpty)
        'idle   ${status.idleConnections}',
    ];
    return ListCard(
      padding: const EdgeInsets.fromLTRB(14, 6, 14, 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Align(
            alignment: AlignmentDirectional.centerStart,
            child: TextButton.icon(
              key: const Key('relay-routing'),
              onPressed: () => setState(() => _open = !_open),
              style: TextButton.styleFrom(
                padding: EdgeInsets.zero,
                foregroundColor: scheme.onSurfaceVariant,
                textStyle: theme.textTheme.bodySmall?.copyWith(
                  fontWeight: FontWeight.w500,
                ),
              ),
              icon: Icon(
                _open ? AppIcons.chevronDown : AppIcons.chevronRight,
                size: 15,
              ),
              label: Text(l10n.relayRoutingHint),
            ),
          ),
          if (_open)
            Container(
              padding: const EdgeInsets.all(12),
              decoration: BoxDecoration(
                color: codeSurface(scheme),
                borderRadius: BorderRadius.circular(10),
              ),
              child: SelectableText(
                lines.isEmpty ? l10n.relayRoutingEmpty : lines.join('\n'),
                style: mono(fontSize: 12, color: scheme.onSurface),
              ),
            ),
        ],
      ),
    );
  }
}
