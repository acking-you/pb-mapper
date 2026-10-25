import 'package:flutter/material.dart';
import 'package:pb_mapper_ui/src/common/app_toast.dart';
import 'package:pb_mapper_ui/src/common/l10n_extension.dart';
import 'package:pb_mapper_ui/src/shell/app_scope.dart';
import 'package:pb_mapper_ui/src/state/app_store.dart';
import 'package:pb_mapper_ui/src/state/tunnel.dart';
import 'package:pb_mapper_ui/src/theme/app_icons.dart';
import 'package:pb_mapper_ui/src/theme/app_theme.dart';
import 'package:pb_mapper_ui/src/theme/fonts.dart';
import 'package:pb_mapper_ui/src/theme/glass.dart';
import 'package:pb_mapper_ui/src/theme/metrics.dart';
import 'package:pb_mapper_ui/src/theme/motion.dart';
import 'package:pb_mapper_ui/src/widgets/surfaces.dart';

/// Open the editor for a new tunnel of [kind] (the user can switch), or for
/// [existing]. [key] prefills the service key — a planet picked in the sky —
/// and [namespace] says whose service it is, when it is another credential's.
Future<void> openTunnelEditor(
  BuildContext context, {
  TunnelKind kind = TunnelKind.share,
  Tunnel? existing,
  String? key,
  int? namespace,
}) {
  final store = AppScope.of(context).store;
  final l10n = context.l10n;
  return showAdaptivePanel<void>(
    context: context,
    builder: (c) => Padding(
      padding: const EdgeInsets.fromLTRB(24, 20, 24, 22),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  existing == null ? l10n.newTunnel : l10n.editTunnel,
                  style: Theme.of(c).textTheme.titleLarge,
                ),
              ),
              IconButton(
                tooltip: l10n.close,
                onPressed: () => Navigator.of(c).pop(),
                icon: const Icon(AppIcons.close, size: 18),
              ),
            ],
          ),
          const SizedBox(height: 14),
          TunnelForm(
            store: store,
            kind: existing?.kind ?? kind,
            kindLocked: existing != null,
            existing: existing,
            initialKey: key,
            initialNamespace: namespace,
            submitLabel: existing == null ? l10n.create : l10n.save,
            onCancel: () => Navigator.of(c).pop(),
            onDone: (draft) {
              Navigator.of(c).pop();
              followTunnel(
                context,
                store,
                draft.kind,
                draft.key,
                updated: existing != null,
              );
            },
          ),
        ],
      ),
    ),
  );
}

/// Say what a tunnel the user just started turned into. Accepting the request
/// is not coming up, so the outcome waits for the retry loop to settle.
Future<void> followTunnel(
  BuildContext context,
  AppStore store,
  TunnelKind kind,
  String key, {
  bool updated = false,
}) async {
  final l10n = context.l10n;
  showToast(
    context,
    updated ? l10n.toastUpdated(key) : l10n.toastStarting(key),
  );
  final settled = await store.settle(kind, key);
  if (settled == null) return;
  // The page that asked may be gone by now; the toast goes to the app's own
  // overlay, which wears the theme either way.
  switch (settled.health) {
    case TunnelHealth.running:
      showToast(null, l10n.toastRunning(key), kind: ToastKind.success);
    case TunnelHealth.retrying:
      showToast(null, l10n.toastRetrying(key), kind: ToastKind.warning);
    case TunnelHealth.failed:
      showToast(
        null,
        l10n.toastFailed(key),
        kind: ToastKind.error,
        description: settled.message.isEmpty ? null : settled.message,
      );
    case TunnelHealth.stopped:
      break;
  }
}

/// The fields of a tunnel, and its create/save action. Used by the editor
/// panel and by first-run setup.
class TunnelForm extends StatefulWidget {
  const TunnelForm({
    super.key,
    required this.store,
    required this.kind,
    required this.submitLabel,
    required this.onDone,
    this.kindLocked = false,
    this.existing,
    this.initialKey,
    this.initialNamespace,
    this.cancelLabel,
    this.onCancel,
  });

  final AppStore store;
  final TunnelKind kind;

  /// Hide the share/connect switch: editing, or a choice already made.
  final bool kindLocked;
  final Tunnel? existing;
  final String? initialKey;

  /// Whose service [initialKey] is, when it is another credential's: only
  /// the administrator key connects to those.
  final int? initialNamespace;
  final String submitLabel;
  final String? cancelLabel;
  final VoidCallback? onCancel;

  /// Called once the Rust side accepted the tunnel.
  final ValueChanged<TunnelDraft> onDone;

  @override
  State<TunnelForm> createState() => _TunnelFormState();
}

class _TunnelFormState extends State<TunnelForm> {
  late TunnelKind _kind = widget.kind;
  late final TextEditingController _key = TextEditingController(
    text: widget.existing?.key ?? widget.initialKey ?? '',
  );
  late final TextEditingController _address = TextEditingController(
    text:
        widget.existing?.localAddress ??
        TunnelDraft.defaultAddress(widget.kind),
  );
  late String _protocol = widget.existing?.protocol ?? 'TCP';
  late bool _encrypted = widget.existing?.encrypted ?? true;
  late bool _keepAlive =
      widget.existing?.keepAlive ?? widget.store.config.keepAliveEnabled;
  late int? _namespace = widget.existing?.namespace ?? widget.initialNamespace;
  late bool _direct = widget.existing?.allowDirect ?? true;
  bool _more = false;
  bool _busy = false;
  String? _keyError;
  String? _addressError;
  String? _error;

  @override
  void initState() {
    super.initState();
    // Whether this replaces a connection of the same name hangs on the key.
    _key.addListener(_onKey);
  }

  void _onKey() => setState(() {});

  @override
  void dispose() {
    _key.removeListener(_onKey);
    _key.dispose();
    _address.dispose();
    super.dispose();
  }

  void _switchKind(TunnelKind next) {
    setState(() {
      // A default nobody touched follows the kind; a typed address stays.
      if (_address.text.trim() == TunnelDraft.defaultAddress(_kind)) {
        _address.text = TunnelDraft.defaultAddress(next);
      }
      _kind = next;
      _keyError = null;
      _error = null;
    });
  }

  /// This computer's connection of the same name in another namespace, which
  /// this one replaces: a connection is known here by its name alone.
  Tunnel? get _replaces {
    if (_kind != TunnelKind.connect || widget.existing != null) return null;
    final store = widget.store;
    final same = store.find(TunnelKind.connect, _key.text.trim());
    if (same == null ||
        store.namespaceOf(same) == (_namespace ?? store.ownNamespace)) {
      return null;
    }
    return same;
  }

  Future<void> _submit() async {
    if (_busy) return;
    final l10n = context.l10n;
    final key = _key.text.trim();
    final address = _address.text.trim();
    final existing = widget.existing;
    final replaces = _replaces;
    String? keyError;
    if (key.isEmpty) {
      keyError = l10n.errorKeyRequired;
    } else if (replaces == null &&
        key != existing?.key &&
        widget.store.find(_kind, key) != null) {
      keyError = _kind == TunnelKind.share
          ? l10n.errorDuplicateShare(key)
          : l10n.errorDuplicateConnect(key);
    }
    final addressError = looksLikeHostPort(address) ? null : l10n.errorAddress;
    setState(() {
      _keyError = keyError;
      _addressError = addressError;
      _error = null;
    });
    if (keyError != null || addressError != null) return;

    final draft = TunnelDraft(
      kind: _kind,
      key: key,
      localAddress: address,
      protocol: _protocol,
      encrypted: _encrypted,
      keepAlive: _keepAlive,
      namespace: _kind == TunnelKind.connect ? _namespace : null,
      allowDirect: _direct,
    );
    setState(() => _busy = true);
    // Replacing goes the way an edit does: the old connection stops first,
    // and comes back if the new one is refused.
    final replacing = existing ?? replaces;
    final result = replacing == null
        ? await widget.store.create(draft)
        : await widget.store.update(replacing, draft);
    if (!mounted) return;
    setState(() => _busy = false);
    if (!result.success) {
      setState(() => _error = result.message);
      return;
    }
    widget.onDone(draft);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final share = _kind == TunnelKind.share;
    final store = widget.store;
    final suggestions = share
        ? const <RemoteService>[]
        : [
            for (final s in store.remoteServices)
              if (store.connectionTo(s) == null) s,
          ];
    final namespace = share ? null : _namespace;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        if (!widget.kindLocked) ...[
          GlassSegmented<TunnelKind>(
            key: const Key('tunnel-kind'),
            selected: _kind,
            onChanged: _switchKind,
            height: 38,
            segments: [
              GlassSegment(
                key: const Key('tunnel-kind-share'),
                value: TunnelKind.share,
                label: l10n.kindShare,
                icon: AppIcons.share,
              ),
              GlassSegment(
                key: const Key('tunnel-kind-connect'),
                value: TunnelKind.connect,
                label: l10n.kindConnect,
                icon: AppIcons.connect,
              ),
            ],
          ),
          const SizedBox(height: 10),
        ],
        AnimatedSwitcher(
          duration: Motion.of(context, Motion.fast),
          child: Text(
            share ? l10n.kindShareHint : l10n.kindConnectHint,
            key: ValueKey(_kind),
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
              fontSize: 12.5,
            ),
          ),
        ),
        const SizedBox(height: 16),
        _Field(
          key: const Key('tunnel-key'),
          controller: _key,
          label: l10n.serviceKey,
          help: share ? l10n.serviceKeyHelpShare : l10n.serviceKeyHelpConnect,
          error: _keyError,
          icon: AppIcons.key,
          monospace: true,
          autofocus: widget.existing == null && widget.initialKey == null,
          onSubmitted: _submit,
        ),
        if (namespace != null) ...[
          const SizedBox(height: 8),
          _NamespaceNote(
            namespace: namespace,
            onClear: widget.existing == null
                ? () => setState(() => _namespace = null)
                : null,
          ),
        ],
        // Read as the key changes: see initState.
        if (_replaces case final replaced?) ...[
          const SizedBox(height: 8),
          Text(
            l10n.tunnelReplaces(remoteLabel(replaced.key, replaced.namespace)),
            key: const Key('tunnel-replaces'),
            style: theme.textTheme.bodySmall?.copyWith(
              color: cautionColor(scheme),
            ),
          ),
        ],
        if (suggestions.isNotEmpty) ...[
          const SizedBox(height: 10),
          _Suggestions(
            label: l10n.suggestionsFromRelay,
            services: suggestions,
            labelOf: (s) => remoteLabel(s.name, store.connectNamespace(s)),
            selected: relayServiceId(
              _namespace ?? store.ownNamespace,
              _key.text.trim(),
            ),
            onPick: (s) => setState(() {
              _key.text = s.name;
              _namespace = store.connectNamespace(s);
              _keyError = null;
            }),
          ),
        ],
        const SizedBox(height: 14),
        _Field(
          key: const Key('tunnel-address'),
          controller: _address,
          label: share ? l10n.localAddressShare : l10n.localAddressConnect,
          help: share
              ? l10n.localAddressHelpShare
              : l10n.localAddressHelpConnect,
          error: _addressError,
          icon: share ? AppIcons.service : AppIcons.computer,
          monospace: true,
          onSubmitted: _submit,
        ),
        const SizedBox(height: 14),
        Row(
          children: [
            Text(
              l10n.protocol,
              style: theme.textTheme.bodyMedium?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
            ),
            const Spacer(),
            GlassSegmented<String>(
              key: const Key('tunnel-protocol'),
              expand: false,
              selected: _protocol,
              onChanged: (p) => setState(() => _protocol = p),
              segments: const [
                GlassSegment(value: 'TCP', label: 'TCP'),
                GlassSegment(value: 'UDP', label: 'UDP'),
              ],
            ),
          ],
        ),
        const SizedBox(height: 8),
        _Disclosure(
          label: l10n.moreOptions,
          open: _more,
          onToggle: () => setState(() => _more = !_more),
        ),
        Motion.reduced(context)
            ? (_more ? _options(context, share) : const SizedBox.shrink())
            : AnimatedSize(
                duration: Motion.medium,
                curve: Motion.move,
                alignment: Alignment.topCenter,
                child: _more
                    ? _options(context, share)
                    : const SizedBox(width: double.infinity),
              ),
        if (_error != null) ...[
          const SizedBox(height: 10),
          Container(
            padding: const EdgeInsets.fromLTRB(12, 10, 12, 10),
            decoration: BoxDecoration(
              color: scheme.errorContainer,
              borderRadius: BorderRadius.circular(kControlRadius),
            ),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Icon(AppIcons.error, size: 16, color: scheme.onErrorContainer),
                const SizedBox(width: 8),
                Expanded(
                  child: SelectableText(
                    _error!,
                    key: const Key('tunnel-error'),
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: scheme.onErrorContainer,
                    ),
                  ),
                ),
              ],
            ),
          ),
        ],
        const SizedBox(height: 20),
        Row(
          mainAxisAlignment: MainAxisAlignment.end,
          children: [
            if (widget.onCancel != null)
              TextButton(
                onPressed: _busy ? null : widget.onCancel,
                child: Text(widget.cancelLabel ?? l10n.cancel),
              ),
            const SizedBox(width: 8),
            FilledButton(
              key: const Key('tunnel-submit'),
              onPressed: _busy ? null : _submit,
              child: _busy
                  ? SizedBox.square(
                      dimension: 18,
                      child: CircularProgressIndicator(
                        strokeWidth: 2,
                        color: scheme.onSurfaceVariant,
                      ),
                    )
                  : Text(widget.submitLabel),
            ),
          ],
        ),
      ],
    );
  }

  Widget _options(BuildContext context, bool share) {
    final l10n = context.l10n;
    return Column(
      children: [
        if (share)
          _SwitchRow(
            key: const Key('tunnel-encryption'),
            title: l10n.encryption,
            subtitle: l10n.encryptionHelp,
            value: _encrypted,
            onChanged: (v) => setState(() => _encrypted = v),
          ),
        _SwitchRow(
          key: const Key('tunnel-direct'),
          title: share ? l10n.directAllow : l10n.directPrefer,
          subtitle: share ? l10n.directAllowHelp : l10n.directPreferHelp,
          value: _direct,
          onChanged: (v) => setState(() => _direct = v),
        ),
        _SwitchRow(
          key: const Key('tunnel-keepalive'),
          title: l10n.keepAlive,
          subtitle: l10n.keepAliveHelp,
          value: _keepAlive,
          onChanged: (v) => setState(() => _keepAlive = v),
        ),
      ],
    );
  }
}

/// A filled field whose purpose is spelled out under it only while it has
/// focus: at rest a form shows names and values, not instructions.
class _Field extends StatefulWidget {
  const _Field({
    super.key,
    required this.controller,
    required this.label,
    required this.help,
    required this.onSubmitted,
    this.error,
    this.icon,
    this.monospace = false,
    this.autofocus = false,
  });

  final TextEditingController controller;
  final String label;
  final String help;
  final String? error;
  final IconData? icon;
  final bool monospace;
  final bool autofocus;
  final VoidCallback onSubmitted;

  @override
  State<_Field> createState() => _FieldState();
}

class _FieldState extends State<_Field> {
  final FocusNode _focus = FocusNode();

  @override
  void initState() {
    super.initState();
    _focus.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _focus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return TextField(
      controller: widget.controller,
      focusNode: _focus,
      autofocus: widget.autofocus,
      autocorrect: false,
      enableSuggestions: false,
      style: widget.monospace ? mono(fontSize: 13.5) : null,
      onSubmitted: (_) => widget.onSubmitted(),
      decoration: InputDecoration(
        labelText: widget.label,
        helperText: _focus.hasFocus ? widget.help : null,
        errorText: widget.error,
        prefixIcon: widget.icon == null ? null : Icon(widget.icon, size: 18),
      ),
    );
  }
}

/// Whose service a connection reaches, when it is another credential's: the
/// administrator key connects into that credential's namespace.
class _NamespaceNote extends StatelessWidget {
  const _NamespaceNote({required this.namespace, this.onClear});

  final int namespace;

  /// Connect in this computer's own namespace instead. Null when editing,
  /// since a connection keeps the service it was made for.
  final VoidCallback? onClear;

  @override
  Widget build(BuildContext context) {
    final l10n = context.l10n;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Row(
      key: const Key('tunnel-namespace'),
      children: [
        Icon(AppIcons.key, size: 15, color: scheme.onSurfaceVariant),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            l10n.tunnelNamespace(namespace),
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ),
        if (onClear != null)
          IconButton(
            key: const Key('tunnel-namespace-clear'),
            tooltip: l10n.tunnelNamespaceClear,
            visualDensity: VisualDensity.compact,
            onPressed: onClear,
            icon: const Icon(AppIcons.close, size: 15),
          ),
      ],
    );
  }
}

/// Services the relay lists that nobody here connects to yet, one tap away.
/// With the administrator key, other credentials' too, named as theirs.
class _Suggestions extends StatelessWidget {
  const _Suggestions({
    required this.label,
    required this.services,
    required this.labelOf,
    required this.selected,
    required this.onPick,
  });

  final String label;
  final List<RemoteService> services;
  final String Function(RemoteService service) labelOf;

  /// The id ([RemoteService.id]) of the one the form holds.
  final String selected;
  final ValueChanged<RemoteService> onPick;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          label,
          style: theme.textTheme.bodySmall?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
        const SizedBox(height: 6),
        Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            for (final s in services.take(12))
              _Pill(
                key: Key('suggestion-${s.id}'),
                label: labelOf(s),
                selected: s.id == selected,
                onTap: () => onPick(s),
              ),
          ],
        ),
      ],
    );
  }
}

class _Pill extends StatelessWidget {
  const _Pill({
    super.key,
    required this.label,
    required this.selected,
    required this.onTap,
  });

  final String label;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: selected ? softWash(scheme, strength: 2) : Colors.transparent,
      shape: StadiumBorder(side: BorderSide(color: scheme.outlineVariant)),
      child: InkWell(
        mouseCursor: clickable,
        customBorder: const StadiumBorder(),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
          child: Text(
            label,
            style: mono(fontSize: 12, color: scheme.onSurface),
          ),
        ),
      ),
    );
  }
}

class _Disclosure extends StatelessWidget {
  const _Disclosure({
    required this.label,
    required this.open,
    required this.onToggle,
  });

  final String label;
  final bool open;
  final VoidCallback onToggle;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Align(
      alignment: AlignmentDirectional.centerStart,
      child: TextButton.icon(
        key: const Key('tunnel-more'),
        onPressed: onToggle,
        style: TextButton.styleFrom(
          foregroundColor: scheme.onSurfaceVariant,
          padding: const EdgeInsets.symmetric(horizontal: 8),
        ),
        icon: AnimatedRotation(
          turns: open ? 0.25 : 0,
          duration: Motion.of(context, Motion.fast),
          child: const Icon(AppIcons.chevronRight, size: 16),
        ),
        label: Text(label, style: const TextStyle(fontSize: 13)),
      ),
    );
  }
}

/// A setting that is on or off, with its explanation beneath.
class _SwitchRow extends StatelessWidget {
  const _SwitchRow({
    super.key,
    required this.title,
    required this.subtitle,
    required this.value,
    required this.onChanged,
  });

  final String title;
  final String subtitle;
  final bool value;
  final ValueChanged<bool> onChanged;

  @override
  Widget build(BuildContext context) {
    return SwitchListTile(
      contentPadding: const EdgeInsets.symmetric(horizontal: 4),
      title: Text(title),
      subtitle: Text(subtitle),
      value: value,
      onChanged: onChanged,
    );
  }
}
