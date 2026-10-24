import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/pages/relay/connections_tab.dart';
import 'package:pb_mapper_ui/src/pages/relay/credentials_tab.dart'
    show CredentialsTab, issueCredential;
import 'package:pb_mapper_ui/src/pages/relay/local_tab.dart';
import 'package:pb_mapper_ui/src/pages/relay/security_tab.dart';
import 'package:pb_mapper_ui/src/pages/relay/services_tab.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

enum RelaySection { services, connections, credentials, security, local }

/// The relay as a whole: whether it answers and how fast, who this computer
/// is to it, what it holds, and — with the administrator key — everything an
/// administrator can do to it. The page reads the relay only while it is on
/// screen.
class RelayPage extends StatefulWidget {
  const RelayPage({super.key, this.initial = RelaySection.services});

  final RelaySection initial;

  @override
  State<RelayPage> createState() => _RelayPageState();
}

class _RelayPageState extends State<RelayPage> {
  late RelaySection _section = widget.initial;
  RelayStore? _relay;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final relay = AppScope.of(context).relay;
    if (!identical(relay, _relay)) {
      _relay?.unwatch();
      _relay = relay..watch();
    }
  }

  @override
  void dispose() {
    _relay?.unwatch();
    super.dispose();
  }

  List<RelaySection> _sections(RelayStore relay) => [
    RelaySection.services,
    RelaySection.connections,
    if (relay.isAdmin) ...[RelaySection.credentials, RelaySection.security],
    RelaySection.local,
  ];

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scope = AppScope.of(context);
    final store = scope.store;
    final relay = scope.relay;
    return ListenableBuilder(
      listenable: Listenable.merge([store, relay]),
      builder: (context, _) => PageFrame(
        title: l10n.navRelay,
        actions: [
          IconButton(
            key: const Key('relay-refresh'),
            tooltip: l10n.refresh,
            onPressed: relay.loading
                ? null
                : () async {
                    await store.checkRelay(force: true);
                    await relay.refresh();
                  },
            icon: const Icon(AppIcons.refresh),
          ),
        ],
        // The one thing an administrator comes here to do most often.
        primary: relay.isAdmin
            ? PrimaryAction(
                key: const Key('relay-issue'),
                icon: AppIcons.add,
                label: l10n.credentialIssue,
                onPressed: () => issueCredential(context, relay, store),
              )
            : null,
        body: Builder(
          builder: (context) {
            final sections = _sections(relay);
            // A credential that stopped being the administrator's takes its
            // sections with it.
            final section = sections.contains(_section)
                ? _section
                : RelaySection.services;
            return Align(
              alignment: Alignment.topCenter,
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 920),
                child: ListView(
                  key: const Key('relay-list'),
                  padding: const EdgeInsets.fromLTRB(18, 4, 18, 28),
                  children: [
                    _Hero(store: store, relay: relay),
                    const SizedBox(height: 16),
                    // Across the page where there is room; on a phone it
                    // scrolls sideways rather than squeeze five labels.
                    LayoutBuilder(
                      builder: (context, box) {
                        final wide = box.maxWidth >= 640;
                        final bar = GlassSegmented<RelaySection>(
                          key: const Key('relay-sections'),
                          expand: wide,
                          selected: section,
                          onChanged: (value) =>
                              setState(() => _section = value),
                          segments: [
                            for (final s in sections)
                              GlassSegment(
                                key: Key('relay-section-${s.name}'),
                                value: s,
                                label: _label(context, s),
                                icon: _icon(s),
                              ),
                          ],
                        );
                        return wide
                            ? bar
                            : SingleChildScrollView(
                                scrollDirection: Axis.horizontal,
                                child: bar,
                              );
                      },
                    ),
                    const SizedBox(height: 8),
                    switch (section) {
                      RelaySection.services => const ServicesTab(),
                      RelaySection.connections => const ConnectionsTab(),
                      RelaySection.credentials => const CredentialsTab(),
                      RelaySection.security => const SecurityTab(),
                      RelaySection.local => const LocalRelayTab(),
                    },
                  ],
                ),
              ),
            );
          },
        ),
      ),
    );
  }

  static String _label(BuildContext context, RelaySection section) {
    final l10n = context.l10n;
    return switch (section) {
      RelaySection.services => l10n.relayTabServices,
      RelaySection.connections => l10n.relayTabConnections,
      RelaySection.credentials => l10n.relayTabCredentials,
      RelaySection.security => l10n.relayTabSecurity,
      RelaySection.local => l10n.relayTabLocal,
    };
  }

  static IconData _icon(RelaySection section) => switch (section) {
    RelaySection.services => AppIcons.service,
    RelaySection.connections => AppIcons.link,
    RelaySection.credentials => AppIcons.key,
    RelaySection.security => AppIcons.lock,
    RelaySection.local => AppIcons.computer,
  };
}

/// Whether the relay answers, how fast, who this computer is to it, and how
/// busy it is, at a glance.
class _Hero extends StatelessWidget {
  const _Hero({required this.store, required this.relay});

  final AppStore store;
  final RelayStore relay;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: scheme.onSurfaceVariant,
    );
    final status = store.relay;
    final online = store.relayState == RelayState.online;
    final identity = relay.identity;
    final auth = relay.authStatus;

    final head = Row(
      children: [
        Container(
          width: 46,
          height: 46,
          decoration: BoxDecoration(
            color: scheme.tertiaryContainer,
            borderRadius: BorderRadius.circular(14),
          ),
          child: Icon(AppIcons.relay, color: scheme.onTertiaryContainer),
        ),
        const SizedBox(width: 14),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SelectableText(
                store.hasRelay ? store.relayAddress : l10n.relayUnset,
                key: const Key('relay-address'),
                maxLines: 1,
                style: mono(
                  fontSize: 15,
                  fontWeight: FontWeight.w600,
                  color: scheme.onSurface,
                ),
              ),
              const SizedBox(height: 6),
              Wrap(
                spacing: 8,
                runSpacing: 6,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  RelayChip(state: store.relayState),
                  if (online && status?.latencyMs != null)
                    Text(l10n.relayLatency(status!.latencyMs!), style: muted),
                  if (relay.identityLoaded) _RoleChip(identity: identity),
                ],
              ),
            ],
          ),
        ),
      ],
    );

    // Counts the relay keeps as such. Its status strings are diagnostics:
    // counting the ids in them would count a publisher's idle pool as
    // traffic, so connections are an administrator's, from the inventory.
    final stats = [
      (l10n.relayStatServices, online ? '${relay.serviceRows.length}' : '—'),
      if (relay.isAdmin && relay.adminLoaded)
        (l10n.relayTabConnections, '${relay.connections.length}'),
      (l10n.relayStatSessions, online ? '${relay.sessions.length}' : '—'),
      if (relay.isAdmin && auth != null)
        (l10n.relayStatCredentials, '${auth.activeKeys} / ${auth.capacity}'),
    ];

    return RaisedSurface(
      radius: 20,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(18, 18, 18, 16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            head,
            const SizedBox(height: 16),
            Wrap(
              spacing: 10,
              runSpacing: 10,
              children: [
                for (final (label, value) in stats)
                  _Stat(label: label, value: value),
              ],
            ),
            if (store.relayState == RelayState.offline) ...[
              const SizedBox(height: 12),
              Text(
                l10n.relayNotAnswering(status?.error ?? l10n.relayNoReason),
                key: const Key('relay-error'),
                style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
              ),
            ],
            if (relay.identityLoaded && !identity.isAdmin) ...[
              const SizedBox(height: 12),
              Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Icon(AppIcons.info, size: 15, color: onSurfaceMuted(scheme)),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      identity.role == RelayRole.temporary
                          ? l10n.relayTemporaryHint
                          : l10n.relayAdminHint,
                      key: const Key('relay-role-hint'),
                      style: muted,
                    ),
                  ),
                ],
              ),
            ],
            // A relay that does not answer has already said why above.
            if (relay.isAdmin &&
                relay.adminError != null &&
                store.relayState != RelayState.offline) ...[
              const SizedBox(height: 12),
              Text(
                l10n.relayAdminError(relay.adminError!),
                style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _RoleChip extends StatelessWidget {
  const _RoleChip({required this.identity});

  final RelayIdentity identity;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    return switch (identity.role) {
      RelayRole.admin => StatusChip(
        key: const Key('relay-role-admin'),
        color: signalColor(scheme),
        label: l10n.relayRoleAdmin,
      ),
      RelayRole.temporary => StatusChip(
        key: const Key('relay-role-temporary'),
        color: availableColor(scheme),
        label: l10n.relayRoleTemporary(identity.keyId ?? 0),
      ),
      RelayRole.none => StatusChip(
        key: const Key('relay-role-none'),
        color: cautionColor(scheme),
        label: l10n.relayRoleNone,
      ),
    };
  }
}

class _Stat extends StatelessWidget {
  const _Stat({required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      constraints: const BoxConstraints(minWidth: 112),
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 10),
      decoration: BoxDecoration(
        color: softWash(scheme),
        borderRadius: BorderRadius.circular(12),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            value,
            style: theme.textTheme.titleLarge?.copyWith(
              fontWeight: FontWeight.w600,
              height: 1.1,
            ),
          ),
          const SizedBox(height: 2),
          Text(
            label,
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}
