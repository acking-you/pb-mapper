import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/pages/remote/add_computer.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_actions.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_format.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/desktop_store.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';
import 'package:pb_mapper_ui/src/widgets/dashed_border.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// One computer on the list: a remembered one, or the open session of one
/// that is not remembered (yet), shown first while it lasts.
@immutable
class RemoteEntry {
  const RemoteEntry({
    required this.id,
    required this.name,
    required this.presence,
    this.computer,
  });

  final String id, name;
  final ComputerPresence presence;

  /// Null for the open session of a computer that is not remembered.
  final RemoteComputer? computer;

  bool get viewing => presence == ComputerPresence.viewing;
}

/// The list's entries, in its order: an unremembered open session first,
/// then the computers that can be opened (viewing, online, unknown), then
/// the ones that cannot. Each group keeps the store's order, the most
/// recently connected first.
({RemoteEntry? transient, List<RemoteEntry> open, List<RemoteEntry> closed})
remoteEntries(AppStore store) {
  final session = store.desktop.session.value;
  final computers = store.computers;
  final target = session.target;
  final transient =
      target != null &&
          session.open &&
          (target.computerId == null ||
              computers.byId(target.computerId!) == null)
      ? RemoteEntry(
          id: target.computerId ?? 'session',
          name: target.name,
          presence: ComputerPresence.viewing,
        )
      : null;
  final relay = store.relayState;
  final services = store.desktopServiceNames;
  final open = <RemoteEntry>[], closed = <RemoteEntry>[];
  for (final c in computers.computers) {
    final presence = presenceOf(
      c,
      session: session,
      relay: relay,
      relayAddress: store.relayAddress,
      credentialKeyId: store.credentialKeyId,
      desktopServices: services,
    );
    final entry = RemoteEntry(
      id: c.id,
      name: c.name,
      presence: presence,
      computer: c,
    );
    switch (presence) {
      case ComputerPresence.viewing ||
          ComputerPresence.online ||
          ComputerPresence.unknown:
        open.add(entry);
      case ComputerPresence.notSharing || ComputerPresence.otherRelay:
        closed.add(entry);
    }
  }
  return (transient: transient, open: open, closed: closed);
}

/// The status pill for [presence]; none while the relay cannot say.
Widget? presencePill(BuildContext context, ComputerPresence presence) {
  final l10n = context.l10n;
  final scheme = Theme.of(context).colorScheme;
  return switch (presence) {
    ComputerPresence.viewing => StatusChip(
      inkLabel: true,
      shrinks: true,
      color: signalColor(scheme),
      label: l10n.remoteViewing,
    ),
    ComputerPresence.online => StatusChip(
      inkLabel: true,
      shrinks: true,
      color: successColor(scheme),
      label: l10n.remoteOnline,
    ),
    ComputerPresence.notSharing => StatusChip(
      shrinks: true,
      color: onSurfaceMuted(scheme),
      label: l10n.remoteNotSharing,
    ),
    ComputerPresence.otherRelay => StatusChip(
      shrinks: true,
      color: onSurfaceMuted(scheme),
      label: l10n.remoteOtherRelay,
    ),
    ComputerPresence.unknown => null,
  };
}

/// A computer's state as a dot and a few words, where a row has no room for
/// the pill and the meta: on a phone, and on the narrowest wide list.
class PresenceLine extends StatelessWidget {
  const PresenceLine({super.key, required this.entry});
  final RemoteEntry entry;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final c = entry.computer;
    final at = c?.lastConnectedAt;
    final when = at == null
        ? null
        : formatLastSeen(context, at, DateTime.now());
    final path = c?.lastPath;
    final (dot, words) = switch (entry.presence) {
      ComputerPresence.viewing => (signalColor(scheme), l10n.remoteViewing),
      ComputerPresence.online => (
        successColor(scheme),
        path == null
            ? l10n.remoteOnline
            : l10n.remoteOnlineLast(pathLabel(context, path)),
      ),
      ComputerPresence.notSharing => (
        onSurfaceMuted(scheme),
        when == null ? l10n.remoteNotSharing : l10n.remoteOfflineLast(when),
      ),
      ComputerPresence.otherRelay => (
        onSurfaceMuted(scheme),
        l10n.remoteOtherRelay,
      ),
      ComputerPresence.unknown => (
        onSurfaceMuted(scheme),
        when == null ? '' : l10n.remoteLast(when),
      ),
    };
    if (words.isEmpty) return const SizedBox.shrink();
    return Padding(
      padding: const EdgeInsets.only(top: 2),
      child: Row(
        children: [
          StatusDot(color: dot, size: 6),
          const SizedBox(width: 6),
          Expanded(
            child: Text(
              words,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant),
            ),
          ),
        ],
      ),
    );
  }
}

/// The vault has answered that it cannot keep anything, rather than not
/// having answered yet.
bool cannotRemember(RememberSupport support) =>
    support is RememberUnavailable && support.reason != 'loading';

/// The footnote under the list: what is kept, or why nothing can be. Saved
/// computers that cannot be opened come with the way to clear them.
class RememberNote extends StatelessWidget {
  const RememberNote({super.key, required this.support, this.fontSize = 12.5});
  final RememberSupport support;
  final double fontSize;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final note = Text(
      cannotRemember(support)
          ? cannotRememberText(context, support)
          : context.l10n.remoteRememberNote,
      style: TextStyle(fontSize: fontSize, color: quietText(scheme)),
    );
    if (!vaultUnreadable(support)) return note;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        note,
        const SizedBox(height: 6),
        TextButton(
          key: const Key('remote-vault-reset'),
          style: TextButton.styleFrom(
            foregroundColor: scheme.error,
            padding: EdgeInsets.zero,
            minimumSize: const Size(0, 32),
            textStyle: buttonLabel(context, 13.5),
          ),
          onPressed: () => unawaited(resetComputers(context)),
          child: Text(context.l10n.remoteVaultReset),
        ),
      ],
    );
  }
}

/// The remembered computers on a wide window (Main): each with its state on
/// the relay and the one thing to do with it, then Add computer.
class RemoteComputerList extends StatelessWidget {
  const RemoteComputerList({super.key, this.emptyMinHeight = 0});

  /// The empty card's height, to stand level beside the host card.
  final double emptyMinHeight;

  @override
  Widget build(BuildContext context) {
    final store = AppScope.of(context).store;
    return ListenableBuilder(
      listenable: Listenable.merge([
        store.computers,
        store,
        store.desktop.session,
      ]),
      builder: (context, _) {
        final (:transient, :open, :closed) = remoteEntries(store);
        final support = store.computers.support;
        if (transient == null && open.isEmpty && closed.isEmpty) {
          return RemoteEmptyCard(
            minHeight: emptyMinHeight,
            note: vaultUnreadable(support)
                ? RememberNote(support: support)
                : null,
          );
        }
        return _ListCard(
          count: store.computers.computers.length,
          entries: [?transient, ...open],
          closed: closed,
          support: support,
        );
      },
    );
  }
}

class _ListCard extends StatelessWidget {
  const _ListCard({
    required this.count,
    required this.entries,
    required this.closed,
    required this.support,
  });

  final int count;
  final List<RemoteEntry> entries, closed;
  final RememberSupport support;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    // The pill and action columns are as wide as their longest words in
    // this language, so the rows line up whatever they say.
    final pill = math.max(
      86.0,
      [
                l10n.remoteViewing,
                l10n.remoteOnline,
                l10n.remoteNotSharing,
                l10n.remoteOtherRelay,
              ]
              .map(
                (s) => measureTextLine(
                  context,
                  s,
                  const TextStyle(fontSize: 12, fontWeight: FontWeight.w500),
                ).width,
              )
              .reduce(math.max)
              .ceilToDouble() +
          34,
    );
    final action = math.max(
      96.0,
      [l10n.remoteOpen, l10n.remoteBackToPicture]
              .map(
                (s) => measureTextLine(
                  context,
                  s,
                  buttonLabel(context, 13.5),
                ).width,
              )
              .reduce(math.max)
              .ceilToDouble() +
          28,
    );
    Widget row(RemoteEntry e) =>
        ComputerRow(entry: e, pillWidth: pill, actionWidth: action);
    return RaisedSurface(
      radius: kCardRadius,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(20, 20, 20, 24),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(4, 4, 4, 12),
              child: Row(
                children: [
                  Flexible(
                    child: Text(
                      l10n.remoteListTitle,
                      style: const TextStyle(
                        fontSize: 17,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  const SizedBox(width: 8),
                  Container(
                    constraints: const BoxConstraints(minWidth: 20),
                    height: 20,
                    padding: const EdgeInsets.symmetric(horizontal: 6),
                    alignment: Alignment.center,
                    decoration: BoxDecoration(
                      color: backdropBase(scheme),
                      borderRadius: BorderRadius.circular(10),
                    ),
                    child: Text(
                      '$count',
                      style: TextStyle(
                        fontSize: 12,
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ),
                ],
              ),
            ),
            for (final (i, e) in entries.indexed) ...[
              if (i > 0) const SizedBox(height: 4),
              row(e),
            ],
            if (entries.isNotEmpty && closed.isNotEmpty)
              Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: 12,
                  vertical: 4,
                ),
                child: Divider(height: 1, color: scheme.outlineVariant),
              ),
            for (final (i, e) in closed.indexed) ...[
              if (i > 0) const SizedBox(height: 4),
              row(e),
            ],
            const SizedBox(height: 12),
            const AddComputerTile(),
            Padding(
              padding: const EdgeInsets.fromLTRB(4, 10, 4, 0),
              child: RememberNote(support: support),
            ),
          ],
        ),
      ),
    );
  }
}

/// The computer's monitor tile: blue while viewed, ink when it can be
/// opened, faint when it cannot.
class ComputerTile extends StatelessWidget {
  const ComputerTile({super.key, required this.presence});
  final ComputerPresence presence;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final viewing = presence == ComputerPresence.viewing;
    return Container(
      width: 40,
      height: 40,
      decoration: BoxDecoration(
        color: viewing ? scheme.tertiaryContainer : cardFieldFill(scheme),
        borderRadius: BorderRadius.circular(11),
      ),
      child: Icon(
        AppIcons.remote,
        size: AppIconSize.nav,
        color: switch (presence) {
          ComputerPresence.viewing => signalColor(scheme),
          ComputerPresence.online => scheme.onSurfaceVariant,
          _ => onSurfaceMuted(scheme),
        },
      ),
    );
  }
}

/// One thing a computer's row offers, drawn as a menu item on a wide list
/// and as a sheet row on a phone.
typedef ComputerAction = ({
  Key key,
  IconData icon,
  String label,
  VoidCallback? onPressed,
  bool destructive,
});

/// Rename and remove a remembered computer; remember the open session of
/// one that is not.
List<ComputerAction> computerActions(BuildContext context, RemoteEntry entry) {
  final l10n = context.l10n;
  final scope = AppScope.of(context);
  final c = entry.computer;
  if (c == null) {
    return [
      (
        key: const Key('computer-remember'),
        icon: AppIcons.star,
        label: l10n.addRemember,
        onPressed:
            scope.remote.canRememberCurrent &&
                scope.store.computers.support.available
            ? () => unawaited(rememberOpenComputer(context))
            : null,
        destructive: false,
      ),
    ];
  }
  return [
    (
      key: Key('computer-rename-${c.id}'),
      icon: AppIcons.edit,
      label: l10n.remoteRename,
      onPressed: () => unawaited(renameComputer(context, c)),
      destructive: false,
    ),
    (
      key: Key('computer-remove-${c.id}'),
      icon: AppIcons.delete,
      label: l10n.remoteRemove,
      onPressed: () => unawaited(removeComputer(context, c)),
      destructive: true,
    ),
  ];
}

List<Widget> _menuItems(BuildContext context, RemoteEntry entry) {
  final error = Theme.of(context).colorScheme.error;
  return [
    for (final a in computerActions(context, entry))
      MenuItemButton(
        key: a.key,
        leadingIcon: Icon(
          a.icon,
          size: AppIconSize.inline,
          color: a.destructive ? error : null,
        ),
        style: a.destructive
            ? MenuItemButton.styleFrom(foregroundColor: error)
            : null,
        onPressed: a.onPressed,
        child: Text(a.label),
      ),
  ];
}

/// How a wide row fits its width: everything in columns, the meta under the
/// service line, or both as one [PresenceLine], as on a phone.
enum _RowLayout { columns, metaUnder, line }

/// The narrowest a row's name may get before its columns stack.
const double _minName = 116;

/// One computer on a wide list: tile, name and service, how it was last
/// reached, its state, and Open or Back to picture. Hovering reveals its
/// menu; a secondary click opens it where the pointer is.
class ComputerRow extends StatefulWidget {
  const ComputerRow({
    super.key,
    required this.entry,
    this.pillWidth = 86,
    this.actionWidth = 96,
  });

  final RemoteEntry entry;
  final double pillWidth, actionWidth;

  @override
  State<ComputerRow> createState() => _ComputerRowState();
}

class _ComputerRowState extends State<ComputerRow> {
  final _menu = MenuController();
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final entry = widget.entry;
    final c = entry.computer;
    final service = Text(
      c?.service ?? '',
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: mono(fontSize: 12, color: quietText(scheme)),
    );
    final name = Text(
      entry.name,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: const TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
    );
    final pill = presencePill(context, entry.presence);
    return MenuAnchor(
      controller: _menu,
      // The menu button stays while its menu is open.
      onOpen: () => setState(() {}),
      onClose: () {
        if (mounted) setState(() {});
      },
      menuChildren: _menuItems(context, entry),
      child: MouseRegion(
        onEnter: (_) => setState(() => _hover = true),
        onExit: (_) => setState(() => _hover = false),
        child: GestureDetector(
          onSecondaryTapUp: (details) =>
              _menu.open(position: details.localPosition),
          onLongPressStart: (details) =>
              _menu.open(position: details.localPosition),
          child: Container(
            key: Key('computer-${entry.id}'),
            padding: const EdgeInsets.all(12),
            decoration: BoxDecoration(
              color: entry.viewing
                  ? scheme.tertiaryContainer.withValues(alpha: 0.45)
                  : null,
              borderRadius: BorderRadius.circular(kRowRadius),
            ),
            child: LayoutBuilder(
              builder: (context, box) {
                // The name keeps at least _minName: first the meta moves
                // under the service line (below 600 at the narrowest
                // columns), then the pill and the meta become one line.
                final ends = 40 + 14 + 8 + 36 + 6 + widget.actionWidth;
                final withPill = ends + 14 + widget.pillWidth;
                final layout = box.maxWidth - withPill - 184 >= _minName
                    ? _RowLayout.columns
                    : box.maxWidth - withPill >= _minName
                    ? _RowLayout.metaUnder
                    : _RowLayout.line;
                final meta = _Meta(entry: entry);
                return Row(
                  children: [
                    ComputerTile(presence: entry.presence),
                    const SizedBox(width: 14),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          name,
                          if (c != null) ...[
                            const SizedBox(height: 2),
                            service,
                          ],
                          if (layout == _RowLayout.metaUnder) ...[
                            const SizedBox(height: 2),
                            meta,
                          ],
                          if (layout == _RowLayout.line)
                            PresenceLine(entry: entry),
                        ],
                      ),
                    ),
                    if (layout == _RowLayout.columns) ...[
                      const SizedBox(width: 14),
                      SizedBox(width: 170, child: meta),
                    ],
                    if (layout != _RowLayout.line) ...[
                      const SizedBox(width: 14),
                      SizedBox(
                        width: widget.pillWidth,
                        child: Center(child: pill ?? const SizedBox.shrink()),
                      ),
                    ],
                    const SizedBox(width: 8),
                    SizedBox(
                      width: 36,
                      child: _hover || _menu.isOpen
                          ? IconButton(
                              key: Key('computer-more-${entry.id}'),
                              tooltip: context.l10n.more,
                              style: IconButton.styleFrom(
                                minimumSize: const Size(32, 32),
                              ),
                              onPressed: () =>
                                  _menu.isOpen ? _menu.close() : _menu.open(),
                              icon: const Icon(
                                AppIcons.more,
                                size: AppIconSize.button,
                              ),
                            )
                          : null,
                    ),
                    const SizedBox(width: 6),
                    SizedBox(
                      width: widget.actionWidth,
                      child: _RowAction(entry: entry),
                    ),
                  ],
                );
              },
            ),
          ),
        ),
      ),
    );
  }
}

/// How a computer was last reached, or how the open session runs now.
class _Meta extends StatelessWidget {
  const _Meta({required this.entry});
  final RemoteEntry entry;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final style = TextStyle(fontSize: 12.5, color: scheme.onSurfaceVariant);
    Text text(String s) =>
        Text(s, maxLines: 1, overflow: TextOverflow.ellipsis, style: style);
    if (entry.viewing) {
      final desktop = AppScope.of(context).store.desktop;
      // Only this row listens to the stats, which are sampled only while
      // something listens.
      return ValueListenableBuilder<DesktopStats>(
        valueListenable: desktop.stats,
        builder: (context, stats, _) {
          final session = desktop.session.value;
          final path = pathLabel(context, session.path);
          if (session.paused) return text(l10n.remotePicturePaused(path));
          final rtt = stats.rttMs;
          if (rtt == null || !session.hasPicture) return text(path);
          return text(
            l10n.remoteViewingMeta(path, rtt, stats.presentedFps.round()),
          );
        },
      );
    }
    final c = entry.computer;
    final at = c?.lastConnectedAt;
    if (c == null || at == null) return const SizedBox.shrink();
    final when = formatLastSeen(context, at, DateTime.now());
    final path = c.lastPath;
    return text(
      entry.presence == ComputerPresence.online && path != null
          ? l10n.remoteLastPath(when, pathLabel(context, path))
          : l10n.remoteLast(when),
    );
  }
}

class _RowAction extends StatelessWidget {
  const _RowAction({required this.entry});
  final RemoteEntry entry;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scope = AppScope.of(context);
    final key = Key('computer-open-${entry.id}');
    final style = ButtonStyle(
      minimumSize: const WidgetStatePropertyAll(Size.fromHeight(34)),
      padding: const WidgetStatePropertyAll(
        EdgeInsets.symmetric(horizontal: 12),
      ),
      textStyle: WidgetStatePropertyAll(buttonLabel(context, 13.5)),
    );
    final c = entry.computer;
    switch (entry.presence) {
      case ComputerPresence.viewing:
        return FilledButton(
          key: key,
          style: style,
          onPressed: scope.remote.viewer.open,
          child: Text(l10n.remoteBackToPicture, maxLines: 1),
        );
      case ComputerPresence.online || ComputerPresence.unknown:
        return FilledButton(
          key: key,
          style: style,
          onPressed: c == null
              ? null
              : () => unawaited(openComputer(context, c)),
          child: Text(l10n.remoteOpen, maxLines: 1),
        );
      case ComputerPresence.notSharing || ComputerPresence.otherRelay:
        return Tooltip(
          message: entry.presence == ComputerPresence.notSharing
              ? l10n.remoteOfflineHint
              : l10n.remoteOtherRelayHint,
          child: OutlinedButton(
            key: key,
            style: style,
            onPressed: null,
            child: Text(l10n.remoteOpen, maxLines: 1),
          ),
        );
    }
  }
}

/// Add computer as a dashed tile at the end of the list.
class AddComputerTile extends StatelessWidget {
  const AddComputerTile({super.key});

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final shape = DashedBorder(
      side: BorderSide(color: scheme.outline, width: 1.5),
      borderRadius: BorderRadius.circular(kRowRadius),
    );
    return Material(
      type: MaterialType.transparency,
      shape: shape,
      child: InkWell(
        key: const Key('remote-add-tile'),
        customBorder: shape,
        onTap: () => unawaited(showAddComputer(context)),
        child: SizedBox(
          height: 56,
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                const Icon(AppIcons.add, size: AppIconSize.button),
                const SizedBox(width: 10),
                Text(
                  l10n.remoteAddComputer,
                  style: const TextStyle(
                    fontSize: 14,
                    fontWeight: FontWeight.w500,
                  ),
                ),
                const SizedBox(width: 10),
                Flexible(
                  child: Text(
                    l10n.remoteAddHint,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: 13, color: quietText(scheme)),
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

/// No remembered computers and no session (HostSetup): how to get one.
class RemoteEmptyCard extends StatelessWidget {
  const RemoteEmptyCard({super.key, this.minHeight = 0, this.note});
  final double minHeight;

  /// Said under the card's body, as saved computers that cannot be opened.
  final Widget? note;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    return RaisedSurface(
      radius: kCardRadius,
      child: ConstrainedBox(
        constraints: BoxConstraints(minHeight: minHeight),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 20, 20, 24),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(4, 4, 4, 12),
                child: Text(
                  l10n.remoteListTitle,
                  style: const TextStyle(
                    fontSize: 17,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
              SizedBox(height: math.max(0, (minHeight - 420) / 2)),
              Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: 24,
                  vertical: 32,
                ),
                child: Column(
                  children: [
                    const RemoteEmptyIllustration(),
                    const SizedBox(height: 14),
                    Text(
                      l10n.remoteEmptyTitle,
                      textAlign: TextAlign.center,
                      style: const TextStyle(
                        fontSize: 16,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    const SizedBox(height: 14),
                    ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: 360),
                      child: Text(
                        l10n.remoteEmptyBody,
                        textAlign: TextAlign.center,
                        style: TextStyle(
                          fontSize: 13.5,
                          height: 1.6,
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    ),
                    const SizedBox(height: 20),
                    OutlinedButton.icon(
                      key: const Key('remote-add-empty'),
                      style: OutlinedButton.styleFrom(
                        minimumSize: const Size(0, 40),
                        padding: const EdgeInsets.symmetric(horizontal: 18),
                        side: BorderSide(color: scheme.onSurface),
                        textStyle: buttonLabel(
                          context,
                          14,
                          weight: FontWeight.w500,
                        ),
                      ),
                      onPressed: () => unawaited(showAddComputer(context)),
                      icon: const Icon(AppIcons.add, size: AppIconSize.inline),
                      label: Text(l10n.remoteAddComputer),
                    ),
                  ],
                ),
              ),
              if (note case final note?)
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 4),
                  child: note,
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// A monitor and a phone joined by a dashed blue link (HostSetup:101-107).
class RemoteEmptyIllustration extends StatelessWidget {
  const RemoteEmptyIllustration({super.key});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return CustomPaint(
      size: const Size(120, 84),
      painter: _EmptyPainter(
        stroke: scheme.outline,
        signal: signalColor(scheme),
        fill: raisedCardColor(scheme),
      ),
    );
  }
}

class _EmptyPainter extends CustomPainter {
  _EmptyPainter({
    required this.stroke,
    required this.signal,
    required this.fill,
  });

  final Color stroke, signal, fill;

  @override
  void paint(Canvas canvas, Size size) {
    final line = Paint()
      ..color = stroke
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.5
      ..strokeCap = StrokeCap.round
      ..strokeJoin = StrokeJoin.round;
    const r = Radius.circular(5);
    canvas.drawRRect(
      RRect.fromRectAndRadius(const Rect.fromLTWH(8, 6, 76, 50), r),
      line,
    );
    canvas.drawLine(const Offset(34, 72), const Offset(58, 72), line);
    canvas.drawLine(const Offset(46, 56), const Offset(46, 72), line);
    final phone = RRect.fromRectAndRadius(
      const Rect.fromLTWH(88, 26, 26, 50),
      r,
    );
    canvas.drawRRect(phone, Paint()..color = fill);
    canvas.drawRRect(phone, line);
    canvas.drawLine(const Offset(98, 70), const Offset(104, 70), line);
    final link = Paint()
      ..color = signal
      ..strokeWidth = 1.5
      ..strokeCap = StrokeCap.round;
    for (var x = 60.0; x < 80; x += 7) {
      canvas.drawLine(Offset(x, 31), Offset(math.min(x + 3, 80), 31), link);
    }
  }

  @override
  bool shouldRepaint(_EmptyPainter old) =>
      old.stroke != stroke || old.signal != signal || old.fill != fill;
}
