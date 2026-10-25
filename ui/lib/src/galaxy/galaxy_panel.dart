/// The galaxy's details for what the user picked: this computer, the relay,
/// one of this computer's tunnels, or a service on the relay — each with the
/// one or two things worth doing about it.
library;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_model.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_glyph.dart';
import 'package:pb_mapper_ui/src/pages/relay/relay_format.dart' show ownerName;
import 'package:pb_mapper_ui/src/pages/tunnel_actions.dart';
import 'package:pb_mapper_ui/src/pages/tunnel_editor.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/widgets/brand_mark.dart';
import 'package:pb_mapper_ui/src/widgets/connections.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';

class GalaxyPanel extends StatelessWidget {
  const GalaxyPanel({
    super.key,
    required this.scene,
    required this.id,
    required this.onPick,
    required this.onClose,
  });

  final GalaxyScene scene;

  /// The picked body.
  final String id;

  /// Pick another body.
  final ValueChanged<String> onPick;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    final system = scene.systemOf(id);
    final planet = scene.planetOf(id);
    final Widget body;
    if (system != null) {
      body = _SystemDetail(system: system, onPick: onPick);
    } else if (planet != null) {
      body = _PlanetDetail(planet: planet.planet, onPick: onPick);
    } else {
      body = const SizedBox.shrink();
    }
    return Stack(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(18, 16, 18, 16),
          child: body,
        ),
        PositionedDirectional(
          top: 6,
          end: 6,
          child: IconButton(
            key: const Key('galaxy-panel-close'),
            tooltip: context.l10n.close,
            icon: const Icon(AppIcons.close, size: 17),
            onPressed: onClose,
          ),
        ),
      ],
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({
    required this.leading,
    required this.title,
    this.subtitle,
    this.mono = false,
  });

  final Widget leading;
  final String title;
  final String? subtitle;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final style = theme.textTheme.titleMedium;
    return Padding(
      padding: const EdgeInsetsDirectional.only(end: 34, bottom: 12),
      child: Row(
        children: [
          leading,
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SelectableText(
                  title,
                  maxLines: 2,
                  style: mono ? style?.merge(_monoTitle(style)) : style,
                ),
                if (subtitle != null)
                  Text(
                    subtitle!,
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
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

/// The mono face over [base]'s size and weight, a step smaller: Geist Mono
/// runs wider than the UI face.
TextStyle _monoTitle(TextStyle? base) =>
    mono(fontSize: (base?.fontSize ?? 16) - 1, fontWeight: base?.fontWeight);

class _SystemDetail extends StatelessWidget {
  const _SystemDetail({required this.system, required this.onPick});

  final StarSystem system;
  final ValueChanged<String> onPick;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scope = AppScope.of(context);
    final store = scope.store;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final shares = system.planets.where((p) => p.kind == BodyKind.share).length;
    final connections = system.planets
        .where((p) => p.kind == BodyKind.connect)
        .length;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        _Header(
          leading: system.home
              ? const BrandMark(size: 34)
              : Icon(AppIcons.relay, size: 28, color: scheme.onSurfaceVariant),
          title: system.label,
          subtitle: system.home
              ? '${l10n.summaryShared(shares)} · ${l10n.summaryConnected(connections)}'
              : l10n.servicesCount(system.planets.length),
        ),
        if (!system.home) ...[
          Align(
            alignment: AlignmentDirectional.centerStart,
            child: RelayChip(state: store.relayState),
          ),
          const SizedBox(height: 10),
        ],
        if (system.planets.isNotEmpty)
          Flexible(
            child: ListView(
              shrinkWrap: true,
              padding: EdgeInsets.zero,
              children: [
                for (final planet in system.planets)
                  ListTile(
                    key: Key('galaxy-row-${planet.id}'),
                    dense: true,
                    contentPadding: EdgeInsets.zero,
                    leading: PlanetGlyph(
                      color: planetColor(scheme, planet),
                      size: 26,
                    ),
                    title: Text(
                      planet.label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: mono(fontSize: 13, color: scheme.onSurface),
                    ),
                    subtitle: Text(planetSubtitle(context, planet)),
                    onTap: () => onPick(planet.id),
                  ),
              ],
            ),
          ),
        const SizedBox(height: 10),
        Wrap(
          spacing: 8,
          runSpacing: 8,
          children: system.home
              ? [
                  FilledButton.tonalIcon(
                    onPressed: () =>
                        openTunnelEditor(context, kind: TunnelKind.share),
                    icon: const Icon(AppIcons.share, size: 16),
                    label: Text(l10n.addShare),
                  ),
                  OutlinedButton.icon(
                    onPressed: () =>
                        openTunnelEditor(context, kind: TunnelKind.connect),
                    icon: const Icon(AppIcons.connect, size: 16),
                    label: Text(l10n.addConnect),
                  ),
                ]
              : [
                  FilledButton.tonalIcon(
                    key: const Key('galaxy-open-relay'),
                    onPressed: () => scope.go(AppPage.relay),
                    icon: const Icon(AppIcons.relay, size: 16),
                    label: Text(l10n.relayOpen),
                  ),
                ],
        ),
      ],
    );
  }
}

class _PlanetDetail extends StatelessWidget {
  const _PlanetDetail({required this.planet, required this.onPick});

  final Planet planet;
  final ValueChanged<String> onPick;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final store = AppScope.of(context).store;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return ListenableBuilder(
      listenable: store,
      builder: (context, _) {
        // Read the tunnel fresh: the scene may lag a state change by a frame.
        final tunnel = planet.tunnel == null
            ? null
            : store.find(planet.tunnel!.kind, planet.key);
        // A service on the relay, by namespace and name: with the
        // administrator key it may be a temporary credential's.
        final service = planet.kind == BodyKind.remote
            ? store.remoteById(planet.key)
            : null;
        final connection = service == null ? null : store.connectionTo(service);
        final namespace = service == null
            ? null
            : store.connectNamespace(service);
        final name = service?.name ?? planet.key;
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            _Header(
              leading: PlanetGlyph(
                color: planetColor(scheme, planet),
                size: 36,
              ),
              title: planet.label,
              subtitle: planetSubtitle(context, planet),
              mono: true,
            ),
            if (tunnel != null) ...[
              Wrap(
                spacing: 8,
                runSpacing: 6,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  HealthChip(health: tunnel.health),
                  _Fact(icon: AppIcons.computer, text: tunnel.localAddress),
                  _Fact(icon: AppIcons.link, text: tunnel.protocol),
                  if (tunnel.encrypted)
                    _Fact(icon: AppIcons.lock, text: l10n.encrypted),
                ],
              ),
              if (tunnel.problem case final problem?) ...[
                const SizedBox(height: 8),
                Text(
                  problem,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: tunnel.health == TunnelHealth.failed
                        ? scheme.error
                        : scheme.onSurfaceVariant,
                  ),
                ),
              ],
              const SizedBox(height: 14),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [
                  FilledButton.tonalIcon(
                    key: const Key('galaxy-toggle'),
                    onPressed: () => toggleTunnel(context, store, tunnel),
                    icon: Icon(
                      tunnel.isLive ? AppIcons.stop : AppIcons.start,
                      size: 16,
                    ),
                    label: Text(tunnel.isLive ? l10n.stop : l10n.start),
                  ),
                  OutlinedButton.icon(
                    onPressed: () =>
                        openTunnelEditor(context, existing: tunnel),
                    icon: const Icon(AppIcons.edit, size: 16),
                    label: Text(l10n.edit),
                  ),
                  IconButton(
                    tooltip: l10n.copyKey,
                    onPressed: () => copyText(context, tunnel.key),
                    icon: const Icon(AppIcons.copy, size: 17),
                  ),
                ],
              ),
            ] else ...[
              Align(
                alignment: AlignmentDirectional.centerStart,
                child: StatusChip(
                  color: planetColor(scheme, planet),
                  label: connection == null
                      ? l10n.healthAvailable
                      : l10n.openAt(connection.localAddress),
                ),
              ),
              if (service != null && namespace != null) ...[
                const SizedBox(height: 10),
                _Fact(
                  icon: AppIcons.key,
                  text: ownerName(
                    context,
                    AppScope.of(context).relay,
                    service.owner,
                  ),
                ),
              ],
              const SizedBox(height: 12),
              // What the relay holds for it is asked in this credential's own
              // namespace; another credential's is on the Relay page.
              if (namespace == null)
                ConnectionsDisclosure(api: store.api, serviceKey: name),
              const SizedBox(height: 8),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [
                  if (connection == null)
                    FilledButton.tonalIcon(
                      key: const Key('galaxy-connect'),
                      onPressed: () => openTunnelEditor(
                        context,
                        kind: TunnelKind.connect,
                        key: name,
                        namespace: namespace,
                      ),
                      icon: const Icon(AppIcons.connect, size: 16),
                      label: Text(l10n.connect),
                    )
                  else
                    FilledButton.tonalIcon(
                      onPressed: () => onPick(connection.id),
                      icon: const Icon(AppIcons.forward, size: 16),
                      label: Text(l10n.showConnection),
                    ),
                  IconButton(
                    tooltip: l10n.copyKey,
                    onPressed: () => copyText(context, name),
                    icon: const Icon(AppIcons.copy, size: 17),
                  ),
                ],
              ),
            ],
          ],
        );
      },
    );
  }
}

class _Fact extends StatelessWidget {
  const _Fact({required this.icon, required this.text});

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(icon, size: 14, color: scheme.onSurfaceVariant),
        const SizedBox(width: 4),
        Text(text, style: mono(fontSize: 12, color: scheme.onSurfaceVariant)),
      ],
    );
  }
}

/// A planet's colour outside the galaxy (list rows, headers).
Color planetColor(ColorScheme scheme, Planet planet) {
  if (planet.tunnel case final tunnel?) {
    return healthColor(scheme, tunnel.health);
  }
  return planet.inUse ? successColor(scheme) : availableColor(scheme);
}

/// What a planet is, and its state, in words.
String planetSubtitle(BuildContext context, Planet planet) {
  final l10n = context.l10n;
  return switch (planet.kind) {
    BodyKind.share =>
      '${l10n.kindShare} · ${healthLabel(context, planet.tunnel!.health)}',
    BodyKind.connect =>
      '${l10n.kindConnect} · ${healthLabel(context, planet.tunnel!.health)}',
    BodyKind.remote =>
      '${l10n.kindRemote} · ${planet.inUse ? l10n.healthInUse : l10n.healthAvailable}',
  };
}
