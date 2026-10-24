import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_glyph.dart';
import 'package:pb_mapper_ui/src/pages/tunnel_actions.dart';
import 'package:pb_mapper_ui/src/pages/tunnel_editor.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/widgets/connections.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// Every tunnel on one page: what this computer shares, what it connects to,
/// and what else the relay offers. The register and connect workspaces, their
/// forms and their lists were five places; this is one.
class TunnelsPage extends StatelessWidget {
  const TunnelsPage({super.key});

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scope = AppScope.of(context);
    final store = scope.store;
    return PageFrame(
      title: l10n.navTunnels,
      actions: [
        IconButton(
          key: const Key('tunnels-refresh'),
          tooltip: l10n.refresh,
          onPressed: () => store.refresh(force: true),
          icon: const Icon(AppIcons.refresh),
        ),
      ],
      primary: PrimaryAction(
        key: const Key('new-tunnel'),
        icon: AppIcons.add,
        label: l10n.newTunnel,
        onPressed: () => openTunnelEditor(context),
      ),
      body: ListenableBuilder(
        listenable: store,
        builder: (context, _) => Align(
          alignment: Alignment.topCenter,
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 880),
            child: ListView(
              key: const Key('tunnels-list'),
              padding: const EdgeInsets.fromLTRB(18, 0, 18, 28),
              children: [
                SectionHeader(
                  title: l10n.sectionShared,
                  count: store.shares.length,
                  action: _AddButton(
                    key: const Key('add-share'),
                    label: l10n.addShare,
                    onPressed: () =>
                        openTunnelEditor(context, kind: TunnelKind.share),
                  ),
                ),
                if (store.shares.isEmpty)
                  EmptyNote(icon: AppIcons.share, text: l10n.sectionSharedEmpty)
                else
                  for (final tunnel in store.shares) TunnelCard(tunnel: tunnel),
                SectionHeader(
                  title: l10n.sectionConnected,
                  count: store.connections.length,
                  action: _AddButton(
                    key: const Key('add-connect'),
                    label: l10n.addConnect,
                    onPressed: () =>
                        openTunnelEditor(context, kind: TunnelKind.connect),
                  ),
                ),
                if (store.connections.isEmpty)
                  EmptyNote(
                    icon: AppIcons.connect,
                    text: l10n.sectionConnectedEmpty,
                  )
                else
                  for (final tunnel in store.connections)
                    TunnelCard(tunnel: tunnel),
                SectionHeader(
                  title: l10n.sectionRelay,
                  count: store.relayState == RelayState.online
                      ? store.remoteServices.length
                      : null,
                ),
                ..._relaySection(context, store),
              ],
            ),
          ),
        ),
      ),
    );
  }

  List<Widget> _relaySection(BuildContext context, AppStore store) {
    final l10n = context.l10n;
    final scope = AppScope.of(context);
    switch (store.relayState) {
      case RelayState.unset:
        return [
          EmptyNote(
            icon: AppIcons.relay,
            text: l10n.sectionRelayUnset,
            action: TextButton(
              onPressed: () => scope.go(AppPage.settings),
              child: Text(l10n.relaySettings),
            ),
          ),
        ];
      case RelayState.offline:
        return [
          EmptyNote(icon: AppIcons.offline, text: l10n.sectionRelayOffline),
        ];
      case RelayState.checking:
        return [EmptyNote(icon: AppIcons.relay, text: l10n.relayChecking)];
      case RelayState.online:
        final remote = store.remoteServices;
        if (remote.isEmpty) {
          return [
            EmptyNote(icon: AppIcons.relay, text: l10n.sectionRelayEmpty),
          ];
        }
        return [
          _Card(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Column(
              children: [
                for (final (i, key) in remote.indexed) ...[
                  if (i > 0) const Divider(indent: 58),
                  _RemoteRow(serviceKey: key),
                ],
              ],
            ),
          ),
        ];
    }
  }
}

class _AddButton extends StatelessWidget {
  const _AddButton({super.key, required this.label, required this.onPressed});

  final String label;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) => TextButton.icon(
    onPressed: onPressed,
    style: TextButton.styleFrom(
      minimumSize: const Size(0, 32),
      padding: const EdgeInsets.symmetric(horizontal: 10),
      foregroundColor: Theme.of(context).colorScheme.onSurfaceVariant,
    ),
    icon: const Icon(AppIcons.add, size: 16),
    label: Text(label, style: const TextStyle(fontSize: 13)),
  );
}

/// A white card on the grouped sheet.
class _Card extends StatelessWidget {
  const _Card({
    super.key,
    required this.child,
    this.padding = const EdgeInsets.all(14),
  });

  final Widget child;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: 10),
    child: RaisedSurface(
      child: Padding(padding: padding, child: child),
    ),
  );
}

/// One tunnel: its sphere in its state's colour, its key and where it points,
/// its state, the one action that matters (start or stop), and the rest in a
/// menu. Two lines on a narrow window, where the actions would crowd the key.
class TunnelCard extends StatelessWidget {
  const TunnelCard({super.key, required this.tunnel});

  final Tunnel tunnel;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final store = AppScope.of(context).store;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final facts = [
      tunnel.localAddress,
      tunnel.protocol,
      if (tunnel.encrypted) l10n.encrypted,
    ].join('  ·  ');
    final identity = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          tunnel.key,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: mono(
            fontSize: 14,
            fontWeight: FontWeight.w600,
            color: scheme.onSurface,
          ),
        ),
        const SizedBox(height: 3),
        Text(
          facts,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: mono(fontSize: 12, color: scheme.onSurfaceVariant),
        ),
      ],
    );
    final actions = Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        HealthChip(health: tunnel.health),
        const SizedBox(width: 10),
        _ToggleButton(tunnel: tunnel, store: store),
        _TunnelMenu(tunnel: tunnel, store: store),
      ],
    );
    final glyph = PlanetGlyph(
      color: healthColor(scheme, tunnel.health),
      size: 34,
    );
    final problem = tunnel.problem;
    return _Card(
      key: Key('tunnel-${tunnel.id}'),
      padding: const EdgeInsets.fromLTRB(14, 12, 6, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          LayoutBuilder(
            builder: (context, box) {
              if (box.maxWidth >= 540) {
                return Row(
                  children: [
                    glyph,
                    const SizedBox(width: 12),
                    Expanded(child: identity),
                    const SizedBox(width: 10),
                    actions,
                  ],
                );
              }
              return Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Row(
                    children: [
                      glyph,
                      const SizedBox(width: 12),
                      Expanded(child: identity),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Align(
                    alignment: AlignmentDirectional.centerEnd,
                    child: actions,
                  ),
                ],
              );
            },
          ),
          if (problem != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(46, 8, 8, 0),
              child: Text(
                problem,
                maxLines: 3,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: tunnel.health == TunnelHealth.failed
                      ? scheme.error
                      : scheme.onSurfaceVariant,
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// Start or stop. Tracks its own request, so a click cannot be repeated while
/// the first is in flight.
class _ToggleButton extends StatefulWidget {
  const _ToggleButton({required this.tunnel, required this.store});

  final Tunnel tunnel;
  final AppStore store;

  @override
  State<_ToggleButton> createState() => _ToggleButtonState();
}

class _ToggleButtonState extends State<_ToggleButton> {
  bool _busy = false;

  Future<void> _toggle() async {
    setState(() => _busy = true);
    try {
      await toggleTunnel(context, widget.store, widget.tunnel);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final live = widget.tunnel.isLive;
    final style = _rowButtonStyle(context);
    final child = _busy
        ? const SizedBox.square(
            dimension: 15,
            child: CircularProgressIndicator(strokeWidth: 2),
          )
        : Text(live ? l10n.stop : l10n.start);
    final key = Key('toggle-${widget.tunnel.id}');
    return live
        ? OutlinedButton(
            key: key,
            onPressed: _busy ? null : _toggle,
            style: style,
            child: child,
          )
        : FilledButton(
            key: key,
            onPressed: _busy ? null : _toggle,
            style: style,
            child: child,
          );
  }
}

/// The one action on a row, sized alike down the list so rows line up.
ButtonStyle _rowButtonStyle(BuildContext context) => ButtonStyle(
  minimumSize: const WidgetStatePropertyAll(Size(76, 34)),
  padding: const WidgetStatePropertyAll(EdgeInsets.symmetric(horizontal: 14)),
  textStyle: WidgetStatePropertyAll(
    Theme.of(context).textTheme.labelLarge?.copyWith(fontSize: 13),
  ),
);

enum _MenuAction { edit, copyKey, copyAddress, delete }

class _TunnelMenu extends StatelessWidget {
  const _TunnelMenu({required this.tunnel, required this.store});

  final Tunnel tunnel;
  final AppStore store;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    PopupMenuItem<_MenuAction> item(
      _MenuAction value,
      IconData icon,
      String label, {
      bool danger = false,
    }) => PopupMenuItem(
      key: Key('menu-${value.name}'),
      value: value,
      child: Row(
        children: [
          Icon(icon, size: 17, color: danger ? scheme.error : null),
          const SizedBox(width: 10),
          Text(label, style: danger ? TextStyle(color: scheme.error) : null),
        ],
      ),
    );
    return PopupMenuButton<_MenuAction>(
      key: Key('menu-${tunnel.id}'),
      tooltip: l10n.more,
      icon: const Icon(AppIcons.more, size: 18),
      onSelected: (action) => switch (action) {
        _MenuAction.edit => openTunnelEditor(context, existing: tunnel),
        _MenuAction.copyKey => copyText(context, tunnel.key),
        _MenuAction.copyAddress => copyText(context, tunnel.localAddress),
        _MenuAction.delete => deleteTunnel(context, store, tunnel),
      },
      itemBuilder: (context) => [
        item(_MenuAction.edit, AppIcons.edit, l10n.edit),
        item(_MenuAction.copyKey, AppIcons.copy, l10n.copyKey),
        item(_MenuAction.copyAddress, AppIcons.copy, l10n.copyAddress),
        const PopupMenuDivider(),
        item(_MenuAction.delete, AppIcons.delete, l10n.delete, danger: true),
      ],
    );
  }
}

/// A key the relay lists that this computer does not share: connect to it,
/// or see what the relay holds for it.
class _RemoteRow extends StatelessWidget {
  const _RemoteRow({required this.serviceKey});

  final String serviceKey;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final store = AppScope.of(context).store;
    final scheme = Theme.of(context).colorScheme;
    final connection = store.connectionTo(serviceKey);
    final inUse = connection?.health == TunnelHealth.running;
    return Padding(
      key: Key('remote-$serviceKey'),
      padding: const EdgeInsets.fromLTRB(14, 8, 10, 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(top: 2),
            child: PlanetGlyph(
              color: inUse ? successColor(scheme) : availableColor(scheme),
              size: 30,
            ),
          ),
          const SizedBox(width: 14),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SelectableText(
                  serviceKey,
                  maxLines: 1,
                  style: mono(
                    fontSize: 13.5,
                    fontWeight: FontWeight.w600,
                    color: scheme.onSurface,
                  ),
                ),
                ConnectionsDisclosure(api: store.api, serviceKey: serviceKey),
              ],
            ),
          ),
          const SizedBox(width: 8),
          if (connection == null)
            FilledButton.tonal(
              key: Key('connect-$serviceKey'),
              onPressed: () => openTunnelEditor(
                context,
                kind: TunnelKind.connect,
                key: serviceKey,
              ),
              style: _rowButtonStyle(context),
              child: Text(l10n.connect),
            )
          else
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: StatusChip(
                color: inUse ? successColor(scheme) : availableColor(scheme),
                label: l10n.openAt(connection.localAddress),
              ),
            ),
        ],
      ),
    );
  }
}
