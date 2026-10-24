/// The update, in a dialog: what there is, then the download, the check and
/// the hand-over, each as the controller reaches it.
library;

import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/updates/release.dart';
import 'package:pb_mapper_ui/src/updates/update_controller.dart';
import 'package:url_launcher/url_launcher.dart';

/// Open the dialog. It checks again unless a check already found something
/// to offer, so a background find opens straight onto it.
Future<void> showUpdateDialog(BuildContext context, UpdateController updates) {
  final settled =
      updates.stage == UpdateStage.available ||
      updates.stage == UpdateStage.noPackage;
  if (!updates.busy && !settled) updates.check();
  return showDialog<void>(
    context: context,
    // A download in flight is cancelled with its own button, not by a stray
    // click outside.
    barrierDismissible: false,
    builder: (_) => UpdateDialog(updates: updates),
  );
}

class UpdateDialog extends StatelessWidget {
  const UpdateDialog({super.key, required this.updates});

  final UpdateController updates;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: updates,
      builder: (context, _) => AlertDialog(
        key: const Key('update-dialog'),
        icon: const Icon(AppIcons.update),
        title: Text(context.l10n.checkForUpdates),
        content: ConstrainedBox(
          constraints: const BoxConstraints(minWidth: 320, maxWidth: 440),
          child: _body(context),
        ),
        actions: _actions(context),
      ),
    );
  }

  Widget _body(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: scheme.onSurfaceVariant,
    );
    final installed = updates.installed;
    final release = updates.published;
    final plan = updates.plan;

    Widget progress([double? value]) => Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        LinearProgressIndicator(value: value),
        const SizedBox(height: 12),
      ],
    );

    final children = <Widget>[
      switch (updates.stage) {
        UpdateStage.idle || UpdateStage.checking => Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [progress(), Text(l10n.updateChecking)],
        ),
        UpdateStage.upToDate => _Status(
          icon: AppIcons.ok,
          color: successColor(scheme),
          text: l10n.updateUpToDate('${installed ?? ''}'),
        ),
        UpdateStage.available => Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              l10n.updateAvailableTitle('${release!.version}'),
              key: const Key('update-available'),
              style: theme.textTheme.titleMedium,
            ),
            const SizedBox(height: 4),
            if (installed != null)
              Text(l10n.updateInstalled('$installed'), style: muted),
            const SizedBox(height: 10),
            Text(
              l10n.updateDownloadSize(plan!.asset.name, _size(plan.asset.size)),
              style: mono(fontSize: 12, color: scheme.onSurfaceVariant),
            ),
            const SizedBox(height: 10),
            Text(switch (plan.method) {
              UpdateMethod.installer => l10n.updateNoteInstaller,
              UpdateMethod.replaceAppImage => l10n.updateNoteAppImage,
              UpdateMethod.saveDownload =>
                plan.asset.name.endsWith('.dmg')
                    ? l10n.updateNoteDiskImage
                    : l10n.updateNoteArchive,
              UpdateMethod.openInBrowser => l10n.updateNoteBrowser,
            }),
            const SizedBox(height: 4),
            TextButton.icon(
              style: TextButton.styleFrom(padding: EdgeInsets.zero),
              onPressed: () => _open(release.page),
              icon: const Icon(AppIcons.external, size: 15),
              label: Text(l10n.updateReleaseNotes),
            ),
          ],
        ),
        UpdateStage.noPackage => Text(
          l10n.updateNoPackage('${release?.version ?? ''}'),
        ),
        UpdateStage.downloading => Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            progress(updates.progress),
            Text(l10n.updateDownloading((updates.progress * 100).floor())),
            const SizedBox(height: 4),
            Text(
              '${_size(updates.received)} / ${_size(plan?.asset.size ?? 0)}'
              ' · ${_size(updates.bytesPerSecond.round())}/s',
              style: muted,
            ),
          ],
        ),
        UpdateStage.verifying => Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [progress(), Text(l10n.updateVerifying)],
        ),
        UpdateStage.installing => Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [progress(), Text(l10n.updateInstalling)],
        ),
        UpdateStage.saved => _Status(
          icon: AppIcons.ok,
          color: successColor(scheme),
          text: l10n.updateSaved(updates.savedPath ?? ''),
        ),
        UpdateStage.opened => _Status(
          icon: AppIcons.ok,
          color: successColor(scheme),
          text: l10n.updateOpened,
        ),
        UpdateStage.verifiedOnly => _Status(
          icon: AppIcons.ok,
          color: successColor(scheme),
          text: l10n.updateVerifiedOnly,
        ),
        UpdateStage.failed => _Status(
          icon: AppIcons.error,
          color: scheme.error,
          text: switch (updates.failure?.kind) {
            UpdateFailure.network || null => l10n.updateFailedNetwork,
            UpdateFailure.unsigned => l10n.updateFailedUnsigned,
            UpdateFailure.signature => l10n.updateFailedSignature,
            UpdateFailure.checksum => l10n.updateFailedChecksum,
            UpdateFailure.install => l10n.updateFailedInstall,
          },
        ),
      },
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: children,
    );
  }

  List<Widget> _actions(BuildContext context) {
    final l10n = context.l10n;
    void close() => Navigator.of(context).pop();
    final release = updates.published;
    final closeButton = TextButton(
      key: const Key('update-close'),
      onPressed: close,
      child: Text(l10n.close),
    );
    return switch (updates.stage) {
      UpdateStage.available => [
        TextButton(onPressed: close, child: Text(l10n.cancel)),
        FilledButton.icon(
          key: const Key('update-start'),
          onPressed: updates.start,
          icon: const Icon(AppIcons.download, size: 16),
          label: Text(l10n.updateNow),
        ),
      ],
      UpdateStage.downloading => [
        TextButton(
          key: const Key('update-cancel'),
          onPressed: updates.cancel,
          child: Text(l10n.cancel),
        ),
      ],
      UpdateStage.idle ||
      UpdateStage.checking ||
      UpdateStage.verifying ||
      UpdateStage.installing => const [],
      UpdateStage.noPackage => [
        closeButton,
        if (release != null)
          FilledButton(
            onPressed: () => _open(release.page),
            child: Text(l10n.updateOpenReleasePage),
          ),
      ],
      UpdateStage.failed => [
        if (updates.newer && release != null)
          TextButton(
            onPressed: () => _open(release.page),
            child: Text(l10n.updateOpenReleasePage),
          ),
        closeButton,
        FilledButton(
          key: const Key('update-retry'),
          onPressed: updates.plan != null ? updates.start : updates.check,
          child: Text(l10n.retry),
        ),
      ],
      _ => [closeButton],
    };
  }

  static Future<void> _open(Uri url) =>
      launchUrl(url, mode: LaunchMode.externalApplication);

  static String _size(int bytes) =>
      '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
}

class _Status extends StatelessWidget {
  const _Status({required this.icon, required this.color, required this.text});

  final IconData icon;
  final Color color;
  final String text;

  @override
  Widget build(BuildContext context) => Row(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Icon(icon, color: color, size: 20),
      const SizedBox(width: 12),
      Expanded(child: SelectableText(text)),
    ],
  );
}
