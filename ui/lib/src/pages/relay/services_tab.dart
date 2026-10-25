import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_glyph.dart';
import 'package:pb_mapper_ui/src/pages/relay/relay_format.dart';
import 'package:pb_mapper_ui/src/pages/tunnel_actions.dart';
import 'package:pb_mapper_ui/src/pages/tunnel_editor.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/widgets/connections.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// Every service on the relay, and what this computer is to each: sharing
/// it, connected to it, or free to connect. An administrator sees every
/// namespace, who registered what, and the connections behind each, and can
/// retire them.
class ServicesTab extends StatefulWidget {
  const ServicesTab({super.key});

  @override
  State<ServicesTab> createState() => _ServicesTabState();
}

class _ServicesTabState extends State<ServicesTab> {
  final TextEditingController _search = TextEditingController();
  String _query = '';

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => RelayListener(builder: _build);

  Widget _build(BuildContext context) {
    final l10n = context.l10n;
    final scope = AppScope.of(context);
    final store = scope.store;
    final relay = scope.relay;
    switch (store.relayState) {
      case RelayState.unset:
        return EmptyNote(
          icon: AppIcons.relay,
          text: l10n.sectionRelayUnset,
          action: TextButton(
            onPressed: () => scope.go(AppPage.settings),
            child: Text(l10n.relaySettings),
          ),
        );
      case RelayState.offline:
        return EmptyNote(
          icon: AppIcons.offline,
          text: l10n.sectionRelayOffline,
        );
      case RelayState.checking:
        return EmptyNote(icon: AppIcons.relay, text: l10n.relayChecking);
      case RelayState.online:
        break;
    }
    final rows = relay.serviceRows;
    final query = _query.trim().toLowerCase();
    final shown = [
      for (final row in rows)
        if (query.isEmpty || row.name.toLowerCase().contains(query)) row,
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SectionHeader(title: l10n.relayTabServices, count: rows.length),
        if (rows.length > 6) ...[
          TextField(
            key: const Key('relay-search'),
            controller: _search,
            onChanged: (value) => setState(() => _query = value),
            decoration: InputDecoration(
              hintText: l10n.relaySearch,
              prefixIcon: const Icon(AppIcons.search, size: 18),
              isDense: true,
            ),
          ),
          const SizedBox(height: 12),
        ],
        if (rows.isEmpty)
          EmptyNote(icon: AppIcons.service, text: l10n.relayServicesEmpty)
        else if (shown.isEmpty)
          EmptyNote(icon: AppIcons.search, text: l10n.relayNoMatch(_query))
        else
          for (final row in shown)
            _ServiceRow(
              key: Key('relay-row-${row.id}'),
              row: row,
              store: store,
              relay: relay,
            ),
      ],
    );
  }
}

class _ServiceRow extends StatefulWidget {
  const _ServiceRow({
    super.key,
    required this.row,
    required this.store,
    required this.relay,
  });

  final RelayServiceRow row;
  final AppStore store;
  final RelayStore relay;

  @override
  State<_ServiceRow> createState() => _ServiceRowState();
}

class _ServiceRowState extends State<_ServiceRow> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final row = widget.row;
    final name = row.name;
    final store = widget.store;
    final relay = widget.relay;
    // This computer shares only into its own namespace; it connects into any
    // the administrator key reaches, so a connection is matched by both.
    final local = row.namespace == relay.ownNamespace;
    final remote = RemoteService(
      name: name,
      namespace: row.namespace,
      owner: row.info?.keyId,
    );
    final mine = local ? store.find(TunnelKind.share, name) : null;
    final connection = store.connectionTo(remote);
    final info = row.info;
    final held = relay.connectionsOf(row);

    final Color color;
    if (mine != null) {
      color = healthColor(scheme, mine.health);
    } else if (connection?.health == TunnelHealth.running) {
      color = successColor(scheme);
    } else {
      color = availableColor(scheme);
    }

    final facts = <String>[
      if (mine != null) l10n.relayServiceMine,
      if (info != null) ...[
        info.transport,
        if (info.encrypted) l10n.encrypted,
        l10n.relayServiceConnections(info.connections),
        // Another credential's service has its owner on a tag of its own.
        if (local) ownerName(context, relay, info.keyId),
      ],
    ];

    final Widget trailing;
    if (mine != null) {
      trailing = StatusChip(color: color, label: l10n.relayThisComputer);
    } else if (connection != null) {
      trailing = StatusChip(
        color: color,
        label: l10n.openAt(connection.localAddress),
        shrinks: true,
      );
    } else {
      trailing = FilledButton.tonal(
        key: Key('connect-${row.id}'),
        onPressed: () => openTunnelEditor(
          context,
          kind: TunnelKind.connect,
          key: name,
          namespace: store.connectNamespace(remote),
        ),
        style: rowButtonStyle(context),
        child: Text(l10n.connect),
      );
    }

    return ListCard(
      padding: const EdgeInsets.fromLTRB(14, 10, 6, 10),
      child: Column(
        key: Key('relay-service-${row.id}'),
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              PlanetGlyph(color: color, size: 30),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SelectableText(
                      name,
                      maxLines: 1,
                      style: mono(
                        fontSize: 13.5,
                        fontWeight: FontWeight.w600,
                        color: scheme.onSurface,
                      ),
                    ),
                    if (!local) ...[
                      const SizedBox(height: 4),
                      // Whose it is, at a glance: the administrator's own
                      // services carry no tag.
                      StatusChip(
                        key: Key('relay-owner-${row.id}'),
                        color: signalColor(scheme),
                        label: ownerName(context, relay, remote.owner),
                        shrinks: true,
                      ),
                    ],
                    if (facts.isNotEmpty) ...[
                      const SizedBox(height: 2),
                      Text(
                        facts.join('  ·  '),
                        maxLines: 2,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    ],
                  ],
                ),
              ),
              const SizedBox(width: 8),
              Flexible(flex: 0, child: trailing),
              _ServiceMenu(
                row: row,
                relay: relay,
                canRetire: relay.isAdmin && (info?.connections ?? 0) > 0,
              ),
            ],
          ),
          // An administrator has the relay's own inventory to hand; anyone
          // else asks the relay about this one service when they open it.
          if (relay.isAdmin && relay.adminLoaded)
            Padding(
              padding: const EdgeInsets.only(left: 42, top: 2),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  if (held.isNotEmpty)
                    TextButton.icon(
                      key: Key('relay-service-open-${row.id}'),
                      onPressed: () => setState(() => _open = !_open),
                      style: TextButton.styleFrom(
                        padding: EdgeInsets.zero,
                        minimumSize: const Size(0, 30),
                        foregroundColor: scheme.onSurfaceVariant,
                        textStyle: Theme.of(context).textTheme.bodySmall
                            ?.copyWith(fontWeight: FontWeight.w500),
                      ),
                      icon: Icon(
                        _open ? AppIcons.chevronDown : AppIcons.chevronRight,
                        size: 15,
                      ),
                      label: Text(l10n.connectionsHeld(held.length)),
                    ),
                  if (_open)
                    for (final c in held)
                      RelayConnectionRow(connection: c, relay: relay),
                ],
              ),
            )
          else if (mine == null)
            Padding(
              padding: const EdgeInsets.only(left: 42),
              child: ConnectionsDisclosure(api: store.api, serviceKey: name),
            ),
        ],
      ),
    );
  }
}

enum _ServiceAction { copyKey, retire }

class _ServiceMenu extends StatelessWidget {
  const _ServiceMenu({
    required this.row,
    required this.relay,
    required this.canRetire,
  });

  final RelayServiceRow row;
  final RelayStore relay;
  final bool canRetire;

  Future<void> _retire(BuildContext context) async {
    final l10n = context.l10n;
    final yes = await confirmDestructive(
      context,
      title: l10n.relayRetireAllTitle(row.name),
      body: l10n.relayRetireAllBody,
      action: l10n.relayRetire,
    );
    if (!yes || !context.mounted) return;
    final retired = await attempt(
      context,
      () =>
          relay.retireConnections(namespace: row.namespace, service: row.name),
    );
    if (retired != null && context.mounted) {
      showToast(context, l10n.relayRetired(retired), kind: ToastKind.success);
    }
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    return PopupMenuButton<_ServiceAction>(
      key: Key('relay-service-menu-${row.id}'),
      tooltip: l10n.more,
      icon: const Icon(AppIcons.more, size: 18),
      onSelected: (action) => switch (action) {
        _ServiceAction.copyKey => copyText(context, row.name),
        _ServiceAction.retire => _retire(context),
      },
      itemBuilder: (context) => [
        PopupMenuItem(
          value: _ServiceAction.copyKey,
          child: Row(
            children: [
              const Icon(AppIcons.copy, size: 17),
              const SizedBox(width: 10),
              Text(l10n.copyKey),
            ],
          ),
        ),
        if (canRetire)
          PopupMenuItem(
            key: Key('relay-retire-all-${row.id}'),
            value: _ServiceAction.retire,
            child: Row(
              children: [
                Icon(AppIcons.unlink, size: 17, color: scheme.error),
                const SizedBox(width: 10),
                Text(
                  l10n.relayRetireAll,
                  style: TextStyle(color: scheme.error),
                ),
              ],
            ),
          ),
      ],
    );
  }
}

/// One connection the relay holds: its health, its number, when the relay
/// last heard from it, and, for an administrator, a way to retire it.
class RelayConnectionRow extends StatelessWidget {
  const RelayConnectionRow({
    super.key,
    required this.connection,
    required this.relay,
    this.showService = false,
  });

  final RelayConnection connection;
  final RelayStore relay;

  /// Name the service too, where the row is not already under it.
  final bool showService;

  Future<void> _retire(BuildContext context) async {
    final l10n = context.l10n;
    final yes = await confirmDestructive(
      context,
      title: l10n.relayRetireOneTitle(connection.connId),
      body: l10n.relayRetireOneBody,
      action: l10n.relayRetire,
    );
    if (!yes || !context.mounted) return;
    final retired = await attempt(
      context,
      () => relay.retireConnections(
        namespace: connection.namespace,
        service: connection.service,
        connId: connection.connId,
      ),
    );
    if (retired != null && context.mounted) {
      showToast(context, l10n.relayRetired(retired), kind: ToastKind.success);
    }
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final c = connection;
    final id = relayServiceId(c.namespace, c.service);
    final facts = [
      c.transport,
      if (c.encrypted) l10n.encrypted,
      'v${c.protocolVersion}',
      l10n.relayGeneration(c.generation),
      l10n.relayHeard(
        formatAgo(context, Duration(milliseconds: c.lastRxAgeMs)),
      ),
      if (showService) ownerName(context, relay, c.keyId),
    ].join('  ·  ');
    return Padding(
      key: Key('relay-conn-$id-${c.connId}'),
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Row(
        children: [
          StatusDot(
            color: c.healthy ? successColor(scheme) : cautionColor(scheme),
          ),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  showService ? '${c.service}  #${c.connId}' : '#${c.connId}',
                  style: mono(
                    fontSize: 12.5,
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
          if (relay.isAdmin)
            IconButton(
              key: Key('relay-retire-$id-${c.connId}'),
              tooltip: l10n.relayRetire,
              onPressed: () => _retire(context),
              icon: Icon(AppIcons.unlink, size: 16, color: scheme.error),
            ),
        ],
      ),
    );
  }
}
