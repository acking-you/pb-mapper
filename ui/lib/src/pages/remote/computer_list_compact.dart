import 'dart:async';

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/pages/remote/add_computer.dart';
import 'package:pb_mapper_ui/src/pages/remote/computer_list.dart';
import 'package:pb_mapper_ui/src/pages/remote/invite_scanner.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_actions.dart';
import 'package:pb_mapper_ui/src/pages/remote/remote_format.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/remote_computers.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';
import 'package:pb_mapper_ui/src/theme/viewer_palette.dart';
import 'package:pb_mapper_ui/src/widgets/dashed_border.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// Opens [entry] from a phone list: Back to picture while viewed, a toast
/// for a computer that cannot be opened now, else the computer.
Future<void> openEntry(BuildContext context, RemoteEntry entry) async {
  final l10n = context.l10n;
  final scope = AppScope.of(context);
  final c = entry.computer;
  switch (entry.presence) {
    case ComputerPresence.viewing:
      scope.remote.viewer.open();
    case ComputerPresence.notSharing:
      showToast(context, l10n.remoteOfflineToast(entry.name));
    case ComputerPresence.otherRelay:
      showToast(context, l10n.remoteOtherRelayHint);
    case ComputerPresence.online || ComputerPresence.unknown:
      if (c != null) await openComputer(context, c);
  }
}

/// The remote computers on a phone (MobileHome): the computer being viewed,
/// or the last one opened, as a card with its last picture; the others as
/// rows; then the ways to add one.
class CompactComputerList extends StatelessWidget {
  const CompactComputerList({super.key});

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final store = AppScope.of(context).store;
    return ListenableBuilder(
      listenable: Listenable.merge([
        store.computers,
        store,
        store.desktop.session,
      ]),
      builder: (context, _) {
        final (:transient, :open, :closed) = remoteEntries(store);
        final all = [?transient, ...open, ...closed];
        // The viewed one leads; else the last one opened, which the store
        // sorts first.
        final featured =
            all.where((e) => e.viewing).firstOrNull ?? all.firstOrNull;
        final others = [
          for (final e in all)
            if (!identical(e, featured)) e,
        ];
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (featured != null) ...[
              FeaturedComputerCard(entry: featured),
              const SizedBox(height: 12),
            ],
            if (others.isNotEmpty) ...[
              Padding(
                padding: const EdgeInsets.fromLTRB(4, 6, 4, 8),
                child: Text(
                  l10n.remoteOthers,
                  style: TextStyle(
                    fontSize: 13,
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ),
              RaisedSurface(
                radius: kCardRadius,
                child: Padding(
                  padding: const EdgeInsets.symmetric(vertical: 4),
                  child: Column(
                    children: [
                      for (final (i, e) in others.indexed) ...[
                        if (i > 0)
                          Divider(
                            height: 1,
                            indent: 68,
                            color: scheme.outlineVariant,
                          ),
                        CompactComputerRow(entry: e),
                      ],
                    ],
                  ),
                ),
              ),
              const SizedBox(height: 12),
            ],
            const AddComputerButtons(),
            if (cannotRemember(store.computers.support)) ...[
              const SizedBox(height: 10),
              Text(
                l10n.addRememberUnavailable,
                style: TextStyle(fontSize: 12, color: quietText(scheme)),
              ),
            ],
          ],
        );
      },
    );
  }
}

/// The computer that leads the phone list: its last picture, how it was
/// last reached, and one wide Open or Back to picture.
class FeaturedComputerCard extends StatelessWidget {
  const FeaturedComputerCard({super.key, required this.entry});
  final RemoteEntry entry;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final c = entry.computer;
    final path = c?.lastPath;
    final pill = presencePill(context, entry.presence);
    return RaisedSurface(
      key: const Key('computer-featured'),
      radius: kCardRadius,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          AspectRatio(
            aspectRatio: 16 / 9,
            child: _Thumbnail(computer: c),
          ),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 14, 16, 16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Row(
                  children: [
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            entry.name,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: const TextStyle(
                              fontSize: 17,
                              fontWeight: FontWeight.w600,
                            ),
                          ),
                          if (path != null) ...[
                            const SizedBox(height: 3),
                            Text(
                              c?.lastControlAllowed == false
                                  ? l10n.remotePathViewOnly(
                                      pathLabel(context, path),
                                    )
                                  : l10n.remotePathPermission(
                                      pathLabel(context, path),
                                    ),
                              style: TextStyle(
                                fontSize: 12.5,
                                color: scheme.onSurfaceVariant,
                              ),
                            ),
                          ],
                        ],
                      ),
                    ),
                    if (pill != null) ...[const SizedBox(width: 10), pill],
                  ],
                ),
                const SizedBox(height: 12),
                FilledButton(
                  key: const Key('computer-featured-open'),
                  style: FilledButton.styleFrom(
                    minimumSize: const Size.fromHeight(46),
                    textStyle: buttonLabel(
                      context,
                      15,
                      weight: FontWeight.w600,
                    ),
                  ),
                  onPressed: () => unawaited(openEntry(context, entry)),
                  child: Text(
                    entry.viewing ? l10n.remoteBackToPicture : l10n.remoteOpen,
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

/// The last picture a computer showed (D23), with when; a placeholder when
/// there is none.
class _Thumbnail extends StatelessWidget {
  const _Thumbnail({required this.computer});
  final RemoteComputer? computer;

  @override
  Widget build(BuildContext context) {
    // A dark plate in both themes, as a screen is: its icon takes the
    // viewer's quiet text colour, which the theme's would not show on it.
    const placeholder = ColoredBox(
      color: Color(0xFF1F2328),
      child: Center(
        child: Icon(
          AppIcons.remoteLarge,
          size: AppIconSize.tile,
          color: ViewerPalette.textSecondary,
        ),
      ),
    );
    final c = computer;
    final computers = AppScope.of(context).store.computers;
    final file = c == null ? null : computers.thumbnailFile(c.id);
    final at = c?.lastConnectedAt;
    if (c == null || file == null) return placeholder;
    return LayoutBuilder(
      builder: (context, box) => Image.file(
        file,
        key: ValueKey(computers.thumbnailVersion(c.id)),
        fit: BoxFit.cover,
        cacheWidth: (box.maxWidth * MediaQuery.devicePixelRatioOf(context))
            .round(),
        gaplessPlayback: true,
        errorBuilder: (_, _, _) => placeholder,
        frameBuilder: (context, child, frame, _) => frame == null
            ? placeholder
            : Stack(
                fit: StackFit.expand,
                children: [
                  child,
                  if (at != null)
                    Positioned(
                      left: 10,
                      top: 10,
                      child: Container(
                        height: 24,
                        padding: const EdgeInsets.symmetric(horizontal: 10),
                        alignment: Alignment.center,
                        decoration: BoxDecoration(
                          color: const Color(0xB3141416),
                          borderRadius: BorderRadius.circular(12),
                        ),
                        child: Text(
                          context.l10n.remoteLastPicture(
                            formatLastSeen(context, at, DateTime.now()),
                          ),
                          style: const TextStyle(
                            fontSize: 11.5,
                            color: Colors.white,
                          ),
                        ),
                      ),
                    ),
                ],
              ),
      ),
    );
  }
}

/// One computer on the phone list: tap opens it, a long press offers
/// rename and remove.
class CompactComputerRow extends StatelessWidget {
  const CompactComputerRow({super.key, required this.entry});
  final RemoteEntry entry;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return InkWell(
      key: Key('computer-${entry.id}'),
      onTap: () => unawaited(openEntry(context, entry)),
      onLongPress: () => unawaited(_menu(context)),
      child: SizedBox(
        height: 64,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16),
          child: Row(
            children: [
              ComputerTile(presence: entry.presence),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  mainAxisAlignment: MainAxisAlignment.center,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      entry.name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 15,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    PresenceLine(entry: entry),
                  ],
                ),
              ),
              Icon(
                AppIcons.chevronRight,
                size: AppIconSize.button,
                color: onSurfaceMuted(scheme),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _menu(BuildContext context) => showModalBottomSheet<void>(
    context: context,
    useSafeArea: true,
    builder: (sheet) => SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 0, 12, 12),
        child: Material(
          type: MaterialType.transparency,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              for (final a in computerActions(context, entry))
                ListTile(
                  key: a.key,
                  leading: Icon(
                    a.icon,
                    color: a.destructive
                        ? Theme.of(sheet).colorScheme.error
                        : null,
                  ),
                  title: Text(
                    a.label,
                    style: a.destructive
                        ? TextStyle(color: Theme.of(sheet).colorScheme.error)
                        : null,
                  ),
                  enabled: a.onPressed != null,
                  onTap: switch (a.onPressed) {
                    final onPressed? => () {
                      Navigator.of(sheet).pop();
                      onPressed();
                    },
                    null => null,
                  },
                ),
            ],
          ),
        ),
      ),
    ),
  );
}

/// The ways to add a computer: scan and paste side by side where there is a
/// camera, else the dashed tile.
class AddComputerButtons extends StatelessWidget {
  const AddComputerButtons({super.key});

  @override
  Widget build(BuildContext context) {
    if (!InviteScanner.supported) return const AddComputerTile();
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    Widget button(Key key, IconData icon, String label, AddMode mode) {
      final shape = DashedBorder(
        side: BorderSide(color: scheme.outline, width: 1.5),
        borderRadius: BorderRadius.circular(16),
      );
      return Expanded(
        child: Material(
          type: MaterialType.transparency,
          shape: shape,
          child: InkWell(
            key: key,
            customBorder: shape,
            onTap: () => unawaited(showAddComputer(context, mode: mode)),
            child: SizedBox(
              height: 52,
              child: Row(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  Icon(icon, size: AppIconSize.button),
                  const SizedBox(width: 8),
                  Flexible(
                    child: Text(
                      label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 14,
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      );
    }

    return Row(
      children: [
        button(
          const Key('remote-scan'),
          AppIcons.scanQrCode,
          l10n.remoteScanAdd,
          AddMode.scan,
        ),
        const SizedBox(width: 10),
        button(
          const Key('remote-paste'),
          AppIcons.paste,
          l10n.remotePasteInvite,
          AddMode.paste,
        ),
      ],
    );
  }
}
