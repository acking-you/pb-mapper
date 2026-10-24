import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/common/setup_string.dart';
import 'package:pb_mapper_ui/src/ffi/pb_mapper_api.dart';
import 'package:pb_mapper_ui/src/pages/relay/relay_format.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/relay_store.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/widgets/status.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// The relay's temporary credentials: one per computer or person that should
/// use the relay without its administrator key, each confined to its own
/// services and with a lifetime. Issue, hand over, renew, revoke.
class CredentialsTab extends StatefulWidget {
  const CredentialsTab({super.key});

  @override
  State<CredentialsTab> createState() => _CredentialsTabState();
}

class _CredentialsTabState extends State<CredentialsTab> {
  bool _all = false;

  @override
  Widget build(BuildContext context) => RelayListener(builder: _build);

  Widget _build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scope = AppScope.of(context);
    final relay = scope.relay;
    final auth = relay.authStatus;
    final keys = [...relay.keys]
      ..sort((a, b) => b.issuedAt.compareTo(a.issuedAt));
    final shown = [
      for (final key in keys)
        if (_all || standingOf(key) == RelayKeyState.active) key,
    ];
    final spent = keys.where((k) => k.state != RelayKeyState.active).length;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // Issuing is the page's primary action, in the title bar.
        SectionHeader(title: l10n.relayTabCredentials, count: keys.length),
        if (auth != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(4, 0, 4, 10),
            child: Text(
              l10n.credentialsSummary(
                auth.activeKeys,
                auth.expiredKeys,
                auth.revokedKeys,
                auth.capacity,
              ),
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ),
        Row(
          children: [
            GlassSegmented<bool>(
              key: const Key('credential-filter'),
              expand: false,
              selected: _all,
              onChanged: (value) => setState(() => _all = value),
              segments: [
                GlassSegment(value: false, label: l10n.credentialsFilterActive),
                GlassSegment(
                  key: const Key('credential-filter-all'),
                  value: true,
                  label: l10n.credentialsFilterAll,
                ),
              ],
            ),
            const Spacer(),
            if (spent > 0)
              TextButton.icon(
                key: const Key('credential-clean-up'),
                onPressed: () async {
                  final removed = await attempt(context, relay.cleanUpKeys);
                  if (removed != null && context.mounted) {
                    showToast(
                      context,
                      l10n.credentialCleanedUp(removed),
                      kind: ToastKind.success,
                    );
                  }
                },
                icon: const Icon(AppIcons.clear, size: 16),
                label: Text(l10n.credentialCleanUp),
              ),
          ],
        ),
        const SizedBox(height: 10),
        if (shown.isEmpty)
          EmptyNote(icon: AppIcons.key, text: l10n.credentialsEmpty)
        else
          for (final key in shown)
            _CredentialRow(credential: key, relay: relay, store: scope.store),
      ],
    );
  }
}

class _CredentialRow extends StatelessWidget {
  const _CredentialRow({
    required this.credential,
    required this.relay,
    required this.store,
  });

  final RelayKey credential;
  final RelayStore relay;
  final AppStore store;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final key = credential;
    final standing = standingOf(key);
    final color = credentialColor(scheme, standing);
    final facts = [
      '#${key.keyId}',
      credentialStanding(context, key),
      l10n.credentialIssuedOn(formatDate(key.issuedAt)),
    ].join('  ·  ');
    final usable = standing == RelayKeyState.active;
    return ListCard(
      padding: const EdgeInsets.fromLTRB(14, 10, 4, 10),
      child: Row(
        key: Key('credential-${key.keyId}'),
        children: [
          Container(
            width: 34,
            height: 34,
            decoration: BoxDecoration(
              color: color.withValues(alpha: 0.14),
              borderRadius: BorderRadius.circular(10),
            ),
            child: Icon(AppIcons.key, size: 17, color: color),
          ),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  credentialName(context, key),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.titleSmall,
                ),
                const SizedBox(height: 2),
                Text(
                  facts,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(width: 8),
          StatusChip(
            color: color,
            label: credentialStateLabel(context, standing),
          ),
          if (usable)
            IconButton(
              key: Key('credential-setup-${key.keyId}'),
              tooltip: l10n.credentialCopySetup,
              onPressed: () async {
                final issued = await attempt(
                  context,
                  () => relay.revealKey(key.keyId),
                );
                if (issued == null || !context.mounted) return;
                await copySecret(
                  context,
                  encodeSetupString(
                    SetupPayload(
                      address: store.relayAddress,
                      credential: issued.credential,
                    ),
                  ),
                  l10n.credentialSetupCopied,
                );
              },
              icon: const Icon(AppIcons.export, size: 17),
            ),
          _CredentialMenu(credential: key, relay: relay, store: store),
        ],
      ),
    );
  }
}

enum _CredentialAction { reveal, renew, revoke }

class _CredentialMenu extends StatelessWidget {
  const _CredentialMenu({
    required this.credential,
    required this.relay,
    required this.store,
  });

  final RelayKey credential;
  final RelayStore relay;
  final AppStore store;

  Future<void> _reveal(BuildContext context) async {
    final issued = await attempt(
      context,
      () => relay.revealKey(credential.keyId),
    );
    if (issued == null || !context.mounted) return;
    await showCredentialDialog(context, issued, store, fresh: false);
  }

  Future<void> _renew(BuildContext context) async {
    final l10n = context.l10n;
    final ttl = await showDialog<Duration>(
      context: context,
      builder: (c) => SimpleDialog(
        title: Text(l10n.credentialRenewTitle(credentialName(c, credential))),
        children: [
          for (final ttl in credentialLifetimes)
            SimpleDialogOption(
              key: Key('credential-renew-${ttl.inHours}'),
              onPressed: () => Navigator.of(c).pop(ttl),
              child: Text(l10n.credentialRenewFor(lifetimeLabel(c, ttl))),
            ),
        ],
      ),
    );
    if (ttl == null || !context.mounted) return;
    final renewed = await attempt(
      context,
      () => relay.renewKey(credential.keyId, ttl),
    );
    if (renewed != null && context.mounted) {
      showToast(
        context,
        l10n.credentialRenewed(
          credentialName(context, renewed),
          credentialStanding(context, renewed),
        ),
        kind: ToastKind.success,
      );
    }
  }

  Future<void> _revoke(BuildContext context) async {
    final l10n = context.l10n;
    final name = credentialName(context, credential);
    final yes = await confirmDestructive(
      context,
      title: l10n.credentialRevokeTitle(name),
      body: l10n.credentialRevokeBody,
      action: l10n.credentialRevoke,
    );
    if (!yes || !context.mounted) return;
    final revoked = await attempt(
      context,
      () => relay.revokeKey(credential.keyId),
    );
    if (revoked != null && context.mounted) {
      showToast(context, l10n.credentialRevoked(name), kind: ToastKind.success);
    }
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final scheme = Theme.of(context).colorScheme;
    final active = credential.state == RelayKeyState.active;
    PopupMenuItem<_CredentialAction> item(
      _CredentialAction value,
      IconData icon,
      String label, {
      bool danger = false,
    }) => PopupMenuItem(
      key: Key('credential-${value.name}-${credential.keyId}'),
      value: value,
      child: Row(
        children: [
          Icon(icon, size: 17, color: danger ? scheme.error : null),
          const SizedBox(width: 10),
          Text(label, style: danger ? TextStyle(color: scheme.error) : null),
        ],
      ),
    );
    return PopupMenuButton<_CredentialAction>(
      key: Key('credential-menu-${credential.keyId}'),
      tooltip: l10n.more,
      icon: const Icon(AppIcons.more, size: 18),
      onSelected: (action) => switch (action) {
        _CredentialAction.reveal => _reveal(context),
        _CredentialAction.renew => _renew(context),
        _CredentialAction.revoke => _revoke(context),
      },
      itemBuilder: (context) => [
        if (active)
          item(_CredentialAction.reveal, AppIcons.show, l10n.credentialReveal),
        if (credential.state != RelayKeyState.revoked)
          item(_CredentialAction.renew, AppIcons.refresh, l10n.credentialRenew),
        if (credential.state != RelayKeyState.revoked)
          item(
            _CredentialAction.revoke,
            AppIcons.delete,
            l10n.credentialRevoke,
            danger: true,
          ),
      ],
    );
  }
}

/// Ask for a name and a lifetime, issue the credential, and show it with its
/// setup string.
Future<void> issueCredential(
  BuildContext context,
  RelayStore relay,
  AppStore store,
) async {
  final issued = await showDialog<IssuedCredential>(
    context: context,
    builder: (_) => _IssueDialog(relay: relay),
  );
  if (issued == null || !context.mounted) return;
  await showCredentialDialog(context, issued, store, fresh: true);
}

/// Issues the credential itself, so that when the relay refuses — a lifetime
/// longer than it allows, a full table — the form is still there to change.
class _IssueDialog extends StatefulWidget {
  const _IssueDialog({required this.relay});

  final RelayStore relay;

  @override
  State<_IssueDialog> createState() => _IssueDialogState();
}

class _IssueDialogState extends State<_IssueDialog> {
  final TextEditingController _label = TextEditingController();
  Duration _ttl = const Duration(days: 7);
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _label.dispose();
    super.dispose();
  }

  Future<void> _submit() async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    final label = _label.text.trim();
    try {
      final issued = await widget.relay.issueKey(
        ttl: _ttl,
        label: label.isEmpty ? null : label,
      );
      if (mounted) Navigator.of(context).pop(issued);
    } on RelayCallException catch (error) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = error.message;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    return AlertDialog(
      key: const Key('credential-issue-dialog'),
      icon: const Icon(AppIcons.key),
      title: Text(l10n.credentialIssueTitle),
      // A set width: AlertDialog sizes its content by asking for its intrinsic
      // width, which the segmented control's LayoutBuilder cannot answer. The
      // dialog's own constraints still narrow it on a phone.
      content: SizedBox(
        width: 440,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(
              l10n.credentialIssueBody,
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
            const SizedBox(height: 16),
            TextField(
              key: const Key('credential-label'),
              controller: _label,
              autofocus: true,
              maxLength: 64,
              onSubmitted: (_) => _submit(),
              decoration: InputDecoration(
                labelText: l10n.credentialLabel,
                hintText: l10n.credentialLabelHint,
              ),
            ),
            const SizedBox(height: 8),
            Text(l10n.credentialLifetime, style: theme.textTheme.labelLarge),
            const SizedBox(height: 8),
            SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              child: GlassSegmented<Duration>(
                key: const Key('credential-ttl'),
                expand: false,
                selected: _ttl,
                onChanged: (value) => setState(() => _ttl = value),
                segments: [
                  for (final ttl in credentialLifetimes)
                    GlassSegment(
                      key: Key('credential-ttl-${ttl.inHours}'),
                      value: ttl,
                      label: lifetimeLabel(context, ttl),
                    ),
                ],
              ),
            ),
            if (_error != null) ...[
              const SizedBox(height: 12),
              Text(
                _error!,
                key: const Key('credential-issue-error'),
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.error,
                ),
              ),
            ],
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(l10n.cancel),
        ),
        FilledButton(
          key: const Key('credential-issue-submit'),
          onPressed: _busy ? null : _submit,
          child: Text(l10n.credentialIssue),
        ),
      ],
    );
  }
}

/// A credential with its secret: to copy as it is, or as a setup string the
/// other computer pastes into Settings.
Future<void> showCredentialDialog(
  BuildContext context,
  IssuedCredential issued,
  AppStore store, {
  required bool fresh,
}) {
  final l10n = context.l10n;
  final theme = Theme.of(context);
  final scheme = theme.colorScheme;
  final name = credentialName(context, issued);
  final setup = encodeSetupString(
    SetupPayload(address: store.relayAddress, credential: issued.credential),
  );
  return showDialog<void>(
    context: context,
    builder: (c) => AlertDialog(
      key: const Key('credential-dialog'),
      icon: Icon(
        fresh ? AppIcons.ok : AppIcons.key,
        color: fresh ? successColor(scheme) : null,
      ),
      title: Text(fresh ? l10n.credentialReadyTitle(name) : name),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(
              '#${issued.keyId}  ·  ${credentialStanding(c, issued)}',
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
            ),
            const SizedBox(height: 12),
            Container(
              padding: const EdgeInsets.all(12),
              decoration: BoxDecoration(
                color: codeSurface(scheme),
                borderRadius: BorderRadius.circular(10),
              ),
              child: SelectableText(
                issued.credential,
                key: const Key('credential-secret'),
                style: mono(fontSize: 12.5, color: scheme.onSurface),
              ),
            ),
            const SizedBox(height: 12),
            Text(l10n.credentialReadyBody, style: theme.textTheme.bodySmall),
          ],
        ),
      ),
      actions: [
        TextButton.icon(
          key: const Key('credential-copy'),
          onPressed: () =>
              copySecret(c, issued.credential, l10n.credentialCopied),
          icon: const Icon(AppIcons.copy, size: 16),
          label: Text(l10n.credentialCopy),
        ),
        FilledButton.icon(
          key: const Key('credential-copy-setup'),
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
