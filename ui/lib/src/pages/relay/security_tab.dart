import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/setup_string.dart';
import 'package:pb_mapper_ui/src/pages/relay/relay_format.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// The relay's security state and the administrator's levers on it: how its
/// credentials stand, whether old clients may still connect, the
/// administrator key itself, and a reset of every temporary credential.
class SecurityTab extends StatelessWidget {
  const SecurityTab({super.key});

  @override
  Widget build(BuildContext context) => RelayListener(builder: _build);

  Widget _build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final relay = AppScope.of(context).relay;
    final auth = relay.authStatus;
    if (auth == null) {
      return EmptyNote(
        icon: AppIcons.lock,
        text: relay.adminError ?? l10n.relayChecking,
      );
    }
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: scheme.onSurfaceVariant,
    );
    final stats = [
      (l10n.credentialStateActive, auth.activeKeys),
      (l10n.credentialStateExpired, auth.expiredKeys),
      (l10n.credentialStateRevoked, auth.revokedKeys),
      (l10n.securityCapacity, auth.capacity),
      (l10n.securitySignIns, auth.authSuccesses),
      (l10n.securityRejected, auth.authFailures),
    ];
    final lastLegacy = auth.lastLegacyConnectionAt;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SizedBox(height: 14),
        GroupCard(
          title: l10n.securityCredentials,
          icon: AppIcons.key,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              if (auth.safeMode) ...[
                Container(
                  key: const Key('security-safe-mode'),
                  padding: const EdgeInsets.all(12),
                  decoration: BoxDecoration(
                    color: scheme.errorContainer,
                    borderRadius: BorderRadius.circular(10),
                  ),
                  child: Text(
                    l10n.securitySafeMode,
                    style: TextStyle(color: scheme.onErrorContainer),
                  ),
                ),
                const SizedBox(height: 12),
              ],
              Wrap(
                spacing: 10,
                runSpacing: 10,
                children: [
                  for (final (label, value) in stats)
                    _Figure(label: label, value: '$value'),
                ],
              ),
              const SizedBox(height: 12),
              SelectableText(
                l10n.securityInstance(auth.serverInstanceId),
                style: mono(fontSize: 12, color: scheme.onSurfaceVariant),
              ),
            ],
          ),
        ),
        const SizedBox(height: 16),
        GroupCard(
          title: l10n.securityLegacyTitle,
          icon: AppIcons.lock,
          child: SwitchListTile(
            key: const Key('security-legacy'),
            contentPadding: const EdgeInsets.symmetric(horizontal: 4),
            title: Text(l10n.securityLegacySwitch),
            subtitle: Text(
              l10n.securityLegacyBody(
                auth.activeLegacyConnections,
                lastLegacy == null
                    ? l10n.securityLegacyNever
                    : formatAgo(
                        context,
                        DateTime.now().difference(fromUnix(lastLegacy)),
                      ),
              ),
            ),
            value: auth.legacyAllowed,
            onChanged: (allow) async {
              final done = await attempt(
                context,
                () => relay.setLegacyProtocol(allow: allow).then((_) => true),
              );
              if (done == true && context.mounted) {
                showToast(
                  context,
                  allow
                      ? l10n.securityLegacyAllowed
                      : l10n.securityLegacyDenied,
                  kind: ToastKind.success,
                );
              }
            },
          ),
        ),
        const SizedBox(height: 16),
        GroupCard(
          title: l10n.securityRotateTitle,
          icon: AppIcons.key,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(l10n.securityRotateBody, style: muted),
              const SizedBox(height: 12),
              OutlinedButton.icon(
                key: const Key('security-rotate'),
                onPressed: () => _rotate(context, relay),
                icon: const Icon(AppIcons.refresh, size: 16),
                label: Text(l10n.securityRotate),
              ),
            ],
          ),
        ),
        const SizedBox(height: 16),
        GroupCard(
          title: l10n.securityResetTitle,
          icon: AppIcons.warning,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(l10n.securityResetBody, style: muted),
              const SizedBox(height: 12),
              OutlinedButton.icon(
                key: const Key('security-reset'),
                style: OutlinedButton.styleFrom(
                  foregroundColor: scheme.error,
                  side: BorderSide(color: scheme.error.withValues(alpha: 0.5)),
                ),
                onPressed: () => _reset(context, relay),
                icon: const Icon(AppIcons.delete, size: 16),
                label: Text(l10n.securityReset),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Future<void> _rotate(BuildContext context, RelayStore relay) async {
    final l10n = context.l10n;
    final yes = await confirmDestructive(
      context,
      title: l10n.securityRotateConfirmTitle,
      body: l10n.securityRotateConfirmBody,
      action: l10n.securityRotateAction,
    );
    if (!yes || !context.mounted) return;
    // The relay rotated, which Settings may have moved away from meanwhile.
    final address = relay.app.relayAddress;
    final rotation = await attempt(context, relay.rotateKey);
    if (rotation == null || !context.mounted) return;
    final key = rotation.key;
    final confirmed = rotation.confirmed;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final setup = encodeSetupString(
      SetupPayload(address: address, credential: key),
    );
    // Unless this computer saved it, this may be the only key the relay
    // takes now, and this dialog the only place it is written down.
    await showDialog<void>(
      context: context,
      barrierDismissible: rotation.saved,
      builder: (c) => AlertDialog(
        key: Key(confirmed ? 'security-rotated' : 'security-rotation-unknown'),
        icon: confirmed
            ? Icon(AppIcons.ok, color: successColor(scheme))
            : Icon(AppIcons.warning, color: cautionColor(scheme)),
        title: Text(
          confirmed
              ? l10n.securityRotatedTitle
              : l10n.securityRotationUnknownTitle,
        ),
        content: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 460),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Container(
                padding: const EdgeInsets.all(12),
                decoration: BoxDecoration(
                  color: codeSurface(scheme),
                  borderRadius: BorderRadius.circular(10),
                ),
                child: SelectableText(
                  key,
                  style: mono(fontSize: 13, color: scheme.onSurface),
                ),
              ),
              const SizedBox(height: 12),
              Text(switch ((confirmed, rotation.saved)) {
                (true, true) => l10n.securityRotatedBody,
                (true, false) => l10n.securityRotatedUnsavedBody(address),
                (false, _) => l10n.securityRotationUnknownBody(
                  rotation.reason ?? l10n.relayNoReason,
                ),
              }, style: theme.textTheme.bodySmall),
            ],
          ),
        ),
        actions: [
          TextButton.icon(
            onPressed: () => copySecret(c, key, l10n.credentialCopied),
            icon: const Icon(AppIcons.copy, size: 16),
            label: Text(l10n.credentialCopy),
          ),
          if (confirmed)
            FilledButton.icon(
              onPressed: () => copySecret(c, setup, l10n.credentialSetupCopied),
              icon: const Icon(AppIcons.export, size: 16),
              label: Text(l10n.credentialCopySetup),
            ),
          TextButton(
            onPressed: () => Navigator.of(c).pop(),
            child: Text(l10n.close),
          ),
        ],
      ),
    );
  }

  Future<void> _reset(BuildContext context, RelayStore relay) async {
    final l10n = context.l10n;
    final count = relay.keys.length;
    final yes = await showDialog<bool>(
      context: context,
      builder: (_) => _ResetDialog(count: count),
    );
    if (yes != true || !context.mounted) return;
    final done = await attempt(
      context,
      () => relay.resetCredentials().then((_) => true),
    );
    if (done == true && context.mounted) {
      showToast(context, l10n.securityResetDone, kind: ToastKind.success);
    }
  }
}

/// The reset cannot be undone, so it takes a second, deliberate yes.
class _ResetDialog extends StatefulWidget {
  const _ResetDialog({required this.count});

  final int count;

  @override
  State<_ResetDialog> createState() => _ResetDialogState();
}

class _ResetDialogState extends State<_ResetDialog> {
  bool _understood = false;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    return AlertDialog(
      key: const Key('security-reset-dialog'),
      icon: Icon(AppIcons.warning, color: scheme.error),
      title: Text(l10n.securityResetConfirmTitle),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 440),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(l10n.securityResetConfirmBody(widget.count)),
            const SizedBox(height: 8),
            CheckboxListTile(
              key: const Key('security-reset-understood'),
              contentPadding: EdgeInsets.zero,
              controlAffinity: ListTileControlAffinity.leading,
              value: _understood,
              onChanged: (value) =>
                  setState(() => _understood = value ?? false),
              title: Text(l10n.securityResetUnderstood),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: Text(l10n.cancel),
        ),
        FilledButton(
          key: const Key('security-reset-confirm'),
          style: FilledButton.styleFrom(
            backgroundColor: scheme.error,
            foregroundColor: scheme.onError,
          ),
          onPressed: _understood ? () => Navigator.of(context).pop(true) : null,
          child: Text(l10n.securityResetAction),
        ),
      ],
    );
  }
}

class _Figure extends StatelessWidget {
  const _Figure({required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      constraints: const BoxConstraints(minWidth: 100),
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 8),
      decoration: BoxDecoration(
        color: softWash(scheme),
        borderRadius: BorderRadius.circular(10),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            value,
            style: theme.textTheme.titleMedium?.copyWith(
              fontWeight: FontWeight.w600,
            ),
          ),
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
