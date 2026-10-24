import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_canvas.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_model.dart';
import 'package:pb_mapper_ui/src/galaxy/galaxy_panel.dart';
import 'package:pb_mapper_ui/src/galaxy/planet_glyph.dart';
import 'package:pb_mapper_ui/src/pages/tunnel_editor.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/shell/page_frame.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';

/// The overview: this computer and the relay as two stars, every tunnel and
/// every service on the relay as a planet, live tunnels as arcs between them.
/// Click a body for what it is and what can be done about it.
class OverviewPage extends StatefulWidget {
  const OverviewPage({super.key});

  @override
  State<OverviewPage> createState() => _OverviewPageState();
}

class _OverviewPageState extends State<OverviewPage> {
  final GlobalKey<GalaxyCanvasState> _canvas = GlobalKey();
  GalaxyScene _scene = GalaxyScene.empty;
  _SceneInputs? _inputs;

  /// The scene for the store's current state, rebuilt only when something it
  /// shows changed, so a relay check that found nothing new does not count as
  /// a change in the sky.
  GalaxyScene _sceneFor(AppStore store, String homeLabel) {
    final inputs = _SceneInputs(
      shares: store.shares,
      connections: store.connections,
      relay: store.hasRelay ? store.relayAddress : null,
      online: store.relayState == RelayState.online,
      remote: store.remoteServices,
      homeLabel: homeLabel,
    );
    if (inputs == _inputs) return _scene;
    _inputs = inputs;
    return _scene = buildScene(
      shares: inputs.shares,
      connections: inputs.connections,
      relayAddress: inputs.relay,
      relayOnline: inputs.online,
      remote: inputs.remote,
      homeLabel: homeLabel,
    );
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scope = AppScope.of(context);
    final store = scope.store;
    final view = scope.galaxy;
    final wide = ShellLayout.wideOf(context);
    return PageFrame(
      title: l10n.navOverview,
      overlay: true,
      actions: [
        IconButton(
          key: const Key('overview-reset'),
          tooltip: l10n.showEverything,
          onPressed: () => _canvas.currentState?.overview(),
          icon: const Icon(AppIcons.overview),
        ),
        IconButton(
          key: const Key('overview-refresh'),
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
        builder: (context, _) {
          final scene = _sceneFor(store, l10n.thisComputer);
          final selected = view.selected;
          final top = PageFrame.barHeight(context) + 6;
          final empty = store.loaded && store.tunnels.isEmpty;
          return LayoutBuilder(
            builder: (context, box) => Stack(
              children: [
                Positioned.fill(
                  child: GalaxyCanvas(
                    key: _canvas,
                    view: view,
                    scene: scene,
                    semanticLabel: l10n.galaxyLabel,
                    onSelect: (_) => setState(() {}),
                  ),
                ),
                // Bounded on both sides so its chip can ellipsize; the Align
                // takes no taps outside the pill, so the sky stays draggable.
                Positioned(
                  top: top,
                  left: wide ? 22 : 12,
                  right: wide ? 22 : 12,
                  child: Align(
                    alignment: AlignmentDirectional.centerStart,
                    child: _Summary(store: store),
                  ),
                ),
                // Below the stars rather than over them: the empty sky is the
                // picture the card is explaining.
                if (empty && selected == null)
                  Positioned(
                    left: 0,
                    right: 0,
                    bottom: wide ? 64 : 12,
                    child: const Center(child: _EmptyCard()),
                  ),
                if (wide && selected == null) ...[
                  const Positioned(left: 18, bottom: 16, child: _Legend()),
                  Positioned(
                    right: 20,
                    bottom: 20,
                    child: Text(
                      l10n.galaxyHint,
                      style: Theme.of(context).textTheme.bodySmall?.copyWith(
                        color: onSurfaceMuted(Theme.of(context).colorScheme),
                      ),
                    ),
                  ),
                ],
                if (selected != null)
                  Positioned(
                    top: wide ? top : null,
                    right: wide ? 16 : 8,
                    left: wide ? null : 8,
                    bottom: wide ? null : 8,
                    width: wide ? 360 : null,
                    child: ConstrainedBox(
                      constraints: BoxConstraints(
                        maxHeight: wide
                            ? box.maxHeight - top - 24
                            : box.maxHeight * 0.62,
                      ),
                      child: Glass(
                        key: ValueKey('galaxy-panel-$selected'),
                        depth: GlassDepth.overlay,
                        borderRadius: BorderRadius.circular(20),
                        child: SingleChildScrollView(
                          child: GalaxyPanel(
                            scene: scene,
                            id: selected,
                            onPick: (id) => _canvas.currentState?.select(id),
                            onClose: () => _canvas.currentState?.select(null),
                          ),
                        ),
                      ),
                    ),
                  ),
              ],
            ),
          );
        },
      ),
    );
  }
}

/// What a scene is built from, compared by value.
@immutable
class _SceneInputs {
  const _SceneInputs({
    required this.shares,
    required this.connections,
    required this.relay,
    required this.online,
    required this.remote,
    required this.homeLabel,
  });

  final List<Tunnel> shares;
  final List<Tunnel> connections;
  final String? relay;
  final bool online;
  final List<String> remote;
  final String homeLabel;

  @override
  bool operator ==(Object other) =>
      other is _SceneInputs &&
      listEquals(other.shares, shares) &&
      listEquals(other.connections, connections) &&
      other.relay == relay &&
      other.online == online &&
      listEquals(other.remote, remote) &&
      other.homeLabel == homeLabel;

  @override
  int get hashCode => Object.hash(
    Object.hashAll(shares),
    Object.hashAll(connections),
    relay,
    online,
    Object.hashAll(remote),
    homeLabel,
  );
}

/// The relay's state and this computer's counts, floating over the sky.
class _Summary extends StatelessWidget {
  const _Summary({required this.store});

  final AppStore store;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: scheme.onSurfaceVariant,
      fontSize: 12,
    );
    return Glass(
      depth: GlassDepth.floating,
      borderRadius: BorderRadius.circular(999),
      padding: const EdgeInsets.fromLTRB(12, 7, 14, 7),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          RelayChip(state: store.relayState, filled: false),
          // The counts give way first on a narrow window.
          Flexible(
            child: Text(
              '  ·  ${l10n.summaryShared(store.shares.length)}'
              '  ·  ${l10n.summaryConnected(store.connections.length)}',
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: muted,
            ),
          ),
        ],
      ),
    );
  }
}

/// The first thing a new user sees in an empty sky: what a planet is, and the
/// two ways to make one.
class _EmptyCard extends StatelessWidget {
  const _EmptyCard();

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 420),
        child: Glass(
          key: const Key('overview-empty'),
          depth: GlassDepth.overlay,
          borderRadius: BorderRadius.circular(24),
          padding: const EdgeInsets.fromLTRB(24, 22, 24, 22),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                l10n.overviewEmptyTitle,
                textAlign: TextAlign.center,
                style: theme.textTheme.titleMedium,
              ),
              const SizedBox(height: 8),
              Text(
                l10n.overviewEmptyBody,
                textAlign: TextAlign.center,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                  fontSize: 13,
                ),
              ),
              const SizedBox(height: 18),
              Wrap(
                alignment: WrapAlignment.center,
                spacing: 10,
                runSpacing: 10,
                children: [
                  FilledButton.icon(
                    key: const Key('empty-share'),
                    onPressed: () =>
                        openTunnelEditor(context, kind: TunnelKind.share),
                    icon: const Icon(AppIcons.share, size: 17),
                    label: Text(l10n.addShare),
                  ),
                  OutlinedButton.icon(
                    key: const Key('empty-connect'),
                    onPressed: () =>
                        openTunnelEditor(context, kind: TunnelKind.connect),
                    icon: const Icon(AppIcons.connect, size: 17),
                    label: Text(l10n.addConnect),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// What the colours mean.
class _Legend extends StatelessWidget {
  const _Legend();

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    Widget entry(Color color, String label) => Padding(
      padding: const EdgeInsets.only(right: 12),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          PlanetGlyph(color: color, size: 16),
          const SizedBox(width: 4),
          Text(
            label,
            style: TextStyle(fontSize: 11.5, color: scheme.onSurfaceVariant),
          ),
        ],
      ),
    );
    return ExcludeSemantics(
      child: Glass(
        depth: GlassDepth.chrome,
        borderRadius: BorderRadius.circular(12),
        padding: const EdgeInsets.fromLTRB(10, 6, 0, 6),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            entry(
              healthColor(scheme, TunnelHealth.running),
              l10n.healthRunning,
            ),
            entry(
              healthColor(scheme, TunnelHealth.retrying),
              l10n.healthRetrying,
            ),
            entry(healthColor(scheme, TunnelHealth.failed), l10n.healthFailed),
            entry(
              healthColor(scheme, TunnelHealth.stopped),
              l10n.healthStopped,
            ),
            entry(availableColor(scheme), l10n.healthAvailable),
          ],
        ),
      ),
    );
  }
}
